use thiserror::Error;

/// 统一应用错误类型
#[derive(Debug, Error)]
pub enum AppError {
    #[error("API error: {0}")]
    Api(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Wallet error: {0}")]
    Wallet(String),

    #[error("DB error: {0}")]
    Db(String),

    #[error("Risk check failed: {0}")]
    RiskCheck(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Trade error: {0}")]
    Trade(String),
}

/// 自动转换为前端可读的 String（Tauri command 返回值需要 String 错误）
impl From<AppError> for String {
    fn from(e: AppError) -> String {
        e.to_string()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Api(e.to_string())
    }
}
