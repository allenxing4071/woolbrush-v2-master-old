use std::sync::Arc;

use regex::Regex;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::infrastructure::proxy::SharedHttpClient;

const BASE_URL: &str = "https://gamma-api.polymarket.com";

/// Gamma API /events/{slug} 返回的完整事件结构
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct FullEvent {
    pub id: String,
    pub slug: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default, rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(default)]
    pub markets: Vec<RawMarket>,
}

/// Gamma API 事件下的子市场（一个温度档位）
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct RawMarket {
    pub id: String,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default, rename = "conditionId")]
    pub condition_id: Option<String>,
    #[serde(default, rename = "groupItemTitle")]
    pub group_item_title: Option<String>,
    #[serde(default, rename = "clobTokenIds")]
    pub clob_token_ids: Option<String>,
    #[serde(default, rename = "outcomePrices")]
    pub outcome_prices: Option<String>,
    #[serde(default)]
    pub outcomes: Option<String>,
}

/// Gamma API 返回的原始事件结构（用于城市列表扫描）
#[derive(Debug, Deserialize)]
struct RawEvent {
    slug: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    icon: Option<String>,
}

/// 从 event slug 解析城市 slug
///
/// e.g. "highest-temperature-in-london-on-july-28-2026" -> "london"
/// e.g. "lowest-temperature-in-new-york-on-july-28-2026" -> "new-york"
pub fn parse_city_from_slug(slug: &str) -> Option<String> {
    let prefix = if slug.starts_with("highest-temperature-in-") {
        "highest-temperature-in-"
    } else if slug.starts_with("lowest-temperature-in-") {
        "lowest-temperature-in-"
    } else {
        return None;
    };

    let rest = &slug[prefix.len()..];
    let city = rest.split("-on-").next()?;
    if city.is_empty() {
        return None;
    }
    Some(city.to_string())
}

/// 从 description 解析出的气象站信息
#[derive(Debug, Clone, Default)]
pub struct ParsedStation {
    pub station_name: Option<String>,
    pub station_code: Option<String>,
    pub station_url: Option<String>,
}

/// 从 description 解析气象站信息
///
/// 三种数据源格式：
/// 1. Wunderground: "recorded at the London City Airport Station ... information from Wunderground ... /history/daily/gb/london/EGLC"
/// 2. NOAA: "recorded by NOAA at the Istanbul Airport ... available here: https://www.weather.gov/wrh/timeseries?site=ltfm"
/// 3. HKO: "recorded by the Hong Kong Observatory ... available here: https://www.weather.gov.hk/en/cis/climat.htm"
pub fn parse_station_from_description(desc: &str) -> ParsedStation {
    // 站名
    let station_name = if let Some(m) = Regex::new(r"recorded at (.+?) Station").unwrap().captures(desc) {
        // 去掉前缀 "the "
        let name = m.get(1).unwrap().as_str().to_string();
        Some(name.strip_prefix("the ").unwrap_or(&name).to_string())
    } else if let Some(m) = Regex::new(r"recorded by NOAA at (.+?) in").unwrap().captures(desc) {
        let name = m.get(1).unwrap().as_str().to_string();
        Some(name.strip_prefix("the ").unwrap_or(&name).to_string())
    } else if desc.contains("Hong Kong Observatory") {
        Some("Hong Kong Observatory".to_string())
    } else {
        None
    };

    // 数据采集站点网址：规则说明中以 "available here: <URL>" 形式出现
    let station_url = Regex::new(r"available here:\s*(https?://[^\s\)\]]+)")
        .unwrap()
        .captures(desc)
        .map(|m| {
            let raw = m.get(1).unwrap().as_str();
            let cleaned = raw.trim_end_matches(|c| matches!(c, '.' | ',' | ';' | ')'));
            cleaned.to_string()
        })
        .filter(|u| !u.is_empty());

    // 机场代码：优先从采集站点网址提取（NOAA site= 参数或 Wunderground URL 末段），
    // 兼容旧规则说明中缺少 "available here:" 前缀的情况（直接在整个 description 中找）
    let station_code = station_url
        .as_deref()
        .and_then(parse_icao_from_url)
        .or_else(|| Regex::new(r"/history/daily/\S+/([A-Z]{3,5})")
            .unwrap()
            .captures(desc)
            .map(|m| m.get(1).unwrap().as_str().to_string()));

    ParsedStation {
        station_name,
        station_code,
        station_url,
    }
}

/// 从站点网址提取 ICAO 机场代码
///
/// 支持两种 URL 形态：
/// - NOAA: https://www.weather.gov/wrh/timeseries?site=kbkf  （site 参数多为小写）
/// - Wunderground: https://www.wunderground.com/history/daily/cn/jinan/ZSJN （ICAO 在路径末段）
fn parse_icao_from_url(url: &str) -> Option<String> {
    if let Some(m) = Regex::new(r"[?&]site=([a-zA-Z0-9]{3,5})")
        .unwrap()
        .captures(url)
    {
        return Some(m.get(1).unwrap().as_str().to_uppercase());
    }
    if let Some(m) = Regex::new(r"/history/daily/\S+/([A-Z0-9]{3,5})")
        .unwrap()
        .captures(url)
    {
        return Some(m.get(1).unwrap().as_str().to_string());
    }
    None
}

