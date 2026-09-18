use chrono::{DateTime, Local, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::infrastructure::proxy::SharedHttpClient;

/// 当前本地时间格式化为 "HH:mm:ss"
fn now_local_str() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

// ── MET 缓存 ──

/// MET 预报缓存 TTL：1 小时。
/// Open-Meteo 同日预报在数小时内变化极小，已过去的小时返回实况值（完全不变）。
/// 1 小时 TTL 在保证数据新鲜度的同时，将日请求量从 ~2500 降至 ~24。
/// 定时调度器每 60 秒调用一次 batch，MET 通过此缓存自动控制为每小时刷新一次。
const MET_CACHE_TTL: Duration = Duration::from_secs(3600);

/// 缓存的单条 MET 预报
#[derive(Clone)]
pub struct CachedMet {
    /// 抓取时刻（用于 TTL 判断）
    fetched_at: Instant,
    /// 预报数据（hour -> temp °C）
    data: HashMap<u32, f64>,
}

/// MET 预报缓存：key = "lat,lon,tz,date"，全局共享
pub type MetCache = Arc<tokio::sync::RwLock<HashMap<String, CachedMet>>>;

/// 创建空的 MET 缓存
pub fn new_met_cache() -> MetCache {
    Arc::new(tokio::sync::RwLock::new(HashMap::new()))
}

// ── AWC METAR ──

#[derive(Debug, Clone, Deserialize)]
struct MetarResponse {
    #[serde(rename = "icaoId")]
    icao_id: Option<String>,
    temp: Option<f64>,
    /// 观测时间 (Unix timestamp)
    #[serde(rename = "obsTime")]
    obs_time: Option<i64>,
    /// 纬度
    lat: Option<f64>,
    /// 经度
    lon: Option<f64>,
    /// 报文类型：METAR（正式观测）/ SPECI（特选报）
    #[serde(rename = "metarType", default)]
    metar_type: Option<String>,
    /// 主导云量代码：CLR/SKC/FEW/SCT/BKN/OVC
    #[serde(default)]
    cover: Option<String>,
}

/// AWC METAR 数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwcData {
    /// 实时温度 (°C)
    pub current: Option<f64>,
    /// 当天最高温度 (°C)
    pub max: Option<f64>,
    /// 当天按小时的观测温度 (hour -> temp °C)
    #[serde(rename = "hourlyTemps")]
    pub hourly_temps: HashMap<u32, f64>,
    /// 观测站纬度
    pub lat: Option<f64>,
    /// 观测站经度
    pub lon: Option<f64>,
}

/// 从 AWC 获取 METAR 数据
///
/// API: https://aviationweather.gov/api/data/metar?ids=ICAO&format=json&hours=24&taf=off
/// 返回最近 24 小时的 METAR 观测：
/// - 最新一条 = 实时温度
/// - 所有观测中的最高温 = 当天最高温
/// - 当天 10:00-17:00 每个整点的最近观测 = 按小时观测温度
async fn fetch_awc(
    http: &SharedHttpClient,
    station_code: &str,
    iana_tz: &str,
) -> anyhow::Result<AwcData> {
    let url = format!(
        "https://aviationweather.gov/api/data/metar?ids={}&format=json&hours=24&taf=off",
        station_code
    );

    let client = http.read().await;
    let resp: Vec<MetarResponse> = client
        .get(&url)
        .header("User-Agent", "WoolBrush/2.0")
        .send()
        .await?
        .json()
        .await?;

    if resp.is_empty() {
        return Ok(AwcData {
            current: None,
            max: None,
            hourly_temps: HashMap::new(),
            lat: None,
            lon: None,
        });
    }

    // 最新一条（数组第一个）= 实时温度
    let current = resp.first().and_then(|r| r.temp);

    // 观测站坐标（取第一条）
    let lat = resp.first().and_then(|r| r.lat);
    let lon = resp.first().and_then(|r| r.lon);

    // 时区转换：用于日期过滤
    let tz: Tz = iana_tz
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid timezone: {}", iana_tz))?;
    let now_local = Utc::now().with_timezone(&tz);
    let local_date = now_local.date_naive();

    // 当天最高温：仅从当地当天观测中取最高值
    let mut max: Option<f64> = None;
    let mut hourly_temps: HashMap<u32, f64> = HashMap::new();

    for r in &resp {
        let Some(temp) = r.temp else { continue };
        let Some(obs_ts) = r.obs_time else { continue };

        // obs_time 是 Unix timestamp，转为当地时间
        let utc_dt = DateTime::<Utc>::from_timestamp(obs_ts, 0);
        let Some(utc_dt) = utc_dt else { continue };
        let local_dt = utc_dt.with_timezone(&tz);

        // 只看当天的
        if local_dt.date_naive() != local_date {
            continue;
        }

        // 当天最高温
        max = Some(max.map_or(temp, |m| m.max(temp)));

        let hour = local_dt.hour();
        // 按小时观测：只关心 10:00-17:00
        if hour < 10 || hour > 17 {
            continue;
        }

        // 同一小时可能有多条观测，保留第一条（最早的）
        hourly_temps.entry(hour).or_insert(temp);
    }

    Ok(AwcData {
        current,
        max,
        hourly_temps,
        lat,
        lon,
    })
}

// ── Open-Meteo Forecast ──

/// Open-Meteo 响应结构
#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    hourly: OpenMeteoHourly,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoHourly {
    /// ISO 8601 时间字符串列表（如 "2026-07-31T10:00"）
    time: Vec<String>,
    /// 对应的 2m 温度列表 (°C)
    #[serde(rename = "temperature_2m")]
    temperature_2m: Vec<f64>,
}

/// 温度条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetForecast {
    /// 温度 (°C)
    pub temp: f64,
}

