mod commands;
mod error;
mod infrastructure;
mod state;

use std::sync::Arc;

use tauri::Manager;

use commands::{analyze_cmd, city_cmd, market_cmd, position_cmd, review_log_cmd, settings_cmd, toolbar_cmd, trade_cmd, weather_cmd};
use state::AppState;

// ── Global allocator: mimalloc ──
// Windows 默认堆分配器在大量小对象分配/释放后会产生严重碎片化，
// 导致 "memory allocation of N bytes failed" + STATUS_STACK_BUFFER_OVERRUN。
// mimalloc 提供更好的碎片整理能力和性能。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 安装 panic hook：在 panic 时确保日志写入文件后再退出
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            location = %info.location().map(|l| l.to_string()).unwrap_or_default(),
            payload = %info.payload().downcast_ref::<&str>().copied().unwrap_or_else(|| {
                info.payload().downcast_ref::<String>().map(|s| s.as_str()).unwrap_or("")
            }),
            backtrace = %backtrace,
            "RUST PANIC CAUGHT - application will exit"
        );
        default_hook(info);
    }));

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("WoolBrush V2 starting...");

    // 异步初始化状态
    let state = tauri::async_runtime::block_on(async {
        match AppState::new().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to initialize state: {}", e);
                panic!("State initialization failed: {}", e);
            }
        }
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(state)
        .setup(|app| {
            // 获取已注册的 AppState 引用
            let app_handle = app.handle().clone();

            // 从 managed state 中提取需要的组件
            let state = app.state::<AppState>();
            let cities_rx = state.cities_tx.subscribe();
            let date_tracker = Arc::clone(&state.date_tracker);

            // 启动跨天检测后台任务
            let scheduler_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                // 等待应用完全启动后再开始检测
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                tracing::info!("Starting date rollover detector...");
                infrastructure::date_rollover::spawn_rollover_detector(
                    app_handle,
                    date_tracker,
                    cities_rx,
                )
                .await;
            });

            // 启动天气定时刷新调度器
            commands::weather_cmd::spawn_weather_scheduler(scheduler_handle);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Market
            market_cmd::stream_temperature_cities,
            market_cmd::start_price_stream,
            market_cmd::backfill_prices,
            market_cmd::get_cached_prices,
            market_cmd::stop_price_stream,
            market_cmd::check_date_rollover,
            // Trade
            trade_cmd::open_position,
            trade_cmd::close_position,
            trade_cmd::sync_open_positions,
            trade_cmd::get_daily_pnl,
            trade_cmd::get_trade_records,
            trade_cmd::clear_trades,
            trade_cmd::save_excel,
            trade_cmd::start_position_monitor,
            trade_cmd::stop_position_monitor,
            trade_cmd::get_position_monitor_status,
            // Position
            position_cmd::get_positions,
            position_cmd::get_portfolio_value,
            // Settings
            settings_cmd::get_settings,
            settings_cmd::save_settings,
            settings_cmd::test_connection_with_settings,
            settings_cmd::get_account_summary,
            // City
            city_cmd::get_cities,
            city_cmd::update_cities,
            city_cmd::update_station_code,
            // Weather
            weather_cmd::fetch_weather,
            weather_cmd::fetch_weather_city,
            weather_cmd::fetch_weather_batch_cmd,
            // Analyze
            analyze_cmd::analyze_city,
            analyze_cmd::test_llm_connection,
            analyze_cmd::get_default_llm_prompt,
            analyze_cmd::get_llm_prompt_suffix,
            // Review Log
            review_log_cmd::append_review_log,
            // Toolbar
            toolbar_cmd::load_toolbar,
            toolbar_cmd::save_toolbar,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "Tauri runtime exited with error");
            std::process::exit(1);
        });
}
