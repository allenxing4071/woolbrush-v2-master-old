use std::str::FromStr;

use alloy::primitives::hex::encode as hex_encode;
use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;

use crate::error::AppError;

/// L2 API 凭证
#[derive(Debug, Clone)]
pub struct ApiCreds {
    pub api_key: String,
    pub api_secret: String,
    pub api_passphrase: String,
}

const MSG_TO_SIGN: &str = "This message attests that I control the given wallet";
const CLOB_DOMAIN_NAME: &str = "ClobAuthDomain";
const CLOB_VERSION: &str = "1";

/// 钱包签名器
///
/// 负责以太坊 EIP-712 签名，用于 CLOB API 的 L1/L2 认证。
/// 私钥永远不离开此模块。
#[derive(Clone)]
pub struct Wallet {
    signer: PrivateKeySigner,
    chain_id: u64,
    creds: Option<ApiCreds>,
    funder_address: Option<String>,
}

impl Wallet {
    /// 从私钥创建钱包
    pub fn new(private_key: &str, chain_id: u64) -> Result<Self, AppError> {
        let signer = PrivateKeySigner::from_str(private_key)
            .map_err(|e| AppError::Wallet(format!("Invalid private key: {}", e)))?;

        Ok(Self {
            signer,
            chain_id,
            creds: None,
            funder_address: None,
        })
    }

    /// 获取钱包地址
    pub fn address(&self) -> String {
        self.signer.address().to_string()
    }

    /// 设置 L2 凭证
    pub fn set_creds(&mut self, creds: ApiCreds) {
        self.creds = Some(creds);
    }

    /// 设置 funder 地址（代理钱包模式）
    pub fn set_funder(&mut self, address: String) {
        self.funder_address = Some(address);
    }