/// 从 Open-Meteo 获取当天 10:00-17:00 全部 8 个小时的温度
///
/// Open-Meteo 的 forecast API 自带历史实况数据，已过去的小时返回的是实况观测值，
/// 未到的小时返回的是预报值，因此无需再从 AWC 补充已过时段。
///
/// 使用代理客户端请求 Open-Meteo（直连在当前网络环境下 429 限流严重）。
///
/// 带内存 TTL 缓存（2 小时），避免每次分析都重复请求同一城市的预报数据。
/// 缓存 key 包含当地日期，跨天自动失效。
///
/// API: https://api.open-meteo.com/v1/forecast?latitude=LAT&longitude=LON&hourly=temperature_2m&timezone=IANA_TZ&forecast_days=1
async fn fetch_met(
    http: &SharedHttpClient,
    cache: &MetCache,
    lat: f64,
    lon: f64,
    iana_tz: &str,
) -> anyhow::Result<HashMap<u32, f64>> {
    // 时区与日期（用于缓存 key 和数据过滤）
    let tz: Tz = iana_tz
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid timezone: {}", iana_tz))?;
    let now_local = Utc::now().with_timezone(&tz);
    let local_date = now_local.date_naive();

    // 缓存 key 包含坐标、时区和日期，跨天自动 miss
    let cache_key = format!("{:.4},{:.4},{},{}", lat, lon, iana_tz, local_date);

    // 1) 检查缓存
    {
        let cache_read = cache.read().await;
        if let Some(cached) = cache_read.get(&cache_key) {
            if cached.fetched_at.elapsed() < MET_CACHE_TTL {
                tracing::debug!("MET cache hit for {}", cache_key);
                return Ok(cached.data.clone());
            }
        }
    }

    // 2) 缓存 miss，请求 API
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&hourly=temperature_2m&timezone={}&forecast_days=1",
        lat, lon, iana_tz
    );

    let client = http.read().await;
    let resp = client.get(&url).send().await?;
    let status = resp.status();
    let body = resp.text().await?;

    if !status.is_success() {
        anyhow::bail!("Open-Meteo HTTP {}: {}", status, &body[..body.len().min(200)]);
    }

    let parsed: OpenMeteoResponse = serde_json::from_str(&body)
        .map_err(|e| {
            tracing::warn!(
                "Open-Meteo JSON decode failed. Body preview: {}",
                &body[..body.len().min(300)]
            );
            anyhow::anyhow!("JSON decode error: {}", e)
        })?;

    let mut forecasts: HashMap<u32, f64> = HashMap::new();

    for (time_str, temp) in parsed.hourly.time.iter().zip(parsed.hourly.temperature_2m.iter()) {
        // Open-Meteo 时间格式: "2026-07-31T10:00"（无时区后缀，已是当地时间）
        let Ok(naive_dt) = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M") else {
            continue;
        };

        if naive_dt.date() != local_date {
            continue;
        }

        let hour = naive_dt.hour();
        if hour < 10 || hour > 17 {
            continue;
        }

        forecasts.entry(hour).or_insert(*temp);
    }

    // 3) 写入缓存
    {
        let mut cache_write = cache.write().await;
        cache_write.insert(cache_key, CachedMet {
            fetched_at: Instant::now(),
            data: forecasts.clone(),
        });
    }

    Ok(forecasts)
}

// ── 温度转换 ──

/// 将摄氏度转为华氏度
fn celsius_to_fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// 根据温度单位决定是否转换：°F 城市需要从 API 返回的 °C 转为 °F，保留一位小数
fn convert_temp(temp: f64, unit: &str) -> f64 {
    if unit == "\u{00B0}F" {
        let f = celsius_to_fahrenheit(temp);
        (f * 10.0).round() / 10.0
    } else {
        temp
    }
}

// ── ST (Source Temperature: weather.gov 或 wunderground.com) ──

/// ST 天气数据（温度单位与城市 unit 一致，后端已转换）
///
/// 根据 city.station_url 判断数据源：
/// - 含 "weather.gov" → aviationweather.gov METAR API (fetch_we_single)
/// - 含 "wunderground.com" → Weather Underground (fetch_wu_single)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StData {
    /// 当前温度（观测值）
    pub current: Option<f64>,
    /// 预报/观测最高温
    pub high: Option<f64>,
    /// 预报最低温（仅 WU 源有值）
    pub low: Option<f64>,
    /// 当前天气状况（如 Clear, Partly Cloudy, Rain）
    pub condition: Option<String>,
    /// 昨天最高温（仅 WE 源有值，METAR 正式观测 max）
    #[serde(rename = "yesterdayHigh")]
    pub yesterday_high: Option<f64>,
    /// 昨天最高温出现的当地时间小时（0-23）
    #[serde(rename = "yesterdayHighHour")]
    pub yesterday_high_hour: Option<u32>,
    /// 昨天天气状况（仅 WE 源有值，最新 METAR 的 cover）
    #[serde(rename = "yesterdayCondition")]
    pub yesterday_condition: Option<String>,
    /// 本地获取到数据的时间 "HH:mm:ss"
    #[serde(rename = "fetchedAt")]
    pub fetched_at: Option<String>,
}

// ── WU (Weather Underground) ──

/// WU HTML 解析用的正则（OnceLock 避免重复编译）
static WU_HIGH_RE: OnceLock<Regex> = OnceLock::new();
static WU_LOW_RE: OnceLock<Regex> = OnceLock::new();

fn wu_high_re() -> &'static Regex {
    WU_HIGH_RE.get_or_init(|| {
        Regex::new(r#"class="temp temp-high"[^>]*>\s*(\d+(?:\.\d+)?)\s*°\s*([CF])"#).unwrap()
    })
}

fn wu_low_re() -> &'static Regex {
    WU_LOW_RE.get_or_init(|| {
        Regex::new(r#"class="temp temp-low"[^>]*>\s*(\d+(?:\.\d+)?)\s*°\s*([CF])"#).unwrap()
    })
}

/// /weather/ 页面实时温度正则：第一个 wu-value-to 后 200 字符内的 °C/°F 单位
static WU_LIVE_RE: OnceLock<Regex> = OnceLock::new();

fn wu_live_re() -> &'static Regex {
    WU_LIVE_RE.get_or_init(|| {
        Regex::new(r#"(?s)wu-value wu-value-to"[^>]*>(\d+(?:\.\d+)?)</span>.{0,200}?>([CF])<"#).unwrap()
    })
}

/// history 页面天气状况正则：class="latest-condition">Mist
static WU_CONDITION_RE: OnceLock<Regex> = OnceLock::new();

fn wu_condition_re() -> &'static Regex {
    WU_CONDITION_RE.get_or_init(|| {
        Regex::new(r#"class="latest-condition"[^>]*>\s*([^<]+)"#).unwrap()
    })
}

/// 华氏度转摄氏度
fn fahrenheit_to_celsius(f: f64) -> f64 {
    (f - 32.0) * 5.0 / 9.0
}

/// WU 温度转换：根据原始单位和目标单位进行转换
///
/// `source_unit` = "C" 或 "F"（页面 HTML 中的单位标识）
/// `target_unit` = "°C" 或 "°F"（城市使用的单位）
fn convert_wu_temp(temp: f64, source_unit: &str, target_unit: &str) -> f64 {
    let is_source_f = source_unit.eq_ignore_ascii_case("F");
    let is_target_f = target_unit == "\u{00B0}F";

    if is_source_f == is_target_f {
        temp
    } else if is_source_f {
        let c = fahrenheit_to_celsius(temp);
        (c * 10.0).round() / 10.0
    } else {
        let f = celsius_to_fahrenheit(temp);
        (f * 10.0).round() / 10.0
    }
}

