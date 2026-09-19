use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::AppError;
use crate::infrastructure::wallet::{OrderSide, OrderSignArgs, SignatureType, Wallet};

const BASE_URL: &str = "https://clob.polymarket.com";

/// pUSD 合约地址（Polygon 上）
const PUSD_CONTRACT: &str = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB";
/// balanceOf(address) 的 4 字节选择器
const BALANCE_OF_SELECTOR: &str = "0x70a08231";
/// 免费公开 Polygon RPC
const POLYGON_RPC: &str = "https://polygon-bor-rpc.publicnode.com";

/// CTF Exchange V2 合约地址
const CTF_EXCHANGE_V2: &str = "0xE111180000d2663C0091e4f400237545B87B996B";
/// Negative Risk Exchange V2 合约地址
const NEG_RISK_EXCHANGE_V2: &str = "0xe2222d279d744050d28e00520010520000310F59";

type HmacSha256 = Hmac<Sha256>;

/// CLOB API 客户端
///
/// 公开端点无需认证（订单簿、价格）；
/// 交易端点需 L2 API Key 认证（下单、撤单）。
/// 使用共享 HTTP 客户端，不自建连接。
#[derive(Clone)]
pub struct ClobClient {
    wallet: Option<Wallet>,
}

/// 订单簿
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub bids: Vec<OrderBookEntry>,
    pub asks: Vec<OrderBookEntry>,
}

#[derive(Debug, Clone)]
pub struct OrderBookEntry {
    pub price: f64,
    /// 该价位挂单数量（shares）；接口未返回时为 0
    pub size: f64,
}

/// 订单时效类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    /// 挂单直到成交或撤销；未吃完的部分留在盘口
    Gtc,
    /// Fill-and-Kill：立即吃掉可成交部分，剩余立即撤销，不留挂单
    Fak,
}

impl OrderType {
    fn as_str(self) -> &'static str {
        match self {
            OrderType::Gtc => "GTC",
            OrderType::Fak => "FAK",
        }
    }
}

/// 下单参数
#[derive(Debug, Clone)]
pub struct OrderArgs {
    pub price: f64,
    pub size: u64,
    pub token_id: String,
    pub order_type: OrderType,
}

/// 订单响应
#[derive(Debug, Clone)]
pub struct OrderResponse {
    pub order_id: String,
    /// CLOB 返回的状态（matched / live / delayed / unmatched ...）
    pub status: Option<String>,
    /// 买单：实际获得的 shares；卖单：实际支付的 shares。接口未返回时为 None
    pub taking_amount: Option<f64>,
    /// 买单：实际支付的 USDC；卖单：实际获得的 USDC
    pub making_amount: Option<f64>,
}

impl ClobClient {
    pub fn new() -> Self {
        Self { wallet: None }
    }

    pub fn with_wallet(mut self, wallet: Wallet) -> Self {
        self.wallet = Some(wallet);
        self
    }

    // ── 公开端点 ──

