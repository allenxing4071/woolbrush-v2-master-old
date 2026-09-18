use serde::{Deserialize, Serialize};
use tauri::State;

use crate::infrastructure::data::DataClient;
use crate::state::AppState;

// ── Types matching frontend Position interface ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub market_id: String,
    pub question: String,
    pub side: String,
    pub size: f64,
    pub avg_price: f64,
    pub cur_price: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub status: String,
}

// ── Commands ──

/// 获取当前持仓（wallet + funder 地址合并）
#[tauri::command]
pub async fn get_positions(state: State<'_, AppState>) -> Result<Vec<Position>, String> {
    let settings = state
        .db
        .get_settings()
        .await
        .map_err(|e| format!("Failed to load settings: {}", e))?;

    let wallet_addr = settings.wallet_address.clone();
    let funder = settings
        .funder_address
        .as_deref()
        .filter(|s| !s.is_empty());

    if wallet_addr.is_empty() {
        tracing::warn!("get_positions: wallet_address is empty, returning empty");
        return Ok(Vec::new());
    }

    let http = state.http.read().await;

    let (wallet_positions, funder_positions) = tokio::join!(
        DataClient::get_positions(&http, &wallet_addr),
        async {
            match funder {
                Some(f) => DataClient::get_positions(&http, f).await,
                None => Ok(Vec::new()),
            }
        },
    );

    let mut positions: Vec<Position> = match wallet_positions {
        Ok(p) => p
            .into_iter()
            .map(|dp| Position {
                token_id: dp.token_id,
                market_id: dp.market_id,
                question: dp.question,
                side: dp.side,
                size: dp.size,
                avg_price: dp.avg_price,
                cur_price: dp.cur_price,
                realized_pnl: dp.realized_pnl,
                unrealized_pnl: dp.unrealized_pnl,
                status: dp.status,
            })
            .collect(),
        Err(e) => {
            tracing::error!("wallet_positions error for {}: {}", wallet_addr, e);
            Vec::new()
        }
    };

    if let Ok(fp) = funder_positions {
        for dp in fp {
            positions.push(Position {
                token_id: dp.token_id,
                market_id: dp.market_id,
                question: dp.question,
                side: dp.side,
                size: dp.size,
                avg_price: dp.avg_price,
                cur_price: dp.cur_price,
                realized_pnl: dp.realized_pnl,
                unrealized_pnl: dp.unrealized_pnl,
                status: dp.status,
            });
        }
    }

    tracing::info!("get_positions: {} positions returned", positions.len());

    Ok(positions)
}

/// 获取持仓总价值
#[tauri::command]
pub async fn get_portfolio_value(state: State<'_, AppState>) -> Result<f64, String> {
    let settings = state
        .db
        .get_settings()
        .await
        .map_err(|e| format!("Failed to load settings: {}", e))?;

    let wallet_addr = &settings.wallet_address;
    if wallet_addr.is_empty() {
        return Ok(0.0);
    }

    let http = state.http.read().await;
    DataClient::get_portfolio_value(&http, wallet_addr)
        .await
        .map_err(|e| e.to_string())
}