/// 从 WU history 页面 HTML 提取 high/low 温度和天气状况
///
/// History 页面温度单位由 `wu_units` cookie 决定（e=°F, h=°C），
/// fetch_wu_single 已按目标单位设置 cookie，正常情况下源单位与目标单位一致。
fn parse_wu_history(html: &str, target_unit: &str) -> (Option<f64>, Option<f64>, Option<String>) {
    let high = wu_high_re()
        .captures(html)
        .and_then(|c| {
            let val = c.get(1)?.as_str().parse::<f64>().ok()?;
            let unit = c.get(2)?.as_str();
            Some(convert_wu_temp(val, unit, target_unit))
        });

    let low = wu_low_re()
        .captures(html)
        .and_then(|c| {
            let val = c.get(1)?.as_str().parse::<f64>().ok()?;
            let unit = c.get(2)?.as_str();
            Some(convert_wu_temp(val, unit, target_unit))
        });

    let condition = wu_condition_re()
        .captures(html)
        .map(|c| c.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default())
        .filter(|s| !s.is_empty());

    (high, low, condition)
}

/// 从 WU /weather/ 页面 HTML 提取实时温度
///
/// /weather/ 页面不响应 wu_units cookie，始终返回 °F，
/// 因此需要用 convert_wu_temp 做单位转换。
/// 页面中第一个 `wu-value wu-value-to` 即为当前观测温度。
fn parse_wu_current(html: &str, target_unit: &str) -> Option<f64> {
    wu_live_re()
        .captures(html)
        .and_then(|c| {
            let val = c.get(1)?.as_str().parse::<f64>().ok()?;
            let unit = c.get(2)?.as_str();
            Some(convert_wu_temp(val, unit, target_unit))
        })
}

/// 请求单个站点的 WU 数据：并发抓取 /weather/（实时温度）和 /history/daily/（高低温）
///
/// - /weather/ 页面不响应 wu_units cookie，始终返回 °F，需做单位转换
/// - /history/daily/ 页面响应 wu_units cookie，按目标单位返回
async fn fetch_wu_single(
    client: &reqwest::Client,
    slug: &str,
    station_code: &str,
    unit: &str,
) -> Option<StData> {
    // wu_units=e → 英制(°F), wu_units=h → 公制(°C)
    let wu_units = if unit == "\u{00B0}F" { "e" } else { "h" };

    let weather_url = format!("https://www.wunderground.com/weather/{}", station_code);
    let history_url = format!("https://www.wunderground.com/history/daily/{}", station_code);

    let common_headers = |req: reqwest::RequestBuilder| {
        req.header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Cookie", format!("wu_units={}", wu_units))
    };

    // 并发请求两个页面
    let (weather_fut, history_fut) = (
        common_headers(client.get(&weather_url)).send(),
        common_headers(client.get(&history_url)).send(),
    );
    let (weather_resp, history_resp) = tokio::join!(weather_fut, history_fut);

    // 从 /weather/ 页面提取实时温度
    let current = match weather_resp {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(html) => parse_wu_current(&html, unit),
            Err(e) => {
                tracing::warn!(city = slug, station = station_code, error = ?e, "WU /weather/ decode failed");
                None
            }
        },
        Ok(r) => {
            tracing::warn!(city = slug, station = station_code, status = r.status().as_u16(), "WU /weather/ non-200");
            None
        }
        Err(e) => {
            tracing::warn!(city = slug, station = station_code, error = ?e, "WU /weather/ request failed");
            None
        }
    };

    // 从 /history/daily/ 页面提取 high/low/condition
    let (high, low, condition) = match history_resp {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(html) => parse_wu_history(&html, unit),
            Err(e) => {
                tracing::warn!(city = slug, station = station_code, error = ?e, "WU /history/ decode failed");
                (None, None, None)
            }
        },
        Ok(r) => {
            tracing::warn!(city = slug, station = station_code, status = r.status().as_u16(), "WU /history/ non-200");
            (None, None, None)
        }
        Err(e) => {
            tracing::warn!(city = slug, station = station_code, error = ?e, "WU /history/ request failed");
            (None, None, None)
        }
    };

    if current.is_none() && high.is_none() && low.is_none() && condition.is_none() {
        return None;
    }

    tracing::debug!(
        city = slug,
        station = station_code,
        current = ?current,
        high = ?high,
        low = ?low,
        condition = ?condition,
        "WU data parsed"
    );

    Some(StData {
        current,
        high,
        low,
        condition,
        yesterday_high: None,
        yesterday_high_hour: None,
        yesterday_condition: None,
        fetched_at: Some(now_local_str()),
    })
}

// ── WE (aviationweather.gov METAR) ──

/// aviationweather.gov 官方 METAR API（免费、无需 token）
///
/// 数据源说明：原实现使用 Synoptic Data API（weather.gov 页面同源 token），
/// 2026-08-26 该公共 token 被封（"Invalid request per token rules" 403），
/// 导致所有 WE 城市拿不到数据。现改用 aviationweather.gov：
/// - 与 AWC 同一数据源（NOAA 官方，无 token，无封禁风险）
/// - 该接口只返回 METAR 报文（metarType 区分 METAR 正式观测 / SPECI 特选报），
///   天然不含 Synoptic 数据里的分钟级瞬时尖峰，与 WRH 页面 hourly 视图口径一致
/// - temp 恒为 °C，按城市 unit 转换

/// 云量代码 → 可读描述（与 NWS 页面显示语义一致）
fn cover_to_condition(cover: Option<&str>) -> Option<String> {
    match cover {
        Some("CLR") | Some("SKC") => Some("clear".to_string()),
        Some("FEW") => Some("few clouds".to_string()),
        Some("SCT") => Some("scattered".to_string()),
        Some("BKN") => Some("broken".to_string()),
        Some("OVC") => Some("overcast".to_string()),
        _ => None,
    }
}

/// WE 当天最高温（°C）：该站本地日期内 METAR 正式观测的温度最大值
///
/// 口径与 weather.gov timeseries 页面 hourly 视图一致：
/// - 只计 METAR 正式观测（metarType != SPECI）
/// - aviationweather 无 Synoptic 那种分钟级瞬时尖峰，不存在尖峰误计问题
/// - 前端 Math.round 取整显示（如 78.98°F → 79）
///
/// "当天" = 参数 now 的站点本地日期（生产传 Utc::now()，与 AWC/MET 口径一致，
/// 跨日立即进入新一天）；now 参数化以便测试固定时间点
fn we_day_high(records: &[MetarResponse], tz: &Tz, now: DateTime<Utc>) -> Option<f64> {
    let local_date = now.with_timezone(tz).date_naive();
    we_date_high_with_hour(records, tz, local_date).map(|(temp, _)| temp)
}