    /// 获取订单簿
    pub async fn get_orderbook(
        &self,
        http: &reqwest::Client,
        token_id: &str,
    ) -> Result<OrderBook, AppError> {
        let url = format!(
            "{}/book?token_id={}",
            BASE_URL,
            urlencoding::encode(token_id)
        );
        let resp = http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Network(e.to_string()))?;

        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| AppError::Api(e.to_string()))?;

        let mut bids = value
            .get("bids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let price = entry
                            .get("price")
                            .and_then(|p| p.as_str())
                            .and_then(|s| s.parse::<f64>().ok())?;
                        let size = entry
                            .get("size")
                            .and_then(|p| p.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        Some(OrderBookEntry { price, size })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut asks = value
            .get("asks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let price = entry
                            .get("price")
                            .and_then(|p| p.as_str())
                            .and_then(|s| s.parse::<f64>().ok())?;
                        let size = entry
                            .get("size")
                            .and_then(|p| p.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        Some(OrderBookEntry { price, size })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        bids.sort_by(|a, b| {
            b.price
                .partial_cmp(&a.price)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        asks.sort_by(|a, b| {
            a.price
                .partial_cmp(&b.price)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(OrderBook {
            bids,
            asks,
        })
    }

    /// 批量查询多个 token 的订单簿（POST /books）
    ///
    /// 用于复盘数据采集：一次请求拿到整个城市 11 个档位的盘口深度，
    /// 避免 51 城 × 11 档逐个调 /book 造成的请求风暴。
    /// 返回 (token_id, OrderBook) 列表；接口未返回某个 token 时该项缺失。
    pub async fn get_orderbooks(
        &self,
        http: &reqwest::Client,
        token_ids: &[String],
    ) -> Result<Vec<(String, OrderBook)>, AppError> {
        if token_ids.is_empty() {
            return Ok(Vec::new());
        }
        let body: Vec<serde_json::Value> = token_ids
            .iter()
            .map(|t| serde_json::json!({ "token_id": t }))
            .collect();

        let resp = http
            .post(format!("{}/books", BASE_URL))
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Network(e.to_string()))?;

        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| AppError::Api(e.to_string()))?;

        let arr = value.as_array().cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(arr.len());
        for book in arr {
            let token_id = match book.get("asset_id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let parse_side = |key: &str| -> Vec<OrderBookEntry> {
                book.get(key)
                    .and_then(|v| v.as_array())
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|e| {
                                let price = e
                                    .get("price")
                                    .and_then(|p| p.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())?;
                                let size = e
                                    .get("size")
                                    .and_then(|p| p.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                                    .unwrap_or(0.0);
                                Some(OrderBookEntry { price, size })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let mut bids = parse_side("bids");
            let mut asks = parse_side("asks");
            bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));
            asks.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
            out.push((token_id, OrderBook { bids, asks }));
        }
        Ok(out)
    }

    /// 查询市场是否为 NegRisk
    pub async fn get_neg_risk(
        &self,
        http: &reqwest::Client,
        token_id: &str,
    ) -> Result<bool, AppError> {
        let url = format!(
            "{}/neg-risk?token_id={}",
            BASE_URL,
            urlencoding::encode(token_id)
        );
        let resp = http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Network(e.to_string()))?;

        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| AppError::Api(e.to_string()))?;

        value
            .get("neg_risk")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| AppError::Api("Invalid neg-risk response".into()))
    }

    // ── 认证端点 (L2) ──

    /// 创建订单（需要 L2 认证）
    pub async fn create_order(
        &self,
        http: &reqwest::Client,
        order: &OrderArgs,
    ) -> Result<OrderResponse, AppError> {
        // Buy: maker pays USDC (price * size), taker receives tokens (size * 1e6)
        let maker_amount = order.size as u128 * ((order.price * 1e6).round() as u128);
        let taker_amount = order.size as u128 * 1_000_000u128;

        self.build_and_submit_order(
            http,
            &order.token_id,
            order.size,
            maker_amount,
            taker_amount,
            OrderSide::Buy,
            order.order_type,
        )
        .await
    }

    /// 创建卖单（平仓）
    pub async fn create_sell_order(
        &self,
        http: &reqwest::Client,
        token_id: &str,
        size: u64,
        price: f64,
    ) -> Result<OrderResponse, AppError> {
        // Sell: maker delivers tokens (size * 1e6), taker pays USDC (price * size * 1e6)
        let maker_amount = size as u128 * 1_000_000u128;
        let taker_amount = size as u128 * ((price * 1e6).round() as u128);

        self.build_and_submit_order(
            http,
            token_id,
            size,
            maker_amount,
            taker_amount,
            OrderSide::Sell,
            OrderType::Gtc,
        )
        .await
    }

    /// Shared order building + signing + submission logic.
    ///
    /// Both buy and sell orders follow the same pattern: construct EIP-712
    /// sign args, sign, build JSON payload, submit with L2 HMAC headers.
    async fn build_and_submit_order(
        &self,
        http: &reqwest::Client,
        token_id: &str,
        size: u64,
        maker_amount: u128,
        taker_amount: u128,
        side: OrderSide,
        order_type: OrderType,
    ) -> Result<OrderResponse, AppError> {
        let wallet = self
            .wallet
            .as_ref()
            .ok_or_else(|| AppError::Wallet("Wallet not initialized".into()))?;

        let creds = wallet
            .creds()
            .ok_or_else(|| AppError::Wallet("L2 API credentials not derived".into()))?;

        let salt = generate_salt();

        let signer_addr_str = wallet.address();
        let funder = wallet
            .funder_address()
            .unwrap_or_else(|| signer_addr_str.as_str());
        let maker = parse_address(funder)?;
        let token_id_u256 = parse_u256(token_id)?;

        let sig_type = SignatureType::Poly1271;

        // neg_risk query: fail hard instead of defaulting to false.
        // Using wrong exchange address would cause the order to be rejected
        // or sent to the wrong contract.
        let neg_risk = self.get_neg_risk(http, token_id).await?;
        let exchange_addr_str = if neg_risk {
            NEG_RISK_EXCHANGE_V2
        } else {
            CTF_EXCHANGE_V2
        };
        let side_str = match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        };
        tracing::info!(
            "Order: side={} token_id={} size={} neg_risk={} exchange={}",
            side_str, token_id, size, neg_risk, exchange_addr_str
        );
        let exchange_addr = parse_address(exchange_addr_str)?;

        // Unified timestamp: use millisecond precision for both signature
        // and API header to avoid timestamp mismatch issues.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // Signature uses ms; CLOB HMAC header also uses ms (as string)
        let ts_ms = now;
        let timestamp_str = ts_ms.to_string();

        let sign_args = OrderSignArgs {
            salt,
            exchange_address: exchange_addr,
            maker,
            token_id: token_id_u256,
            maker_amount: alloy::primitives::U256::from(maker_amount),
            taker_amount: alloy::primitives::U256::from(taker_amount),
            side,
            signature_type: sig_type,
            timestamp: ts_ms,
            metadata: [0u8; 32],
            builder: [0u8; 32],
        };

        let signature = wallet
            .sign_order(&sign_args)
            .await
            .map_err(|e| AppError::Wallet(format!("Order signing failed: {}", e)))?;

        let maker_str = funder.to_string();
        let salt_str = salt.to_string();
        let order_payload = serde_json::json!({
            "order": {
                "salt": salt_str.parse::<serde_json::Value>().unwrap_or(serde_json::Value::Null),
                "maker": maker_str,
                "signer": maker_str,
                "tokenId": token_id,
                "makerAmount": maker_amount.to_string(),
                "takerAmount": taker_amount.to_string(),
                "side": side_str,
                "expiration": "0",
                "signatureType": sig_type as u8,
                "timestamp": timestamp_str,
                "metadata": "0x0000000000000000000000000000000000000000000000000000000000000000",
                "builder": "0x0000000000000000000000000000000000000000000000000000000000000000",
                "signature": signature,
            },
            "owner": creds.api_key,
            "orderType": order_type.as_str(),
            "deferExec": false,
            "postOnly": false,
        });

        let request_path = "/order";
        let request_method = "POST";
        let body_str = serde_json::to_string(&order_payload)
            .map_err(|e| AppError::Api(format!("Serialize order body failed: {}", e)))?;

        // Log order metadata only (no signature or sensitive payload)
        tracing::info!(
            "Submitting {} {} order: token_id={} size={} maker={} exchange={}",
            side_str, order_type.as_str(), token_id, size, maker_str, exchange_addr_str
        );

        let clob_signature = build_clob_signature(
            &creds.api_secret,
            ts_ms,
            request_method,
            request_path,
            &body_str,
        );

        let signer_addr = wallet.address();
        let url = format!("{}{}", BASE_URL, request_path);
        let resp = http
            .post(&url)
            .header("POLY_ADDRESS", &signer_addr)
            .header("POLY_SIGNATURE", clob_signature)
            .header("POLY_TIMESTAMP", &timestamp_str)
            .header("POLY_API_KEY", &creds.api_key)
            .header("POLY_PASSPHRASE", &creds.api_passphrase)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("Order request failed: {}", e)))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AppError::Api(format!("Read response body failed: {}", e)))?;

        if !status.is_success() {
            return Err(AppError::Trade(format!(
                "CLOB {} order HTTP {}: {}",
                side_str, status, body
            )));
        }

        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| AppError::Api(format!("Parse order response failed: {}", e)))?;

        parse_order_response(&json)
    }

    // ── 链上查询 ──

    /// 查询指定地址的 pUSD 余额（Polygon RPC eth_call balanceOf）
    pub async fn get_pusd_balance(
        http: &reqwest::Client,
        address: &str,
    ) -> Result<f64, AppError> {
        let addr_hex = address.trim_start_matches("0x").to_lowercase();
        let padded_addr = format!("{:0>64}", addr_hex);
        let calldata = format!("{}{}", BALANCE_OF_SELECTOR, padded_addr);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [
                { "to": PUSD_CONTRACT, "data": calldata },
                "latest"
            ]
        });

        let resp = http
            .post(POLYGON_RPC)
            .json(&payload)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("Polygon RPC request failed: {}", e)))?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::Api(format!("Failed to parse RPC response: {}", e)))?;

        if let Some(err) = json.get("error") {
            return Err(AppError::Api(format!("Polygon RPC error: {}", err)));
        }

        let hex_result = json
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("0x0");

        let balance_raw = u128::from_str_radix(hex_result.trim_start_matches("0x"), 16)
            .map_err(|e| AppError::Api(format!("Failed to parse balance hex: {}", e)))?;

        Ok(balance_raw as f64 / 1e6)
    }
}