/// Polymarket 用户公开资料
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UserProfile {
    pub name: String,
    #[serde(default)]
    pub pseudonym: String,
    #[serde(default)]
    pub profile_image: Option<String>,
    #[serde(default)]
    pub taker_tier: u32,
    #[serde(rename = "takerTierName", default)]
    pub taker_tier_name: String,
}

/// Gamma API 客户端
///
/// 共享全局 HTTP 客户端（含代理配置），不自建连接。
#[derive(Clone)]
pub struct GammaClient {
    http: Arc<RwLock<reqwest::Client>>,
}

impl GammaClient {
    /// 使用共享 HTTP 客户端构造
    pub fn with_client(http: SharedHttpClient) -> Self {
        Self { http }
    }

    /// 获取用户公开资料
    ///
    /// 用钱包地址查询 Polymarket 上的用户名、头像、等级等信息。
    pub async fn get_profile(&self, address: &str) -> anyhow::Result<UserProfile> {
        let url = format!("{}/profiles/user_address/{}", BASE_URL, address);
        let client = self.http.read().await;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("Gamma profile API returned status {}", resp.status());
        }
        let json: serde_json::Value = resp.json().await?;

        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let pseudonym = json
            .get("pseudonym")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let profile_image = json
            .get("profileImage")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let taker_tier = json
            .get("takerTier")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let taker_tier_name = json
            .get("takerTierName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(UserProfile {
            name,
            pseudonym,
            profile_image,
            taker_tier,
            taker_tier_name,
        })
    }

    /// 发送 GET 请求并解析 JSON（统一走共享 HTTP 客户端 = 统一走代理）
    async fn fetch_json(&self, url: &str) -> anyhow::Result<Vec<RawEvent>> {
        let client = self.http.read().await;
        let resp = client.get(url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("Gamma API returned status {}", resp.status());
        }
        Ok(resp.json().await?)
    }

    /// 按 slug 精确查询事件，返回完整事件结构（含 markets 数组）
    ///
    /// Gamma API 端点: /events?slug={slug}
    /// 返回数组，取第一个匹配项。
    pub async fn fetch_event_by_slug(&self, slug: &str) -> anyhow::Result<Option<FullEvent>> {
        let url = format!("{}/events?slug={}", BASE_URL, slug);
        tracing::debug!("Fetching event by slug: {}", slug);

        let client = self.http.read().await;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("Gamma API returned status {} for slug {}", resp.status(), slug);
        }
        let events: Vec<FullEvent> = resp.json().await?;
        Ok(events.into_iter().next())
    }

    /// 拉取所有活跃气温事件，返回 (city_slug, station_name, station_code, avatar, station_url) 列表
    pub async fn fetch_active_temperature_cities(
        &self,
    ) -> anyhow::Result<Vec<(String, Option<String>, Option<String>, Option<String>, Option<String>)>> {
        let mut all_events = Vec::new();
        let mut offset = 0i32;
        let limit = 100i32;

        loop {
            let url = format!(
                "{}/events?tag_id=103040&active=true&closed=false&limit={}&offset={}",
                BASE_URL, limit, offset
            );
            tracing::debug!("Fetching Gamma API: offset={}", offset);

            let events = self.fetch_json(&url).await?;
            let fetched = events.len();
            all_events.extend(events);
            if (fetched as i32) < limit {
                break;
            }
            offset += limit;
        }

        tracing::info!("Gamma API returned {} events", all_events.len());

        // 从 slug 提取城市，从 description 提取气象站，取 image 作为头像
        let mut city_map: std::collections::HashMap<
            String,
            (Option<String>, Option<String>, Option<String>, Option<String>),
        > = std::collections::HashMap::new();

        for ev in &all_events {
            let Some(city) = parse_city_from_slug(&ev.slug) else {
                continue;
            };
            // 同一城市可能有多条事件，取第一条有 description 的
            if city_map.contains_key(&city) {
                continue;
            }
            let desc = ev.description.as_deref().unwrap_or("");
            let parsed = parse_station_from_description(desc);
            // 优先使用 icon（小图），fallback 到 image（大图）
            let avatar = ev
                .icon
                .clone()
                .filter(|u| !u.is_empty())
                .or_else(|| ev.image.clone().filter(|u| !u.is_empty()));
            city_map.insert(
                city,
                (parsed.station_name, parsed.station_code, avatar, parsed.station_url),
            );
        }

        Ok(city_map
            .into_iter()
            .map(|(city, (st_name, st_code, avatar, st_url))| {
                (city, st_name, st_code, avatar, st_url)
            })
            .collect())
    }
}