/// 指定日期的 METAR 最高温（°C）：该站本地指定日期内 METAR 正式观测的温度最大值
///
/// 与 we_day_high 同口径，仅日期由参数指定而非固定当天。
/// 用于获取昨天最高温（配合 hours=48 的 METAR 请求）。
/// 返回 (最高温°C, 出现的当地时间小时)
fn we_date_high_with_hour(records: &[MetarResponse], tz: &Tz, target_date: NaiveDate) -> Option<(f64, u32)> {
    // 收集目标日期、非 SPECI、且当地 11:00 之后的观测
    let mut candidates: Vec<(f64, u32)> = Vec::new();
    for r in records {
        let Some(temp) = r.temp else { continue };
        let Some(obs_ts) = r.obs_time else { continue };
        let utc_dt = match DateTime::<Utc>::from_timestamp(obs_ts, 0) {
            Some(dt) => dt,
            None => continue,
        };
        let local_dt = utc_dt.with_timezone(tz);
        if local_dt.date_naive() != target_date {
            continue;
        }
        // SPECI 特选报不参与最高温（与页面 hourly 视图一致）
        if r.metar_type.as_deref() == Some("SPECI") {
            continue;
        }
        // 只取 11:00 之后的观测（最高温通常出现在午后）
        if local_dt.hour() < 11 {
            continue;
        }
        candidates.push((temp, local_dt.hour()));
    }

    // 找到最高温，再取该温度第一次出现的小时（最早）
    let max_temp = candidates.iter().map(|(t, _)| *t).reduce(f64::max)?;
    let first_hour = candidates
        .iter()
        .filter(|(t, _)| (*t - max_temp).abs() < 0.01)
        .map(|(_, h)| *h)
        .min()?;

    Some((max_temp, first_hour))
}

/// 指定日期的天气状况：取该日期最新一条 METAR 记录的 cover 转换结果
///
/// records 须按 obs_time 降序（最新在前）。
fn we_date_condition(records: &[MetarResponse], tz: &Tz, target_date: NaiveDate) -> Option<String> {
    for r in records {
        let Some(obs_ts) = r.obs_time else { continue };
        let Some(utc_dt) = DateTime::<Utc>::from_timestamp(obs_ts, 0) else { continue };
        let local_dt = utc_dt.with_timezone(tz);
        if local_dt.date_naive() != target_date {
            continue;
        }
        if let Some(cond) = cover_to_condition(r.cover.as_deref()) {
            return Some(cond);
        }
    }
    None
}

/// 从 aviationweather.gov 获取单个站点的 ST 数据（当前温度 + 当天最高温 + 云况）
///
/// API: `https://aviationweather.gov/api/data/metar?ids=<STID>&format=json&hours=48&taf=off`
/// - hours=48 → 最近 48 小时 METAR 观测（覆盖昨天全部定时观测）
/// - 最近一条观测 = 当前温度
/// - 当天最高温 = we_day_high（METAR 正式观测 max，°C → 按 unit 转换）
/// - 昨天最高温 = we_date_high（昨天 METAR 正式观测 max）
async fn fetch_we_single(
    client: &reqwest::Client,
    slug: &str,
    station_code: &str,
    unit: &str,
    iana_tz: &str,
) -> Option<StData> {
    let icao = station_code.to_uppercase();
    let url = format!(
        "https://aviationweather.gov/api/data/metar?ids={}&format=json&hours=48&taf=off",
        icao
    );

    let resp = client
        .get(&url)
        .header("User-Agent", "WoolBrush/2.0")
        .send()
        .await;

    let data: Vec<MetarResponse> = match resp {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(city = slug, station = station_code, error = ?e, "WE parse failed");
                return None;
            }
        },
        Ok(r) => {
            tracing::warn!(city = slug, station = station_code, status = r.status().as_u16(), "WE non-200");
            return None;
        }
        Err(e) => {
            tracing::warn!(city = slug, station = station_code, error = ?e, "WE request failed");
            return None;
        }
    };

    let tz: Tz = match iana_tz.parse() {
        Ok(tz) => tz,
        Err(_) => {
            tracing::warn!(city = slug, tz = iana_tz, "WE invalid timezone");
            return None;
        }
    };

    if data.is_empty() {
        tracing::warn!(city = slug, station = station_code, "WE returned no observations");
        return None;
    }

    // 当前温度 = 第一条观测（aviationweather 按时间降序返回，最新在前）
    let current = data.first().and_then(|r| r.temp);
    // 当天最高温（°C）
    let high_c = we_day_high(&data, &tz, Utc::now());
    // 昨天最高温（°C）—— hours=48 覆盖昨天全部观测
    let yesterday_date = (Utc::now() - chrono::Duration::days(1))
        .with_timezone(&tz)
        .date_naive();
    let yesterday_high_c = we_date_high_with_hour(&data, &tz, yesterday_date);
    let (yesterday_high_c, yesterday_high_hour) = match yesterday_high_c {
        Some((temp, hour)) => (Some(temp), Some(hour)),
        None => (None, None),
    };
    // 昨天天气状况（最新 METAR 的 cover）
    let yesterday_cond = we_date_condition(&data, &tz, yesterday_date);
    // 云况（第一条，最新）
    let condition = data
        .iter()
        .find_map(|r| cover_to_condition(r.cover.as_deref()));

    let current = current.map(|t| convert_temp(t, unit));
    let high = high_c.map(|t| convert_temp(t, unit));
    let yesterday_high = yesterday_high_c.map(|t| convert_temp(t, unit));

    if current.is_none() && high.is_none() && condition.is_none() {
        return None;
    }

    tracing::debug!(
        city = slug,
        station = station_code,
        current = ?current,
        high = ?high,
        condition = ?condition,
        "WE data parsed"
    );

    Some(StData {
        current,
        high,
        low: None,
        condition,
        yesterday_high,
        yesterday_high_hour,
        yesterday_condition: yesterday_cond,
        fetched_at: Some(now_local_str()),
    })
}

// ── 公开接口 ──

/// 城市天气数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CityWeather {
    pub city: String,
    pub awc: Option<AwcData>,
    pub met_forecast: Vec<MetForecast>,
    /// 源温度：根据 station_url 从 weather.gov 或 wunderground.com 获取
    pub st: Option<StData>,
}