// ── 辅助函数 ──

fn parse_order_response(json: &serde_json::Value) -> Result<OrderResponse, AppError> {
    // CLOB 在 HTTP 200 下也可能返回 success=false（余额不足、市场关闭等）
    if json.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let msg = json
            .get("errorMsg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(AppError::Trade(format!("CLOB rejected order: {}", msg)));
    }
    let order_id = json
        .get("orderID")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("id").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let status = json
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let num = |key: &str| -> Option<f64> {
        json.get(key).and_then(|v| match v {
            serde_json::Value::String(s) => s.parse::<f64>().ok(),
            serde_json::Value::Number(n) => n.as_f64(),
            _ => None,
        })
        .filter(|x| x.is_finite() && *x >= 0.0)
    };

    Ok(OrderResponse {
        order_id,
        status,
        taking_amount: num("takingAmount"),
        making_amount: num("makingAmount"),
    })
}

/// 构建 CLOB L2 API 签名 (HMAC-SHA256)
fn build_clob_signature(
    secret: &str,
    timestamp: u64,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let secret_bytes = general_purpose::URL_SAFE
        .decode(secret)
        .or_else(|_| general_purpose::STANDARD.decode(secret))
        .unwrap_or_else(|_| secret.as_bytes().to_vec());

    let message = format!("{}{}{}{}", timestamp, method, path, body);

    let mut mac = HmacSha256::new_from_slice(&secret_bytes).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let result = mac.finalize().into_bytes();

    general_purpose::URL_SAFE.encode(result)
}

