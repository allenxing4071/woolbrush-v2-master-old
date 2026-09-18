use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use tokio::sync::{Mutex, RwLock, watch};

use crate::infrastructure::db::Database;
use crate::infrastructure::date_rollover::DateRolloverTracker;
use crate::infrastructure::gamma::GammaClient;
use crate::infrastructure::paths::data_dir;
use crate::infrastructure::position_monitor::PositionMonitor;
use crate::infrastructure::price_stream::PriceStreamManager;
use crate::infrastructure::proxy::{build_direct_http_client, build_http_client, detect_system_proxy, SharedHttpClient};
use crate::infrastructure::wallet::Wallet;
use crate::infrastructure::weather::{new_met_cache, MetCache};

/// 全局应用状态
pub struct AppState {
    pub db: Database,
    pub gamma: Arc<RwLock<GammaClient>>,
    /// 系统统一的 HTTP 客户端，所有需要网络请求的模块共享此实例
    pub http: SharedHttpClient,
    /// 当前生效的代理 URL（供 WS 连接复用）
    pub proxy_url: Arc<RwLock<Option<String>>>,
    /// WebSocket 价格流管理器
    pub price_stream: Mutex<PriceStreamManager>,
    /// 防止 stream_temperature_cities 并发执行（React StrictMode 双重调用）
    pub is_streaming: AtomicBool,
    /// stream 防重入锁
    pub stream_guard: Mutex<()>,
    /// 跨天日期追踪器
    pub date_tracker: Arc<DateRolloverTracker>,
    /// 城市列表广播通道（供 rollover 检测器获取最新城市列表）
    pub cities_tx: watch::Sender<Vec<(String, String)>>,
    /// Cached wallet with L2 credentials (rebuilt when settings change)
    pub cached_wallet: Mutex<Option<Wallet>>,
    /// 直连 HTTP 客户端（不走代理，用于千帆 API 等国内服务）
    /// 全局共享单例，避免每次 analyze_city 调用都新建 Client
    pub direct_http: reqwest::Client,
    /// MET 预报缓存（TTL 2h，避免 Open-Meteo 429 限流）
    pub met_cache: MetCache,
    /// 持仓监控器（止损/止盈自动平仓）
    pub position_monitor: Mutex<Option<PositionMonitor>>,
    /// LLM API Key 轮询计数器（多 Key 轮询调用，分散单 Key 调用压力）
    pub llm_key_counter: AtomicU64,
}

impl AppState {
    /// 初始化应用状态
    pub async fn new() -> anyhow::Result<Self> {
        // 数据库路径：统一通过 data_dir() 解析，确保打包后与 exe 同级
        let db_path = data_dir().join("woolbrush.db");
        let db = Database::open(db_path.to_str().unwrap()).await?;

        // 从 DB 读取代理设置
        let settings = db.get_settings().await.unwrap_or_else(|e| {
            tracing::warn!("Failed to load settings, using defaults: {}", e);
            crate::infrastructure::db::UserSettings {
                private_key: None,
                wallet_address: String::new(),
                funder_address: None,
                proxy_url: None,
                feishu_webhook: None,
                feishu_chat_id: None,
                qianfan_api_key: None,
                qianfan_secret_key: None,
                llm_provider: None,
                bailian_api_key: None,
                bailian_model: None,
                ollama_api_key: None,
                ollama_url: None,
                ollama_model: None,
                llm_prompt: None,
            }
        });

        // 确定生效代理：DB 配置优先，其次系统代理
        let proxy_url = settings
            .proxy_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(detect_system_proxy);

        // 构建全局共享 HTTP 客户端
        let http = build_http_client(proxy_url.as_deref())?;
        let shared_http = Arc::new(RwLock::new(http));

        // GammaClient 共享同一个 HTTP 客户端
        let gamma = GammaClient::with_client(Arc::clone(&shared_http));

        // 初始化城市列表 watch 通道
        let (cities_tx, _cities_rx) = watch::channel(Vec::<(String, String)>::new());

        // 初始化跨天日期追踪器
        let date_tracker = Arc::new(DateRolloverTracker::new());

        // 直连 HTTP 客户端（不走代理，用于千帆 API 等国内服务）
        let direct_http = build_direct_http_client()?;

        Ok(Self {
            db,
            gamma: Arc::new(RwLock::new(gamma)),
            http: shared_http,
            proxy_url: Arc::new(RwLock::new(proxy_url.clone())),
            price_stream: Mutex::new(PriceStreamManager::new()),
            is_streaming: AtomicBool::new(false),
            stream_guard: Mutex::new(()),
            date_tracker,
            cities_tx,
            cached_wallet: Mutex::new(None),
            direct_http,
            met_cache: new_met_cache(),
            position_monitor: Mutex::new(None),
            llm_key_counter: AtomicU64::new(0),
        })
    }

    /// 重建全局 HTTP 客户端（设置变更后调用）
    ///
    /// 同时更新 GammaClient 和所有持有 SharedHttpClient 引用的模块。
    pub async fn rebuild_http_client(&self, proxy_url: Option<&str>) -> anyhow::Result<()> {
        let effective_proxy = proxy_url
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(detect_system_proxy);

        let new_client = build_http_client(effective_proxy.as_deref())?;

        // 更新全局 HTTP 客户端
        {
            let mut guard = self.http.write().await;
            *guard = new_client;
        }

        // 更新 proxy_url（供 WS 连接复用）
        {
            let mut guard = self.proxy_url.write().await;
            *guard = effective_proxy.clone();
        }

        tracing::info!(
            "Global HTTP client rebuilt (proxy: {})",
            effective_proxy.as_deref().unwrap_or("direct")
        );

        Ok(())
    }
}