/// 获取单个城市的天气数据
///
/// - AWC: 用 station_code 查询 METAR 实况温度（实时 + 当天最高）
/// - Open-Meteo: 用坐标查询当天 10:00-17:00 全部 8 个小时的温度
///   （已过去的返回实况观测值，未到的返回预报值）
/// - 单位转换：API 原始返回为 °C，根据 unit 参数决定是否转为 °F
pub async fn fetch_city_weather(
    http: &SharedHttpClient,
    met_cache: &MetCache,
    city_slug: &str,
    station_code: Option<&str>,
    station_url: Option<&str>,
    lat: Option<f64>,
    lon: Option<f64>,
    iana_tz: &str,
    unit: &str,
) -> CityWeather {
    // AWC（需要有 station code）——仅用于实时温度和当天最高温
    let awc = if let Some(code) = station_code {
        match fetch_awc(http, code, iana_tz).await {
            Ok(data) => Some(data),
            Err(e) => {
                tracing::warn!("AWC fetch failed for {} ({}): {}", city_slug, code, e);
                None
            }
        }
    } else {
        None
    };

    // ST（源温度）——根据 station_url 判断数据源
    // aviationweather.gov → METAR API；wunderground.com → WU history 页面
    let st = if let Some(code) = station_code {
        let client = http.read().await.clone();
        let url = station_url.unwrap_or("");
        if url.contains("wunderground.com") {
            fetch_wu_single(&client, city_slug, code, unit).await
        } else {
            // 默认使用 aviationweather.gov METAR API
            fetch_we_single(&client, city_slug, code, unit, iana_tz).await
        }
    } else {
        None
    };

    // 坐标优先用数据库中的，如果数据库没有则从 AWC 返回中取
    let effective_lat = lat.or_else(|| awc.as_ref().and_then(|a| a.lat));
    let effective_lon = lon.or_else(|| awc.as_ref().and_then(|a| a.lon));

    // Open-Meteo（10:00-17:00 全部 8 个小时）——使用代理客户端 + TTL 缓存
    let met_all = match (effective_lat, effective_lon) {
        (Some(la), Some(lo)) => match fetch_met(http, met_cache, la, lo, iana_tz).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("Open-Meteo fetch failed for {}: {}", city_slug, e);
                HashMap::new()
            }
        },
        _ => {
            tracing::warn!("No coordinates for city: {}", city_slug);
            HashMap::new()
        }
    };

    // 根据城市温度单位转换 AWC 数据
    let awc = awc.map(|mut a| {
        a.current = a.current.map(|t| convert_temp(t, unit));
        a.max = a.max.map(|t| convert_temp(t, unit));
        a.hourly_temps = a
            .hourly_temps
            .iter()
            .map(|(h, &t)| (*h, convert_temp(t, unit)))
            .collect();
        a
    });

    // 10:00-17:00 全部使用 Open-Meteo 数据
    let mut merged: Vec<MetForecast> = Vec::new();
    for hour in 10..=17u32 {
        if let Some(&temp) = met_all.get(&hour) {
            merged.push(MetForecast { temp: convert_temp(temp, unit) });
        }
    }

    CityWeather {
        city: city_slug.to_string(),
        awc,
        met_forecast: merged,
        st,
    }
}

// ── 批量获取 ──

/// 批量信息：城市 + station_code + iana_tz + unit + lat + lon + station_url
#[derive(Clone)]
struct CityInfo<'a> {
    slug: &'a str,
    station_code: &'a str,
    station_url: &'a str,
    iana_tz: &'a str,
    unit: &'a str,
    lat: Option<f64>,
    lon: Option<f64>,
}

/// 批量获取 AWC METAR 数据
///
/// 一次请求获取所有站点的 24h METAR 观测，按 icaoId 分组后逐城市处理。
/// API: `https://aviationweather.gov/api/data/metar?ids=KDAL,KLAX,...&format=json&hours=24&taf=off`
async fn fetch_awc_batch(
    http: &SharedHttpClient,
    cities: &[CityInfo<'_>],
) -> HashMap<String, AwcData> {
    let mut result: HashMap<String, AwcData> = HashMap::new();
    if cities.is_empty() {
        return result;
    }

    // 拼接所有 station_code
    let ids: Vec<&str> = cities.iter().map(|c| c.station_code).collect();
    let url = format!(
        "https://aviationweather.gov/api/data/metar?ids={}&format=json&hours=24&taf=off",
        ids.join(",")
    );

    let client = http.read().await;
    let resp: Vec<MetarResponse> = match client
        .get(&url)
        .header("User-Agent", "WoolBrush/2.0")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!(error = ?e, "AWC batch parse failed");
                return result;
            }
        },
        Ok(r) => {
            tracing::warn!(status = r.status().as_u16(), "AWC batch non-200");
            return result;
        }
        Err(e) => {
            tracing::warn!(error = ?e, "AWC batch request failed");
            return result;
        }
    };

    // 按 icaoId 分组
    let mut by_station: HashMap<&str, Vec<&MetarResponse>> = HashMap::new();
    for r in &resp {
        if let Some(ref icao) = r.icao_id {
            by_station.entry(icao.as_str()).or_default().push(r);
        }
    }

    // 逐城市处理（纯内存操作，无网络 IO）
    for city in cities {
        let stid_upper = city.station_code.to_uppercase();
        let records = match by_station.get(stid_upper.as_str()) {
            Some(rs) if !rs.is_empty() => rs,
            _ => {
                tracing::warn!(city = city.slug, station = city.station_code, "AWC batch: no data for station");
                continue;
            }
        };

        let tz: Tz = match city.iana_tz.parse() {
            Ok(tz) => tz,
            Err(_) => {
                tracing::warn!(city = city.slug, tz = city.iana_tz, "AWC batch: invalid timezone");
                continue;
            }
        };

        let now_local = Utc::now().with_timezone(&tz);
        let local_date = now_local.date_naive();

        let current = records.first().and_then(|r| r.temp);
        let lat = records.first().and_then(|r| r.lat);
        let lon = records.first().and_then(|r| r.lon);

        let mut max: Option<f64> = None;
        let mut hourly_temps: HashMap<u32, f64> = HashMap::new();

        for r in records {
            let Some(temp) = r.temp else { continue };
            let Some(obs_ts) = r.obs_time else { continue };

            let utc_dt = match DateTime::<Utc>::from_timestamp(obs_ts, 0) {
                Some(dt) => dt,
                None => continue,
            };
            let local_dt = utc_dt.with_timezone(&tz);

            if local_dt.date_naive() != local_date {
                continue;
            }

            max = Some(max.map_or(temp, |m| m.max(temp)));

            let hour = local_dt.hour();
            if hour < 10 || hour > 17 {
                continue;
            }
            hourly_temps.entry(hour).or_insert(temp);
        }

        // 单位转换
        let unit = city.unit;
        result.insert(
            city.slug.to_string(),
            AwcData {
                current: current.map(|t| convert_temp(t, unit)),
                max: max.map(|t| convert_temp(t, unit)),
                hourly_temps: hourly_temps
                    .iter()
                    .map(|(h, &t)| (*h, convert_temp(t, unit)))
                    .collect(),
                lat,
                lon,
            },
        );
    }

    tracing::info!(cities = cities.len(), got = result.len(), "AWC batch done");
    result
}

