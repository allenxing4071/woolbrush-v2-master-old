use serde::{Deserialize, Serialize};

use crate::error::AppError;

const BASE_URL: &str = "https://data-api.polymarket.com";

/// 持仓（来自 Data API）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    #[serde(alias = "asset")]
    pub token_id: String,
    #[serde(alias = "conditionId")]
    pub market_id: String,
    #[serde(alias = "title")]
    pub question: String,
    #[serde(alias = "outcome")]
    pub side: String,
    pub size: f64,
    #[serde(alias = "avgPrice")]
    pub avg_price: f64,
    #[serde(alias = "curPrice")]
    pub cur_price: f64,
    #[serde(alias = "realizedPnl")]
    pub realized_pnl: f64,
    #[serde(alias = "cashPnl")]
    pub unrealized_pnl: f64,
    #[serde(default)]
    pub status: String,
}

/// Data API 客户端（公开，无需认证）
///
/// 使用共享 HTTP 客户端查询用户持仓和持仓总价值。
pub struct DataClient;

impl DataClient {
    /// 获取用户当前持仓
    pub async fn get_positions(
        http: &reqwest::Client,
        address: &str,
    ) -> Result<Vec<Position>, AppError> {
        let resp = http
            .get(format!("{}/positions?user={}", BASE_URL, address))
            .send()
            .await
            .map_err(|e| AppError::Network(e.to_string()))?;

        let positions = resp
            .json::<Vec<Position>>()
            .await
            .map_err(|e| AppError::Api(e.to_string()))?;

        Ok(positions)
    }

    /// 获取用户交易活动（含买卖历史），用于对账同步
    pub async fn get_activity(
        http: &reqwest::Client,
        address: &str,
    ) -> Result<Vec<serde_json::Value>, AppError> {
        let resp = http
            .get(format!("{}/activity?user={}&limit=500", BASE_URL, address))
            .send()
            .await
            .map_err(|e| AppError::Network(e.to_string()))?;

        resp.json::<Vec<serde_json::Value>>()
            .await
            .map_err(|e| AppError::Api(e.to_string()))
    }

    /// 获取用户持仓总价值
    pub async fn get_portfolio_value(
        http: &reqwest::Client,
        address: &str,
    ) -> Result<f64, AppError> {
        let resp = http
            .get(format!("{}/value?user={}", BASE_URL, address))
            .send()
            .await
            .map_err(|e| AppError::Network(e.to_string()))?;

        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| AppError::Api(e.to_string()))?;

        value
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("value"))
            .and_then(|v| v.as_f64())
            .ok_or_else(|| AppError::Api("Invalid portfolio value response".into()))
    }
}
