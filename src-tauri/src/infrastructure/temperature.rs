use chrono::{Datelike, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::infrastructure::gamma::{FullEvent, GammaClient, RawMarket};

/// 温度市场类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MarketType {
    Highest,
}

/// 温度档位（一个可交易的结果区间）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempThreshold {
    pub label: String,
    pub question: String,
    pub slug: String,
    pub market_id: String,
    pub condition_id: String,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_price: f64,
    pub no_price: f64,
}

/// 气温市场事件（一个城市 + 日期 + 类型的完整事件）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempMarketEvent {
    pub event_id: String,
    pub event_slug: String,
    pub title: String,
    pub city: String,
    pub city_tz: String,
    pub market_type: MarketType,
    pub end_date_iso: String,
    pub image: Option<String>,
    pub icon: Option<String>,
    pub thresholds: Vec<TempThreshold>,
}

/// 城市气温市场汇总（最高温市场）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CityTempMarkets {
    pub city: String,
    pub city_tz: String,
    pub highest: Option<TempMarketEvent>,
}

/// 月份名称（用于构造 Polymarket slug）
const MONTH_NAMES: [&str; 12] = [
    "january", "february", "march", "april", "may", "june",
    "july", "august", "september", "october", "november", "december",
];

/// 根据城市时区计算当地当前日期
///
/// 核心逻辑：UTC 时间 + 时区偏移 = 当地时间，取当地日期。
/// 这保证了查询的温度市场是当地当天结算的市场。
pub fn local_date(iana_tz: &str) -> Option<NaiveDate> {
    let tz: Tz = iana_tz.parse().ok()?;
    let now_local = Utc::now().with_timezone(&tz);
    Some(now_local.date_naive())
}

/// 构造 Polymarket 气温事件 slug
///
/// 格式: highest-temperature-in-{city}-on-{month}-{day}-{year}
/// 例: highest-temperature-in-london-on-july-30-2026
fn build_slug(market_type: MarketType, city_slug: &str, date: NaiveDate) -> String {
    let prefix = match market_type {
        MarketType::Highest => "highest-temperature",
    };
    let month_name = MONTH_NAMES[(date.month() as usize) - 1];
    format!(
        "{}-in-{}-on-{}-{}-{}",
        prefix, city_slug, month_name, date.day(), date.year()
    )
}

/// 从 RawMarket 解析温度档位信息
fn parse_threshold(market: &RawMarket) -> Option<TempThreshold> {
    let question = market.question.as_deref().unwrap_or("");
    let group_title = market.group_item_title.as_deref().unwrap_or("");

    // 解析 clobTokenIds (JSON 字符串，如 "[\"123\", \"456\"]")
    let token_ids: Vec<String> = market
        .clob_token_ids
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    if token_ids.len() < 2 {
        return None;
    }

    // 解析 outcomePrices (JSON 字符串，如 "[\"0.95\", \"0.05\"]")
    let prices: Vec<f64> = market
        .outcome_prices
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .map(|v| v.iter().filter_map(|p| p.parse::<f64>().ok()).collect())
        .unwrap_or_default();

    let yes_price = prices.first().copied().unwrap_or(0.0);
    let no_price = prices.get(1).copied().unwrap_or(0.0);

    // 优先使用 groupItemTitle 作为 label，回退到 question
    let label = if !group_title.is_empty() {
        group_title.to_string()
    } else {
        question.to_string()
    };

    Some(TempThreshold {
        label,
        question: question.to_string(),
        slug: market.slug.clone().unwrap_or_default(),
        market_id: market.id.clone(),
        condition_id: market.condition_id.clone().unwrap_or_default(),
        yes_token_id: token_ids[0].clone(),
        no_token_id: token_ids[1].clone(),
        yes_price,
        no_price,
    })
}

/// 将 FullEvent 转换为 TempMarketEvent
fn event_to_temp_market(
    event: &FullEvent,
    city: &str,
    city_tz: &str,
    market_type: MarketType,
) -> TempMarketEvent {
    let thresholds: Vec<TempThreshold> = event
        .markets
        .iter()
        .filter_map(parse_threshold)
        .collect();

    TempMarketEvent {
        event_id: event.id.clone(),
        event_slug: event.slug.clone(),
        title: event.title.clone().unwrap_or_default(),
        city: city.to_string(),
        city_tz: city_tz.to_string(),
        market_type,
        end_date_iso: event.end_date.clone().unwrap_or_default(),
        image: event.image.clone(),
        icon: event.icon.clone(),
        thresholds,
    }
}

/// 加载单个城市的气温市场数据
///
/// 根据城市时区计算当地日期，构造 highest slug 查询 Gamma API 获取完整事件数据。
/// 仅查询最高温市场，不查询最低温市场。
pub async fn load_city_markets(
    gamma: &GammaClient,
    city_slug: &str,
    iana_tz: &str,
) -> anyhow::Result<CityTempMarkets> {
    let local_dt = local_date(iana_tz).ok_or_else(|| {
        anyhow::anyhow!("Failed to compute local date for tz: {}", iana_tz)
    })?;

    tracing::debug!(
        "Loading markets for {} (tz={}, local_date={})",
        city_slug, iana_tz, local_dt
    );

    let highest_slug = build_slug(MarketType::Highest, city_slug, local_dt);

    let highest = gamma
        .fetch_event_by_slug(&highest_slug)
        .await
        .ok()
        .flatten()
        .map(|ev| event_to_temp_market(&ev, city_slug, iana_tz, MarketType::Highest));

    if highest.is_none() {
        tracing::debug!(
            "No active highest-temperature event for {} on {} (slug: {})",
            city_slug, local_dt, highest_slug
        );
    }

    Ok(CityTempMarkets {
        city: city_slug.to_string(),
        city_tz: iana_tz.to_string(),
        highest,
    })
}