/// 批量获取 ST (aviationweather.gov METAR) 数据
///
/// API 对单个请求有 400 条记录硬上限（实测 47 站仅返回 400 条，每站只剩 5-13 条，
/// 会丢掉当天较早时段的高温观测导致最高温偏低）。因此按 7 站/组分片请求
/// （hours=48 每站约 48-52 条，7 站实测约 350 条，安全在 400 条上限内），
/// 合并后各站按 obs_time 降序取最新一条为当前温度。
/// 与单站 fetch_we_single 同一数据源与口径（we_day_high / cover_to_condition）。
/// API: `https://aviationweather.gov/api/data/metar?ids=KDAL,KLAX,...&format=json&hours=24&taf=off`
async fn fetch_we_batch(
    http: &SharedHttpClient,
    cities: &[CityInfo<'_>],
) -> HashMap<String, StData> {
    let mut result: HashMap<String, StData> = HashMap::new();
    if cities.is_empty() {
        return result;
    }

    let client = http.read().await.clone();

    // aviationweather 单请求 400 条硬上限；hours=48 每站约 48-52 条，按 7 站/组分片（7×50≈350，安全）
    const CHUNK_SIZE: usize = 7;
    let mut futures = Vec::new();
    for chunk in cities.chunks(CHUNK_SIZE) {
        let client = client.clone();
        let ids: Vec<&str> = chunk.iter().map(|c| c.station_code).collect();
        futures.push(async move {
            let url = format!(
                "https://aviationweather.gov/api/data/metar?ids={}&format=json&hours=48&taf=off",
                ids.join(",")
            );
            let resp: Vec<MetarResponse> = match client
                .get(&url)
                .header("User-Agent", "WoolBrush/2.0")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => match r.json().await {
                    Ok(data) => data,
                    Err(e) => {
                        tracing::warn!(error = ?e, "WE batch parse failed");
                        return Vec::new();
                    }
                },
                Ok(r) => {
                    tracing::warn!(status = r.status().as_u16(), "WE batch non-200");
                    return Vec::new();
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "WE batch request failed");
                    return Vec::new();
                }
            };
            resp
        });
    }

    // 并发请求所有分片，按 icaoId 合并
    let batches = futures_util::future::join_all(futures).await;
    let mut by_station: HashMap<&str, Vec<&MetarResponse>> = HashMap::new();
    for resp in batches.iter().flatten() {
        if let Some(ref icao) = resp.icao_id {
            by_station.entry(icao.as_str()).or_default().push(resp);
        }
    }

    // 逐城市处理（纯内存操作，无网络 IO）
    for city in cities {
        let stid_upper = city.station_code.to_uppercase();
        let records = match by_station.get(stid_upper.as_str()) {
            Some(rs) if !rs.is_empty() => rs,
            _ => {
                tracing::warn!(city = city.slug, station = city.station_code, "WE batch: no data for station");
                continue;
            }
        };

        // 按 obs_time 降序（最新在前），确保 current 取到最新一条
        let mut sorted: Vec<&MetarResponse> = records.clone();
        sorted.sort_by_key(|r| r.obs_time.unwrap_or(0));
        sorted.reverse();

        let tz: Tz = match city.iana_tz.parse() {
            Ok(tz) => tz,
            Err(_) => {
                tracing::warn!(city = city.slug, tz = city.iana_tz, "WE batch: invalid timezone");
                continue;
            }
        };

        // 当前温度 = 第一条观测（最新）
        let current = sorted.first().and_then(|r| r.temp);
        // 当天最高温 (°C) —— 克隆为 owned 复用 we_day_high（与单站口径一致）
        let owned: Vec<MetarResponse> = sorted.iter().map(|r| (*r).clone()).collect();
        let high = we_day_high(&owned, &tz, Utc::now());
        // 昨天最高温 + 天气状况
        let yesterday_date = (Utc::now() - chrono::Duration::days(1))
            .with_timezone(&tz)
            .date_naive();
        let yesterday_high = we_date_high_with_hour(&owned, &tz, yesterday_date);
        let (yesterday_high, yesterday_high_hour) = match yesterday_high {
            Some((temp, hour)) => (Some(temp), Some(hour)),
            None => (None, None),
        };
        let yesterday_condition = we_date_condition(&owned, &tz, yesterday_date);
        // 云况（最新一条）
        let condition = sorted
            .iter()
            .find_map(|r| cover_to_condition(r.cover.as_deref()));

        if current.is_none() && high.is_none() && condition.is_none() {
            continue;
        }

        let unit = city.unit;
        result.insert(
            city.slug.to_string(),
            StData {
                current: current.map(|t| convert_temp(t, unit)),
                high: high.map(|t| convert_temp(t, unit)),
                low: None,
                condition,
                yesterday_high: yesterday_high.map(|t| convert_temp(t, unit)),
                yesterday_high_hour,
                yesterday_condition,
                fetched_at: Some(now_local_str()),
            },
        );
    }

    tracing::info!(cities = cities.len(), got = result.len(), "WE batch done");
    result
}