    /// 创建或派生 L2 API 凭证（L1 操作）
    pub async fn create_or_derive_api_creds(
        &self,
        http_client: &reqwest::Client,
        base_url: &str,
    ) -> Result<ApiCreds, AppError> {
        let timestamp = chrono::Utc::now().timestamp();
        let nonce: u64 = 0;

        let hash = self.build_clob_auth_hash(timestamp, nonce);
        let signature = self
            .signer
            .sign_hash(&hash)
            .await
            .map_err(|e| AppError::Wallet(format!("Signing failed: {}", e)))?;
        let sig_hex = signature.to_string();

        let address = self.address();
        let timestamp_str = timestamp.to_string();
        let nonce_str = nonce.to_string();

        let l1_headers = [
            ("POLY_ADDRESS", address.as_str()),
            ("POLY_SIGNATURE", sig_hex.as_str()),
            ("POLY_TIMESTAMP", timestamp_str.as_str()),
            ("POLY_NONCE", nonce_str.as_str()),
        ];

        // 尝试 POST /auth/api-key（创建新 key）
        let post_url = format!("{}/auth/api-key", base_url);
        let mut req = http_client.post(&post_url);
        for (k, v) in &l1_headers {
            req = req.header(*k, *v);
        }

        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(creds) = parse_api_creds(&json) {
                        return Ok(creds);
                    }
                }
            }
        }

        // Fallback: GET /auth/derive-api-key（获取已有 key）
        let get_url = format!("{}/auth/derive-api-key", base_url);
        let mut req = http_client.get(&get_url);
        for (k, v) in &l1_headers {
            req = req.header(*k, *v);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| AppError::Network(format!("derive-api-key request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            // Don't log response body - may contain sensitive data
            return Err(AppError::Wallet(format!(
                "derive-api-key returned HTTP {} (response body suppressed for security)",
                status
            )));
        }

        let json = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| AppError::Api(format!("Failed to parse derive response: {}", e)))?;

        parse_api_creds(&json).ok_or_else(|| {
            tracing::warn!("L2 API response missing required credential fields");
            AppError::Api("Missing apiKey/secret/passphrase in derive-api-key response".into())
        })
    }

    /// 构建 EIP-712 ClobAuth 哈希
    fn build_clob_auth_hash(&self, timestamp: i64, nonce: u64) -> B256 {
        let domain_type_hash =
            keccak256(b"EIP712Domain(string name,string version,uint256 chainId)");

        let name_hash = keccak256(CLOB_DOMAIN_NAME.as_bytes());
        let version_hash = keccak256(CLOB_VERSION.as_bytes());
        let chain_id_word = u64_to_word(self.chain_id);

        let mut domain_input = Vec::with_capacity(128);
        domain_input.extend_from_slice(domain_type_hash.as_slice());
        domain_input.extend_from_slice(name_hash.as_slice());
        domain_input.extend_from_slice(version_hash.as_slice());
        domain_input.extend_from_slice(&chain_id_word);
        let domain_separator = keccak256(&domain_input);

        let struct_type_hash =
            keccak256(b"ClobAuth(address address,string timestamp,uint256 nonce,string message)");

        let address_word = address_to_word(&self.signer.address());
        let timestamp_hash = keccak256(timestamp.to_string().as_bytes());
        let nonce_word = u64_to_word(nonce);
        let message_hash = keccak256(MSG_TO_SIGN.as_bytes());

        let mut struct_input = Vec::with_capacity(160);
        struct_input.extend_from_slice(struct_type_hash.as_slice());
        struct_input.extend_from_slice(&address_word);
        struct_input.extend_from_slice(timestamp_hash.as_slice());
        struct_input.extend_from_slice(&nonce_word);
        struct_input.extend_from_slice(message_hash.as_slice());
        let struct_hash = keccak256(&struct_input);

        let mut final_input = Vec::with_capacity(66);
        final_input.push(0x19);
        final_input.push(0x01);
        final_input.extend_from_slice(domain_separator.as_slice());
        final_input.extend_from_slice(struct_hash.as_slice());

        keccak256(&final_input)
    }

    pub fn creds(&self) -> Option<&ApiCreds> {
        self.creds.as_ref()
    }

    pub fn funder_address(&self) -> Option<&str> {
        self.funder_address.as_deref()
    }

    /// 签名 EIP-712 V2 Order 消息
    pub async fn sign_order(&self, order_args: &OrderSignArgs) -> Result<String, AppError> {
        if order_args.signature_type == SignatureType::Poly1271 {
            return self.sign_order_poly_1271(order_args).await;
        }
        self.sign_order_eip712(order_args).await
    }

    /// 标准 EIP-712 签名（EOA / PolyProxy / PolyGnosisSafe）
    async fn sign_order_eip712(&self, order_args: &OrderSignArgs) -> Result<String, AppError> {
        let exchange_addr = order_args.exchange_address;

        let domain_type_hash = keccak256(b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
        let name_hash = keccak256(b"Polymarket CTF Exchange");
        let version_hash = keccak256(b"2");
        let chain_id_word = u256_to_word(U256::from(self.chain_id));
        let exchange_word = address_to_word(&exchange_addr);

        let mut domain_input = Vec::with_capacity(160);
        domain_input.extend_from_slice(domain_type_hash.as_slice());
        domain_input.extend_from_slice(name_hash.as_slice());
        domain_input.extend_from_slice(version_hash.as_slice());
        domain_input.extend_from_slice(&chain_id_word);
        domain_input.extend_from_slice(&exchange_word);
        let domain_separator = keccak256(&domain_input);

        let struct_type_hash = keccak256(
            b"Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)",
        );

        let maker_word = address_to_word(&order_args.maker);
        let signer_word = address_to_word(&self.signer.address());
        let token_id_word = u256_to_word(order_args.token_id);
        let maker_amount_word = u256_to_word(order_args.maker_amount);
        let taker_amount_word = u256_to_word(order_args.taker_amount);
        let side_word = u256_to_word(U256::from(order_args.side as u8));
        let sig_type_word = u256_to_word(U256::from(order_args.signature_type as u8));
        let timestamp_word = u256_to_word(U256::from(order_args.timestamp));
        let metadata_word = order_args.metadata;
        let builder_word = order_args.builder;

        let mut struct_input = Vec::with_capacity(416);
        struct_input.extend_from_slice(struct_type_hash.as_slice());
        struct_input.extend_from_slice(&u256_to_word(order_args.salt));
        struct_input.extend_from_slice(&maker_word);
        struct_input.extend_from_slice(&signer_word);
        struct_input.extend_from_slice(&token_id_word);
        struct_input.extend_from_slice(&maker_amount_word);
        struct_input.extend_from_slice(&taker_amount_word);
        struct_input.extend_from_slice(&side_word);
        struct_input.extend_from_slice(&sig_type_word);
        struct_input.extend_from_slice(&timestamp_word);
        struct_input.extend_from_slice(&metadata_word);
        struct_input.extend_from_slice(&builder_word);
        let struct_hash = keccak256(&struct_input);

        let mut final_input = Vec::with_capacity(66);
        final_input.push(0x19);
        final_input.push(0x01);
        final_input.extend_from_slice(domain_separator.as_slice());
        final_input.extend_from_slice(struct_hash.as_slice());
        let digest = keccak256(&final_input);

        let signature = self
            .signer
            .sign_hash(&digest)
            .await
            .map_err(|e| AppError::Wallet(format!("Order signing failed: {}", e)))?;

        Ok(signature.to_string())
    }

    /// POLY_1271 签名（deposit wallet flow / Solady 方案）
    async fn sign_order_poly_1271(&self, order_args: &OrderSignArgs) -> Result<String, AppError> {
        let exchange_addr = order_args.exchange_address;

        let domain_type_hash = keccak256(b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
        let name_hash = keccak256(b"Polymarket CTF Exchange");
        let version_hash = keccak256(b"2");
        let chain_id_word = u256_to_word(U256::from(self.chain_id));
        let exchange_word = address_to_word(&exchange_addr);

        let mut domain_input = Vec::with_capacity(160);
        domain_input.extend_from_slice(domain_type_hash.as_slice());
        domain_input.extend_from_slice(name_hash.as_slice());
        domain_input.extend_from_slice(version_hash.as_slice());
        domain_input.extend_from_slice(&chain_id_word);
        domain_input.extend_from_slice(&exchange_word);
        let app_domain_separator = keccak256(&domain_input);

        let order_type_string = b"Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)";
        let order_type_hash = keccak256(order_type_string);

        let maker_word = address_to_word(&order_args.maker);
        let signer_word = address_to_word(&order_args.maker);
        let token_id_word = u256_to_word(order_args.token_id);
        let maker_amount_word = u256_to_word(order_args.maker_amount);
        let taker_amount_word = u256_to_word(order_args.taker_amount);
        let side_word = u256_to_word(U256::from(order_args.side as u8));
        let sig_type_word = u256_to_word(U256::from(order_args.signature_type as u8));
        let timestamp_word = u256_to_word(U256::from(order_args.timestamp));
        let metadata_word = order_args.metadata;
        let builder_word = order_args.builder;

        let mut contents_input = Vec::with_capacity(416);
        contents_input.extend_from_slice(order_type_hash.as_slice());
        contents_input.extend_from_slice(&u256_to_word(order_args.salt));
        contents_input.extend_from_slice(&maker_word);
        contents_input.extend_from_slice(&signer_word);
        contents_input.extend_from_slice(&token_id_word);
        contents_input.extend_from_slice(&maker_amount_word);
        contents_input.extend_from_slice(&taker_amount_word);
        contents_input.extend_from_slice(&side_word);
        contents_input.extend_from_slice(&sig_type_word);
        contents_input.extend_from_slice(&timestamp_word);
        contents_input.extend_from_slice(&metadata_word);
        contents_input.extend_from_slice(&builder_word);
        let contents_hash = keccak256(&contents_input);

        let solady_type_string = b"TypedDataSign(Order contents,string name,string version,uint256 chainId,address verifyingContract,bytes32 salt)Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)";
        let solady_type_hash = keccak256(solady_type_string);

        let deposit_wallet_name_hash = keccak256(b"DepositWallet");
        let deposit_wallet_version_hash = keccak256(b"1");
        let deposit_wallet_domain_salt = [0u8; 32];

        let mut solady_input = Vec::with_capacity(288);
        solady_input.extend_from_slice(solady_type_hash.as_slice());
        solady_input.extend_from_slice(contents_hash.as_slice());
        solady_input.extend_from_slice(deposit_wallet_name_hash.as_slice());
        solady_input.extend_from_slice(deposit_wallet_version_hash.as_slice());
        solady_input.extend_from_slice(&chain_id_word);
        solady_input.extend_from_slice(&signer_word);
        solady_input.extend_from_slice(&deposit_wallet_domain_salt);
        let typed_data_sign_struct_hash = keccak256(&solady_input);

        let mut final_input = Vec::with_capacity(66);
        final_input.push(0x19);
        final_input.push(0x01);
        final_input.extend_from_slice(app_domain_separator.as_slice());
        final_input.extend_from_slice(typed_data_sign_struct_hash.as_slice());
        let digest = keccak256(&final_input);

        let signature = self
            .signer
            .sign_hash(&digest)
            .await
            .map_err(|e| AppError::Wallet(format!("POLY_1271 signing failed: {}", e)))?;

        let sig_bytes = signature.as_bytes();
        let inner_sig_hex = hex_encode(sig_bytes);

        let contents_type_hex = hex_encode(order_type_string);

        let type_len = order_type_string.len();
        let contents_type_len_hex = format!("{:04x}", type_len);

        let result = format!(
            "0x{}{}{}{}{}",
            inner_sig_hex,
            hex_encode(app_domain_separator.as_slice()),
            hex_encode(contents_hash.as_slice()),
            contents_type_hex,
            contents_type_len_hex,
        );

        Ok(result)
    }
}

/// 订单签名参数（用于 EIP-712 V2 Order 结构体）
#[derive(Debug, Clone)]
pub struct OrderSignArgs {
    pub salt: U256,
    pub exchange_address: Address,
    pub maker: Address,
    pub token_id: U256,
    pub maker_amount: U256,
    pub taker_amount: U256,
    pub side: OrderSide,
    pub signature_type: SignatureType,
    pub timestamp: u64,
    pub metadata: [u8; 32],
    pub builder: [u8; 32],
}

/// 订单方向（CLOB 语义）
#[derive(Debug, Clone, Copy)]
pub enum OrderSide {
    Buy = 0,
    Sell = 1,
}

/// 签名类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignatureType {
    /// EOA 直接签名（预留，当前未使用）
    #[allow(dead_code)]
    Eoa = 0,
    /// PolyProxy 签名（预留，当前未使用）
    #[allow(dead_code)]
    PolyProxy = 1,
    /// PolyGnosisSafe 签名（预留，当前未使用）
    #[allow(dead_code)]
    PolyGnosisSafe = 2,
    /// Poly1271 签名（代理钱包模式，当前使用）
    Poly1271 = 3,
}

// ── ABI 编码辅助 ──

fn address_to_word(addr: &alloy::primitives::Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(addr.as_slice());
    word
}

fn u64_to_word(val: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..32].copy_from_slice(&val.to_be_bytes());
    word
}

fn u256_to_word(val: U256) -> [u8; 32] {
    val.to_be_bytes()
}

/// 从 JSON 响应解析 L2 凭证
fn parse_api_creds(json: &serde_json::Value) -> Option<ApiCreds> {
    let api_key = json.get("apiKey").and_then(|v| v.as_str())?;
    let secret = json.get("secret").and_then(|v| v.as_str())?;
    let passphrase = json.get("passphrase").and_then(|v| v.as_str())?;

    Some(ApiCreds {
        api_key: api_key.to_string(),
        api_secret: secret.to_string(),
        api_passphrase: passphrase.to_string(),
    })
}