/// 生成 salt（对齐 Python SDK: int(random.random() * timestamp_ms)）
fn generate_salt() -> alloy::primitives::U256 {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let uuid = uuid::Uuid::new_v4();
    let rand_bytes = uuid.as_bytes();
    let rand_u32 = u32::from_le_bytes([
        rand_bytes[0],
        rand_bytes[1],
        rand_bytes[2],
        rand_bytes[3],
    ]) >> 5;
    let rand_f64 = rand_u32 as f64 / 134217728.0;
    let salt = (rand_f64 * ts_ms as f64) as u64;
    alloy::primitives::U256::from(salt)
}

/// 将 hex 地址字符串解析为 alloy Address
fn parse_address(addr: &str) -> Result<alloy::primitives::Address, AppError> {
    if addr.is_empty() {
        return Ok(alloy::primitives::Address::ZERO);
    }
    let hex = addr.trim_start_matches("0x");
    if hex.len() != 40 {
        return Err(AppError::Wallet(format!(
            "Invalid address length '{}': expected 40 hex chars, got {}",
            addr,
            hex.len()
        )));
    }
    let bytes = alloy::primitives::hex::decode(hex)
        .map_err(|e| AppError::Wallet(format!("Invalid address hex '{}': {}", addr, e)))?;
    if bytes.len() != 20 {
        return Err(AppError::Wallet(format!(
            "Invalid address bytes: expected 20, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&bytes);
    Ok(alloy::primitives::Address::new(arr))
}

/// 将 token_id 字符串解析为 U256
fn parse_u256(token_id: &str) -> Result<alloy::primitives::U256, AppError> {
    alloy::primitives::U256::from_str_radix(token_id.trim_start_matches("0x"), 10)
        .or_else(|_| {
            token_id
                .parse::<alloy::primitives::U256>()
                .map_err(|e| AppError::Api(format!("Invalid token_id '{}': {}", token_id, e)))
        })
        .map_err(|e| AppError::Api(format!("Invalid token_id '{}': {}", token_id, e)))
}