/// 批量获取 MET (Open-Meteo) 预报
///
/// 按时区分组，每组一次请求。先查缓存，仅对 cache miss 的城市批量请求。
async fn fetch_met_batch(
    http: &SharedHttpClient,
    cache: &MetCache,
    cities: &[CityInfo<'_>],
) -> HashMap<String, Vec<MetForecast>> {
    let mut result: HashMap<String, Vec<MetForecast>> = HashMap::new();
    if cities.is_empty() {
        return result;
    }

    // 1) 查缓存，分离 hit / miss
    let mut miss: HashMap<&str, Vec<&CityInfo>> = HashMap::new(); // key = iana_tz
    for city in cities {
        let (Some(la), Some(lo)) = (city.lat, city.lon) else {
            tracing::warn!(city = city.slug, lat = ?city.lat, lon = ?city.lon, "MET batch: no coordinates, skipping");
            continue;
        };
        let tz: Tz = match city.iana_tz.parse() {
            Ok(tz) => tz,
            Err(_) => continue,
        };
        let now_local = Utc::now().with_timezone(&tz);
        let local_date = now_local.date_naive();
        let cache_key = format!("{:.4},{:.4},{},{}", la, lo, city.iana_tz, local_date);

        let cached = {
            let cr = cache.read().await;
            cr.get(&cache_key).and_then(|c| {
                if c.fetched_at.elapsed() < MET_CACHE_TTL {
                    Some(c.data.clone())
                } else {
                    None
                }
            })
        };

        if let Some(data) = cached {
            let merged: Vec<MetForecast> = (10..=17u32)
                .filter_map(|h| data.get(&h).map(|&t| MetForecast { temp: convert_temp(t, city.unit) }))
                .collect();
            result.insert(city.slug.to_string(), merged);
        } else {
            miss.entry(city.iana_tz).or_default().push(city);
        }
    }

    if miss.is_empty() {
        tracing::info!("MET batch: all cache hits");
        return result;
    }

    // 2) 对每个时区组的 cache miss 批量请求
    let client = http.read().await.clone();

    let futures: Vec<_> = miss
        .into_iter()
        .map(|(tz_str, group)| {
            let client = client.clone();
            let cache = Arc::clone(cache);
            async move {
                let lats: Vec<String> = group.iter().filter_map(|c| c.lat.map(|l| format!("{:.4}", l))).collect();
                let lons: Vec<String> = group.iter().filter_map(|c| c.lon.map(|l| format!("{:.4}", l))).collect();
                let url = format!(
                    "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&hourly=temperature_2m&timezone={}&forecast_days=1",
                    lats.join(","),
                    lons.join(","),
                    tz_str
                );

                let resp = match client.get(&url).send().await {
                    Ok(r) if r.status().is_success() => r,
                    Ok(r) => {
                        tracing::warn!(status = r.status().as_u16(), tz = tz_str, "MET batch non-200");
                        return vec![];
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, tz = tz_str, "MET batch request failed");
                        return vec![];
                    }
                };

                let body = resp.text().await.unwrap_or_default();

                // Open-Meteo 多位置返回数组，单位置返回对象
                let parsed: Vec<OpenMeteoResponse> = if body.trim_start().starts_with('[') {
                    serde_json::from_str(&body).unwrap_or_default()
                } else {
                    serde_json::from_str(&body).map(|r| vec![r]).unwrap_or_default()
                };

                let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
                let now_local = Utc::now().with_timezone(&tz);
                let local_date = now_local.date_naive();

                let mut batch_results: Vec<(String, HashMap<u32, f64>, String)> = Vec::new();

                for (i, city) in group.iter().enumerate() {
                    let Some(loc) = parsed.get(i) else { continue };
                    let mut forecasts: HashMap<u32, f64> = HashMap::new();

                    for (time_str, temp) in loc.hourly.time.iter().zip(loc.hourly.temperature_2m.iter()) {
                        let Ok(naive_dt) = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M") else { continue };
                        if naive_dt.date() != local_date { continue; }
                        let hour = naive_dt.hour();
                        if hour < 10 || hour > 17 { continue; }
                        forecasts.entry(hour).or_insert(*temp);
                    }

                    // 写入缓存
                    let cache_key = format!(
                        "{:.4},{:.4},{},{}",
                        city.lat.unwrap_or(0.0),
                        city.lon.unwrap_or(0.0),
                        tz_str,
                        local_date
                    );
                    {
                        let mut cw = cache.write().await;
                        cw.insert(cache_key, CachedMet {
                            fetched_at: Instant::now(),
                            data: forecasts.clone(),
                        });
                    }

                    batch_results.push((city.slug.to_string(), forecasts, city.unit.to_string()));
                }
                batch_results
            }
        })
        .collect();

    let results_batch = futures_util::future::join_all(futures).await;

    for batch in &results_batch {
        for (slug, data, unit) in batch {
            let merged: Vec<MetForecast> = (10..=17u32)
                .filter_map(|h| data.get(&h).map(|&t| MetForecast { temp: convert_temp(t, unit) }))
                .collect();
            result.insert(slug.clone(), merged);
        }
    }

    tracing::info!(cities = cities.len(), got = result.len(), "MET batch done");
    result
}

/// 批量获取天气数据的统一入口
///
/// 并行获取 AWC / ST / MET 三组数据，组装为 Vec<CityWeather>。
/// - AWC: 一次请求所有站点
/// - ST (非 wunderground): aviationweather.gov 一次批量请求
/// - ST (wunderground.com): 逐站请求（通常仅 2 个城市）
/// - MET: 按时区分组批量请求（含缓存）
pub async fn fetch_weather_batch(
    http: &SharedHttpClient,
    met_cache: &MetCache,
    cities: &[crate::infrastructure::db::CityRow],
) -> Vec<CityWeather> {
    if cities.is_empty() {
        return Vec::new();
    }

    let t0 = std::time::Instant::now();

    // 构建城市信息列表
    let infos: Vec<CityInfo> = cities
        .iter()
        .filter_map(|c| {
            let code = c.station_code.as_deref()?;
            let url = c.station_url.as_deref().unwrap_or("");
            Some(CityInfo {
                slug: &c.slug,
                station_code: code,
                station_url: url,
                iana_tz: &c.iana_tz,
                unit: &c.unit,
                lat: c.lat,
                lon: c.lon,
            })
        })
        .collect();

    // 分组：weather.gov 城市 vs wunderground 城市
    let gov_cities: Vec<CityInfo> = infos.iter().filter(|c| !c.station_url.contains("wunderground.com")).cloned().collect();
    let wu_cities: Vec<&CityInfo> = infos.iter().filter(|c| c.station_url.contains("wunderground.com")).collect();

    // AWC: 所有城市一次批量
    let awc_infos: Vec<CityInfo> = infos.clone();

    // 全并行：AWC + WE + MET + WU 同时发起
    // 缺坐标的城市（lat/lon 为 None）在 fetch_met_batch 内部会被跳过
    let awc_fut = fetch_awc_batch(http, &awc_infos);
    let we_fut = fetch_we_batch(http, &gov_cities);
    let met_fut = fetch_met_batch(http, met_cache, &awc_infos);

    // WU 逐站
    let wu_client = http.read().await.clone();
    let wu_futs: Vec<_> = wu_cities
        .iter()
        .map(|c| {
            let client = wu_client.clone();
            let slug = c.slug.to_string();
            let code = c.station_code.to_string();
            let unit = c.unit.to_string();
            async move {
                let st = fetch_wu_single(&client, &slug, &code, &unit).await;
                (slug, st)
            }
        })
        .collect();

    let (awc_map, we_map, met_map, wu_results) = tokio::join!(
        awc_fut,
        we_fut,
        met_fut,
        async {
            let mut m = HashMap::new();
            for f in wu_futs {
                let (slug, st) = f.await;
                if let Some(st) = st {
                    m.insert(slug, st);
                }
            }
            m
        }
    );

    let results = assemble_results(cities, &awc_map, &we_map, &met_map, &wu_results);
    tracing::info!(
        total = results.len(),
        elapsed_ms = t0.elapsed().as_millis(),
        "fetch_weather_batch done"
    );
    results
}

