//! Private key encryption using AES-256-GCM.
//!
//! The encryption key is derived from a machine-specific value (hostname + username)
//! so the database cannot be simply copied to another machine and decrypted.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose, Engine as _};
use std::env;
use std::io;

/// Tag prefix to distinguish encrypted values from plaintext (legacy).
const ENCRYPTED_PREFIX: &str = "ENC:";

/// Derive a 256-bit key from machine identity.
///
/// Uses hostname + OS username, hashed with SHA-256.
/// This is not high-security key management, but prevents trivial DB theft.
fn derive_machine_key() -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let hostname = hostname_string().unwrap_or_default();
    let username = env::var("USERNAME")
        .or_else(|_| env::var("USER"))
        .unwrap_or_default();

    let mut hasher = Sha256::new();
    hasher.update(b"woolbrush-v2::");
    hasher.update(hostname.as_bytes());
    hasher.update(b"::");
    hasher.update(username.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

#[cfg(windows)]
fn hostname_string() -> io::Result<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    // GetComputerNameW
    extern "system" {
        fn GetComputerNameW(lpBuffer: *mut u16, nSize: *mut u32) -> i32;
    }

    let mut size: u32 = 0;
    // First call to get required size
    unsafe {
        GetComputerNameW(std::ptr::null_mut(), &mut size);
    }
    let mut buf = vec![0u16; size as usize + 1];
    let result = unsafe { GetComputerNameW(buf.as_mut_ptr(), &mut size) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    let len = size as usize;
    let os_str = OsString::from_wide(&buf[..len]);
    Ok(os_str.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn hostname_string() -> io::Result<String> {
    // Fallback: read /etc/hostname
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string())
}

/// Encrypt a plaintext string.
///
/// Returns a Base64-encoded string prefixed with `ENC:`.
/// If encryption fails (e.g. key derivation issue), returns the plaintext
/// so the app remains functional (caller should log a warning).
pub fn encrypt(plaintext: &str) -> Result<String> {
    let key = derive_machine_key();
    let key = Key::<Aes256Gcm>::from_slice(&key);
    let cipher = Aes256Gcm::new(key);

    // Generate a random 12-byte nonce
    let nonce_bytes = uuid::Uuid::new_v4();
    let nonce = Nonce::from_slice(&nonce_bytes.as_bytes()[..12]);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("AES-GCM encrypt failed: {}", e))?;

    // Prepend nonce (12 bytes) to ciphertext, then Base64 encode
    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(nonce);
    combined.extend_from_slice(&ciphertext);

    let encoded = general_purpose::STANDARD.encode(&combined);
    Ok(format!("{}{}", ENCRYPTED_PREFIX, encoded))
}

/// Decrypt an encrypted string.
///
/// Accepts strings with `ENC:` prefix (encrypted) or without (legacy plaintext).
/// Returns the plaintext on success.
pub fn decrypt(stored: &str) -> Result<String> {
    // If not encrypted (legacy plaintext), return as-is
    if !stored.starts_with(ENCRYPTED_PREFIX) {
        return Ok(stored.to_string());
    }

    let encoded = &stored[ENCRYPTED_PREFIX.len()..];
    let combined = general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| anyhow!("Base64 decode failed: {}", e))?;

    if combined.len() < 12 {
        return Err(anyhow!("Encrypted data too short"));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let key = derive_machine_key();
    let key = Key::<Aes256Gcm>::from_slice(&key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("AES-GCM decrypt failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow!("Decrypted data is not valid UTF-8: {}", e))
}

/// Check if a stored value is encrypted (has the ENC: prefix).
pub fn is_encrypted(stored: &str) -> bool {
    stored.starts_with(ENCRYPTED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let plaintext = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let encrypted = encrypt(plaintext).unwrap();
        assert!(is_encrypted(&encrypted));
        assert_ne!(encrypted, plaintext);
        let decrypted = decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_decrypt_legacy_plaintext() {
        let legacy = "0xabc123";
        let result = decrypt(legacy).unwrap();
        assert_eq!(result, legacy);
    }
}