/// 组装最终结果
fn assemble_results(
    cities: &[crate::infrastructure::db::CityRow],
    awc_map: &HashMap<String, AwcData>,
    we_map: &HashMap<String, StData>,
    met_map: &HashMap<String, Vec<MetForecast>>,
    wu_results: &HashMap<String, StData>,
) -> Vec<CityWeather> {
    cities
        .iter()
        .map(|city| {
            let awc = awc_map.get(&city.slug).cloned();
            let st = we_map.get(&city.slug)
                .cloned()
                .or_else(|| wu_results.get(&city.slug).cloned());
            let met_forecast = met_map.get(&city.slug).cloned().unwrap_or_default();
            CityWeather {
                city: city.slug.clone(),
                awc,
                met_forecast,
                st,
            }
        })
        .collect()
}

/// 补充城市坐标：对所有 lat/lon 为 None 的城市，通过 AWC METAR API 获取观测站坐标并返回。
///
/// 调用方（如 `update_cities`）拿到结果后写入 DB。
/// 仅需 station_code，一次请求获取所有缺失坐标的城市。
pub async fn fetch_missing_coords(
    http: &SharedHttpClient,
    cities: &[crate::infrastructure::db::CityRow],
) -> Vec<(String, f64, f64)> {
    // 筛选缺失坐标的城市
    let need_coords: Vec<CityInfo> = cities
        .iter()
        .filter(|c| c.lat.is_none() || c.lon.is_none())
        .filter_map(|c| {
            let code = c.station_code.as_deref()?;
            Some(CityInfo {
                slug: &c.slug,
                station_code: code,
                station_url: c.station_url.as_deref().unwrap_or(""),
                iana_tz: &c.iana_tz,
                unit: &c.unit,
                lat: c.lat,
                lon: c.lon,
            })
        })
        .collect();

    if need_coords.is_empty() {
        return Vec::new();
    }

    tracing::info!(
        cities = need_coords.len(),
        "Fetching missing coordinates from AWC"
    );

    let awc_map = fetch_awc_batch(http, &need_coords).await;

    let coords: Vec<(String, f64, f64)> = need_coords
        .iter()
        .filter_map(|c| {
            let awc = awc_map.get(c.slug)?;
            let lat = awc.lat?;
            let lon = awc.lon?;
            Some((c.slug.to_string(), lat, lon))
        })
        .collect();

    tracing::info!(
        requested = need_coords.len(),
        obtained = coords.len(),
        "Missing coordinates fetched"
    );

    coords
}

#[cfg(test)]
mod tests {
    use super::{we_day_high, MetarResponse};
    use chrono::{TimeZone, Utc};
    use chrono_tz::Tz;

    fn metar(obs_ts: Option<i64>, temp: Option<f64>, mtype: &str) -> MetarResponse {
        MetarResponse {
            icao_id: None,
            temp,
            obs_time: obs_ts,
            lat: None,
            lon: None,
            metar_type: Some(mtype.to_string()),
            cover: None,
        }
    }

    fn utc_ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap().timestamp()
    }

    #[test]
    fn we_high_keeps_metar_only() {
        // aviationweather 数据源：SPECI 特选报（对应旧 Synoptic 无 slp 的瞬时尖峰）
        // 不参与当天最高温。KLGA 2026-08-25 语义：16:55 尖峰 27.0°C 为 SPECI → 排除
        let tz: Tz = "America/New_York".parse().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 25, 23, 0, 0).unwrap();

        let records = vec![
            metar(Some(utc_ts(2026, 8, 25, 20, 51)), Some(26.1), "METAR"), // 16:51 METAR
            metar(Some(utc_ts(2026, 8, 25, 20, 55)), Some(27.0), "SPECI"), // 16:55 尖峰 -> 排除
            metar(Some(utc_ts(2026, 8, 25, 21, 51)), Some(26.2), "METAR"), // 17:51 METAR
            metar(Some(utc_ts(2026, 8, 26, 1, 51)), Some(24.4), "METAR"),  // 21:51 本地 METAR
        ];
        // METAR {26.1, 26.2, 24.4} -> max = 26.2（27.0 是 SPECI 尖峰不参与）
        assert_eq!(we_day_high(&records, &tz, now), Some(26.2));
    }

    #[test]
    fn we_high_filters_previous_day() {
        // 昨日观测不参与当天最高温计算
        let tz: Tz = "America/New_York".parse().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 25, 23, 0, 0).unwrap();

        let records = vec![
            metar(Some(utc_ts(2026, 8, 25, 3, 51)), Some(32.2), "METAR"),  // 昨日 23:51 EDT METAR
            metar(Some(utc_ts(2026, 8, 25, 13, 55)), Some(21.1), "SPECI"), // 今天 09:55 SPECI -> 排除
            metar(Some(utc_ts(2026, 8, 25, 20, 51)), Some(26.1), "METAR"), // 今天 16:51 METAR
            metar(Some(utc_ts(2026, 8, 25, 21, 51)), Some(26.3), "METAR"), // 今天 17:51 METAR
        ];
        // 今日 METAR {26.1, 26.3} -> max = 26.3
        assert_eq!(we_day_high(&records, &tz, now), Some(26.3));
    }

    #[test]
    fn we_high_returns_none_when_no_timestamp() {
        // obs_time 全部缺失 -> 无法确定观测所属日期，返回 None
        let tz: Tz = "America/New_York".parse().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 25, 23, 0, 0).unwrap();

        let records = vec![
            metar(None, Some(26.1), "METAR"),
            metar(None, Some(27.0), "METAR"),
        ];
        assert_eq!(we_day_high(&records, &tz, now), None);
    }

    #[test]
    fn we_high_returns_none_when_all_speci() {
        // 全部 SPECI（瞬时特选报）-> 当天最高温为空
        let tz: Tz = "America/New_York".parse().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 25, 23, 0, 0).unwrap();

        let records = vec![
            metar(Some(utc_ts(2026, 8, 25, 20, 55)), Some(27.0), "SPECI"),
            metar(Some(utc_ts(2026, 8, 25, 21, 0)), Some(26.4), "SPECI"),
        ];
        assert_eq!(we_day_high(&records, &tz, now), None);
    }
}

