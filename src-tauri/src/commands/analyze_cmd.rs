use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::settings_cmd::SettingsForm;
use crate::state::AppState;

// ── LLM Provider 常量 ──

// 百度千帆 V2 API: Bearer 认证，OpenAI 兼容格式
const QIANFAN_CHAT_URL: &str = "https://qianfan.baidubce.com/v2/chat/completions";
const QIANFAN_MODEL: &str = "ernie-4.0-8k-latest";

// 阿里百炼 DashScope 兼容模式: Bearer 认证，OpenAI 兼容格式
const BAILIAN_CHAT_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";
const BAILIAN_DEFAULT_MODEL: &str = "qwen-plus";

// Ollama Cloud API: Bearer 认证，OpenAI 兼容格式
const OLLAMA_DEFAULT_URL: &str = "https://ollama.com/v1/chat/completions";
// 本机自建 Ollama：无需 API Key，OpenAI 兼容端点与云端同格式
const OLLAMA_LOCAL_URL: &str = "http://localhost:11434/v1/chat/completions";
const OLLAMA_DEFAULT_MODEL: &str = "qwen2.5:14b";

/// 判断是否为本机/内网地址。本机 Ollama 不能走代理，也不需要 Bearer 认证。
fn is_local_url(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, p)| {
        // 只有末段全是数字才是端口，避免把 IPv6 地址里的冒号误当端口
        if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) { h } else { host }
    });
    let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();

    if host == "localhost" || host == "::1" || host.ends_with(".local") {
        return true;
    }
    let octets: Vec<&str> = host.split('.').collect();
    if octets.len() == 4 && octets.iter().all(|o| o.parse::<u8>().is_ok()) {
        let a: u8 = octets[0].parse().unwrap();
        let b: u8 = octets[1].parse().unwrap();
        return a == 127 || a == 10 || (a == 192 && b == 168) || (a == 172 && (16..=31).contains(&b));
    }
    false
}

// ── Request types (from frontend) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdData {
    pub label: String,
    pub yes_price: f64,
    pub no_price: f64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub mid: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwcDataInput {
    pub current: Option<f64>,
    pub max: Option<f64>,
    pub hourly_temps: Option<std::collections::HashMap<u32, f64>>,
}

/// ST 源温度数据（weather.gov 或 wunderground.com）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StDataInput {
    /// 当前实时温度
    pub current: Option<f64>,
    /// 当天最高温度
    pub high: Option<f64>,
    /// 当前天气状况
    pub condition: Option<String>,
    /// 昨天最高温度
    #[serde(rename = "yesterdayHigh")]
    pub yesterday_high: Option<f64>,
    /// 昨天最高温出现的当地时间小时（0-23）
    #[serde(rename = "yesterdayHighHour")]
    pub yesterday_high_hour: Option<u32>,
    /// 昨天天气状况
    #[serde(rename = "yesterdayCondition")]
    pub yesterday_condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetForecastInput {
    pub temp: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeCityRequest {
    pub city: String,
    pub city_tz: String,
    pub local_time: String,
    pub unit: String,
    pub bid_ask_min: f64,
    pub bid_ask_max: f64,
    pub thresholds: Vec<ThresholdData>,
    pub awc: Option<AwcDataInput>,
    /// ST 源温度（weather.gov / wunderground.com），判定市场温度档位胜负的标准温度
    pub st: Option<StDataInput>,
    pub met_forecast: Vec<MetForecastInput>,
    /// 当前城市已持仓的档位标签列表（无需再分析这些档位）
    #[serde(default)]
    pub held_thresholds: Vec<String>,
}

// ── Response types (to frontend) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeCityResponse {
    pub threshold_label: String,
    pub side: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeCityResult {
    /// 推荐的开仓列表（可包含多个档位）
    pub actions: Vec<AnalyzeCityResponse>,
    /// 总体分析理由（当 actions 为空时说明原因）
    pub summary: String,
}

// ── LLM API types (OpenAI-compatible, shared by Qianfan & Bailian) ──

#[derive(Debug, Serialize)]
struct LlmMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct LlmChatRequest {
    model: String,
    messages: Vec<LlmMessage>,
    temperature: f32,
    top_p: f32,
}

#[derive(Debug, Deserialize)]
struct LlmChoice {
    message: LlmChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct LlmChoiceMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct LlmChatResponse {
    choices: Vec<LlmChoice>,
}

// ── LLM test result (to frontend) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyTestResult {
    /// Key 序号（1-based）
    pub key_index: usize,
    /// Key 前缀（前 8 字符，用于识别）
    pub key_prefix: String,
    pub success: bool,
    pub message: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestLlmResult {
    pub success: bool,
    pub provider: String,
    pub model: String,
    pub message: String,
    pub duration_ms: u64,
    pub key_results: Vec<KeyTestResult>,
}

// ── Command ──

/// 分析城市数据，由 LLM 给出开仓建议
///
/// 前端传入当前城市的完整数据（温度档位、概率、买卖价、AWC 实况、MET 预报），
/// 后端根据 llm_provider 配置调用千帆 ERNIE 或阿里百炼 Qwen 模型，
/// 返回结构化开仓建议。analyze_city 的输入输出接口对所有 provider 统一。
#[tauri::command]
pub async fn analyze_city(
    data: AnalyzeCityRequest,
    state: State<'_, AppState>,
) -> Result<AnalyzeCityResult, String> {
    let settings = state.db.get_settings().await.map_err(|e| e.to_string())?;

    // 确定 provider: 优先 DB 配置，默认 "qianfan"
    let provider = settings
        .llm_provider
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("qianfan");

    // 根据 provider 选择 URL / model / api_key
    let (api_url, model, api_key): (String, String, Option<String>) = match provider {
        "bailian" => {
            let key = settings
                .bailian_api_key
                .as_ref()
                .filter(|s| !s.is_empty())
                .ok_or("Bailian API key not configured")?;
            let m = settings
                .bailian_model
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(BAILIAN_DEFAULT_MODEL);
            (BAILIAN_CHAT_URL.to_string(), m.to_string(), Some(key.clone()))
        }
        "ollama" => {
            let key = settings
                .ollama_api_key
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned();
            // 未填 URL 时：有 Key 视为云端，无 Key 视为本机自建
            let url = settings
                .ollama_url
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(if key.is_some() { OLLAMA_DEFAULT_URL } else { OLLAMA_LOCAL_URL });
            let m = settings
                .ollama_model
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(OLLAMA_DEFAULT_MODEL);
            if key.is_none() && !is_local_url(url) {
                return Err("Ollama API key not configured".to_string());
            }
            (url.to_string(), m.to_string(), key)
        }
        _ => {
            // 默认走千帆
            let key = settings
                .qianfan_api_key
                .as_ref()
                .filter(|s| !s.is_empty())
                .ok_or("Qianfan API key not configured")?;
            (QIANFAN_CHAT_URL.to_string(), QIANFAN_MODEL.to_string(), Some(key.clone()))
        }
    };

    // 多 Key 轮询：将存储的 Key 字段按换行拆分为多个 Key，轮询选择一个使用
    let api_key: Option<String> = match api_key {
        Some(ref raw) => {
            let keys: Vec<&str> = raw
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect();
            if keys.is_empty() {
                return Err(format!("{} API key not configured", provider));
            }
            if keys.len() > 1 {
                let idx = state.llm_key_counter.fetch_add(1, Ordering::Relaxed) as usize % keys.len();
                tracing::info!(
                    "LLM key round-robin: using key {}/{} for provider {}",
                    idx + 1,
                    keys.len(),
                    provider
                );
                Some(keys[idx].to_string())
            } else {
                Some(keys[0].to_string())
            }
        }
        None => None,
    };

    tracing::info!("analyze_city provider={}, model={}", provider, model);

    // 千帆/百炼是国内服务，本机 Ollama 是回环地址，都用直连；只有 Ollama Cloud 走代理
    let http = if provider == "ollama" && !is_local_url(&api_url) {
        state.http.read().await.clone()
    } else {
        state.direct_http.clone()
    };

    // 构建 prompt（使用用户自定义助记词或默认值）
    let custom_prompt = settings
        .llm_prompt
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let prompt = build_analysis_prompt(&data, custom_prompt);

    // 调用 LLM chat API（Bearer 认证），带 2 次重试
    let chat_req = LlmChatRequest {
        model,
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: prompt,
        }],
        temperature: 0.1,
        top_p: 0.8,
    };

    let mut last_err: Option<String> = None;
    let result_text: String = loop {
        let mut req = http
            .post(&api_url)
            .header("Content-Type", "application/json");

        // 所有 provider 都使用 Bearer 认证
        if let Some(ref key) = api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let attempt = req
            .json(&chat_req)
            .send()
            .await;

        let resp = match attempt {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("Failed to call LLM chat API ({}): {}", provider, e);
                tracing::warn!("LLM API attempt failed ({}), retries left: {}", msg, 2 - last_err.as_ref().map(|_| 1).unwrap_or(0));
                if last_err.is_none() {
                    last_err = Some(msg);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
                return Err(msg);
            }
        };

        match resp.json::<LlmChatResponse>().await {
            Ok(parsed) => {
                break parsed
                    .choices
                    .first()
                    .ok_or("LLM response has no choices")?
                    .message
                    .content
                    .clone();
            }
            Err(e) => {
                let msg = format!("Failed to parse LLM response ({}): {}", provider, e);
                tracing::warn!("LLM API parse failed ({}), retries left: {}", msg, 2 - last_err.as_ref().map(|_| 1).unwrap_or(0));
                if last_err.is_none() {
                    last_err = Some(msg);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
                return Err(msg);
            }
        }
    };

    if let Some(ref err) = last_err {
        tracing::info!("LLM API succeeded after retry (previous error: {})", err);
    }

    // 解析 LLM 返回
    let result = parse_llm_response(&result_text)?;

    Ok(result)
}

/// 默认 LLM 助记词（系统指令）
fn default_llm_prompt() -> &'static str {
    r#"你是一个 Polymarket 气温市场的交易分析师。

## 核心原则：保本第一
保本是最高优先级。只允许开仓最有把握的档位——即 NO概率极高、几乎不可能成为最高温的档位。
如果对某个档位没有很大的把握，就不要开仓。宁可错过机会，也绝不冒亏损风险。
没有合适机会时，should_open 返回 false。

## 任务
分析以下城市气温市场数据，判断是否应该开仓，以及开仓的档位和方向。

## 市场规则
- 每个城市每天有一个「最高温」市场，包含多个温度档位（如 20°C, 21°C, ... 30°C or higher）。
- 最终结算时，只有一个档位的 YES=1（实际最高温命中该档位），其余所有档位 NO=1。
- 温度档位表中的 YES价格 和 NO价格 就是各自命中的概率（YES价格 + NO价格 = 1）。
- YES价格越高 = 该档位越可能是最高温 = NO价格越低 = NO大概率不会结算为1。
- YES价格越低 = 该档位越不可能是最高温 = NO价格越高 = NO大概率结算为1。

## ST 温度——市场结算的判定标准
ST 温度是判定市场温度档位胜负的核心标准和最终依据，必须高度重视。
ST 温度格式为：最高温度/实时温度 天气状况（如 "35/32°C scattered"）。
ST 温度的最高温度反映当天已观测到的最高气温，实时温度反映当前实际气温，天气状况反映当前气象条件。
在判断哪个温度档位会成为最终最高温时，必须以 ST 温度的最高温度为主要依据，而非 AWC 或 MET 数据。
AWC 实况和 MET 预报仅作为辅助参考，当 ST 温度与 AWC/MET 存在差异时，以 ST 温度为准。

## 开仓条件
1. 只考虑 NO 侧开仓（买入 NO token）。
2. 必须选择 NO价格（NO概率）高的档位——这意味着该档位大概率不是最高温，NO结算为1的概率大。
3. NO价格（NO概率）低的档位绝对不能买入——这说明市场认为该档位很可能是最高温，买入NO大概率亏损。
4. ST 温度的最高温度是判断最终最高温的首要依据；AWC 实况温度可作为交叉验证；MET 预报值不够准确，不要直接用其数值判断最高温，但可以参考其预示的最高温出现时间段。
5. 如果所有档位的 NO价格都很低（市场已将最高温锁定在很窄范围），说明没有安全的NO开仓机会，不要开仓。
6. 如果市场尚未充分定价（大部分档位无价格数据），不要开仓。
7. 必须遵守操作栏参数约束：
   - 档位的 bid 和 ask 必须都在 BID/ASK 区间内，否则排除该档位。
   - OFFSET 约束（与当地时间联动，三条独立判断，满足任一即 PASS）：YES峰值档位 = 温度档位表中 mid 值最小的档位（mid 最小 = 市场认为该档位最可能是最终最高温）。注意：必须用 mid 列判定，不要用 YES概率(价格) 列，该列可能滞后或不准确。偏移量 = (当前档位下限 - YES峰值档位下限) / 档位步长。档位步长：°F城市=2（如"68-69°F"下限68），°C城市=1（如"25°C"下限25）。示例：峰值"66-67°F"、当前"68-69°F" → 偏移量=(68-66)/2=1。
     - 条件1：当地时间 <= 13 时，偏移量 >= 3 则 PASS
     - 条件2：当地时间 > 13 时，偏移量 >= 2 则 PASS
     - 条件3：当地时间 > 预估峰值时间 且 ST 实时温度 < ST 最高温度时，偏移量 >= 1 则 PASS。预估峰值时间 = 昨天最高温出现的小时（见下方 ST 数据中的"昨天最高温度"行），若昨天数据缺失则默认为 16:00。
      以上三条为独立判断，满足任一即 PASS，全部不满足才 FAIL。
8. 必须结合时间因素和昨日峰值时间判断最高温是否已经定型：
   - 【预估峰值时间】日最高温通常出现在午后，但每个城市因纬度、气候不同而有所差异。如果下方 ST 数据提供了"昨天最高温度（出现在当地时间 HH:00）"，则以该小时作为今日预估峰值时间的参考；若没有昨日数据，则默认预估峰值时间为 16:00。
   - 如果当前当地时间已过预估峰值时间 + 1 小时，当日最高温大概率已确定，不再会升得更高，此时可以更有信心地排除该最高温档位附近的开仓。
   - 如果当前当地时间在 10:00 到预估峰值时间之间，温度仍可能继续上升，已观测到的最高温未必是最终最高温。此时需要参考 MET 预报中剩余时段是否有更高温度，判断最终最高温可能落在哪个档位。
   - 如果当前当地时间在 10:00 之前，当日最高温远未确定，预报不确定性最大，应更加谨慎，优先选择离预报峰值很远的档位开仓 NO。
   - 关注最高温已观测到的时间点：如果当天最高温出现在预估峰值时间附近且当前已过预估峰值时间 + 1 小时，说明午后高峰已过，最高温大概率定型；如果最高温出现在上午且当前仍在午前，后续可能还有更高温度。
   - 【关键信号】当当地时间 > 预估峰值时间 且 ST 最高温度 > 当前实时温度时，说明最高温峰值已过、后续不太可能再出现更高的温度。此时最高温大概率已经定型，可以显著增加开仓把握度，更放心地排除最高温档位附近的 NO 开仓。注意：在预估峰值时间之前或恰逢预估峰值时间时，ST 最高温度 > 当前实时温度可能只是暂时波动而非峰值已过，不得据此放宽约束。
9. MET 预报值不准确，不要直接用预报数值判断最终最高温；但可以参考预报中高温分布的趋势，辅助判断最高温可能出现的时间段。ST 最高温度才是判断最终最高温的可靠依据。
10. GATE-YESTERDAY 硬约束：按天气大类判断今天与昨天是否相似（将天气状况归入以下大类后比较）：
    - 无降水类：clear, sky clear, few clouds, sunny, scattered, broken, overcast, cloudy, haze, mist, fog, smoke, dust
    - 有降水类：rain, drizzle, showers, thunderstorm, snow, sleet, ice, freezing, hail, snow grains
    当今天与昨天属同一大类时，昨天最高温所在档位及其相邻正负1个步长的档位判定为 FAIL，禁止开仓 NO。理由：相似天气条件下今天最高温很可能落在与昨天相近的区间，开仓 NO 风险过高。仅当昨天最高温不在该档位范围及相邻范围内时才 PASS。当今天与昨天属不同大类时，此约束不生效（直接 PASS）。档位步长：华氏度城市=2（如"68-69°F"步长2），摄氏度城市=1（如"25°C"步长1）。示例：昨天最高温69.1°F落在68-69°F范围内，则66-67°F、68-69°F、70-71°F三个档位均 FAIL。
11. 昨日高温参考约束（GATE-YESTERDAY-SOFT）：无论天气大类是否相同，如果昨天最高温存在且 > 当前 ST 最高温度，则昨天最高温所在档位及其相邻正负1个步长的档位为高风险区，仅同时满足以下全部条件才 PASS：
    - 当地时间 > 预估峰值时间（确认午后升温高峰已过；预估峰值时间 = 昨天最高温出现的小时，无昨日数据时默认 16:00）
    - ST 实时温度 < ST 最高温度（温度处于下降趋势，非暂时波动）
    - 偏移量（当前档位下限 - YES峰值档位下限）/ 档位步长 >= 3
    任一条件不满足则 FAIL。此约束与 GATE-YESTERDAY 独立判断，即使 GATE-YESTERDAY 因天气大类不同而 PASS，本约束仍需单独检查。当昨天最高温 <= 当前 ST 最高温度时此约束不生效（直接 PASS）。
"#
}

/// 构建发送给 LLM 的分析 prompt
///
/// `custom_prompt`: 用户在设置中自定义的助记词，为 None 时使用默认值。
fn build_analysis_prompt(data: &AnalyzeCityRequest, custom_prompt: Option<&str>) -> String {
    let mut prompt = String::new();

    // 助记词部分：用户自定义优先，否则用默认
    let instruction = match custom_prompt {
        Some(p) => p,
        None => default_llm_prompt(),
    };
    prompt.push_str(instruction);
    prompt.push('\n');

    // 已持仓档位
    if !data.held_thresholds.is_empty() {
        prompt.push_str("## 已持仓档位（无需再分析，直接跳过）\n");
        for label in &data.held_thresholds {
            prompt.push_str(&format!("- {}（已开仓）\n", label));
        }
        prompt.push_str("以上档位已经开仓，不要重复推荐。请分析其余未持仓的档位是否也值得开仓。\n\n");
    }

    prompt.push_str("## 当前城市数据\n");
    prompt.push_str(&format!("- 城市: {}\n", data.city));
    prompt.push_str(&format!("- 时区: {}\n", data.city_tz));
    prompt.push_str(&format!("- 当地时间: {}\n", data.local_time));
    prompt.push_str(&format!("- 温度单位: {}\n\n", data.unit));

    // 操作栏参数
    prompt.push_str("### 操作栏参数（用户设定的交易约束）\n");
    prompt.push_str(&format!("- BID/ASK: 只允许买入 bid 和 ask 都在 [{:.3}, {:.3}] 区间内的档位。bid/ask 不在此区间内的档位必须排除。\n", data.bid_ask_min, data.bid_ask_max));
    prompt.push_str("- OFFSET 约束：规则详见上方开仓条件中的定义，按此规则判断每个档位 PASS/FAIL。\n");
    prompt.push_str("- GATE-YESTERDAY 约束：规则详见上方开仓条件中的定义，按此规则判断每个档位 PASS/FAIL。\n\n");

    // ST 源温度（判定标准，排在 AWC 之前）
    prompt.push_str("### ST 温度（市场结算判定标准，最高优先级）\n");
    prompt.push_str("格式为：最高温度/实时温度 天气状况。判定规则详见上方开仓条件。\n");
    if let Some(ref st) = data.st {
        let high_str = st.high.map(|h| format!("{:.1}{}", h, data.unit)).unwrap_or_else(|| "--".to_string());
        let current_str = st.current.map(|c| format!("{:.1}{}", c, data.unit)).unwrap_or_else(|| "--".to_string());
        let cond_str = st.condition.as_deref().unwrap_or("--");
        prompt.push_str(&format!("- ST: {}/{} {}\n", high_str, current_str, cond_str));
        if let Some(h) = st.high {
            prompt.push_str(&format!("- 当天最高温度（ST）: {:.1}{} —— 这是判断最终最高温的首要依据\n", h, data.unit));
        }
        if let Some(c) = st.current {
            prompt.push_str(&format!("- 当前实时温度（ST）: {:.1}{}\n", c, data.unit));
        }
        if let Some(ref cond) = st.condition {
            if !cond.is_empty() {
                prompt.push_str(&format!("- 天气状况: {}\n", cond));
            }
        }
        if let Some(yh) = st.yesterday_high {
            let hour_str = st.yesterday_high_hour
                .map(|h| format!("（出现在当地时间 {:02}:00）", h))
                .unwrap_or_default();
            prompt.push_str(&format!("- 昨天最高温度: {:.1}{}{}\n", yh, data.unit, hour_str));
            if let Some(ref yc) = st.yesterday_condition {
                if !yc.is_empty() {
                    prompt.push_str(&format!("- 昨天天气状况: {}\n", yc));
                }
            }
            prompt.push_str("- 提示：昨天有温度数据，GATE-YESTERDAY 约束生效，请按开仓条件中的规则判断各档位 PASS/FAIL。\n");
        }
    } else {
        prompt.push_str("- 无 ST 温度数据\n");
    }
    prompt.push('\n');

    // AWC 实况
    prompt.push_str("### AWC 实况温度\n");
    if let Some(ref awc) = data.awc {
        if let Some(max) = awc.max {
            prompt.push_str(&format!("- 当天已检测到的最高温度: {:.1}{}（时间向后推移也许会出现更高的值）\n", max, data.unit));
            // 从 hourly_temps 中找出最高温出现的时间
            if let Some(ref hourly) = awc.hourly_temps {
                if !hourly.is_empty() {
                    let max_hour = hourly.iter()
                        .filter(|(_, &v)| (v - max).abs() < 0.05)
                        .map(|(&h, _)| h)
                        .min();
                    if let Some(h) = max_hour {
                        prompt.push_str(&format!("- 最高温度出现在当地时间 {}:00\n", h));
                    }
                }
            }
        }
        if let Some(current) = awc.current {
            prompt.push_str(&format!("- 当前实时温度: {:.1}{}\n", current, data.unit));
        }
        if let Some(ref hourly) = awc.hourly_temps {
            if !hourly.is_empty() {
                prompt.push_str("- 按小时观测温度:\n");
                let mut hours: Vec<u32> = hourly.keys().copied().collect();
                hours.sort();
                for h in hours {
                    if let Some(temp) = hourly.get(&h) {
                        prompt.push_str(&format!("  - {}:00 -> {:.1}{}\n", h, temp, data.unit));
                    }
                }
            }
        }
    } else {
        prompt.push_str("- 无 AWC 数据\n");
    }
    prompt.push('\n');

    // MET 预报
    prompt.push_str("### MET 预报温度 (10:00-17:00)\n");
    if data.met_forecast.is_empty() {
        prompt.push_str("- 无 MET 预报数据\n");
    } else {
        for (i, f) in data.met_forecast.iter().enumerate() {
            let hour = 10 + i as u32;
            prompt.push_str(&format!("- {}:00 -> {:.1}{}\n", hour, f.temp, data.unit));
        }
    }
    prompt.push('\n');

    // 温度档位
    prompt.push_str("### 温度档位及价格（概率）\n");
    prompt.push_str("| 档位 | YES概率(价格) | NO概率(价格) | bid | ask | mid |\n");
    prompt.push_str("|------|---------------|-------------|-----|-----|-----|\n");
    for t in &data.thresholds {
        let bid_str = t.bid.map(|b| format!("{:.3}", b)).unwrap_or("-".to_string());
        let ask_str = t.ask.map(|a| format!("{:.3}", a)).unwrap_or("-".to_string());
        let mid_str = t.mid.map(|m| format!("{:.3}", m)).unwrap_or("-".to_string());
        prompt.push_str(&format!(
            "| {} | {:.3} | {:.3} | {} | {} | {} |\n",
            t.label, t.yes_price, t.no_price, bid_str, ask_str, mid_str
        ));
    }
    prompt.push('\n');

    // 两步推理输出格式（代码硬编码，用户不可编辑）
    // 第一步强制逐条约束检查，第二步才输出 JSON，防止 LLM 先提交 actions 再自我修正
    prompt.push_str("## 输出要求（两步推理结构，严格遵守）\n\n");
    prompt.push_str("### 第一步：逐条约束检查\n");
    prompt.push_str("对每个有 ask 价格的档位，逐条列出以下检查结果：\n");
    prompt.push_str("- GATE-PRICE：bid 和 ask 是否都在 [BID_ASK_MIN, BID_ASK_MAX] 区间内？PASS 或 FAIL\n");
    prompt.push_str("- GATE-OFFSET：按开仓条件中定义的 OFFSET 约束检查，PASS 或 FAIL\n");
    prompt.push_str("- GATE-YESTERDAY：按开仓条件中定义的 GATE-YESTERDAY 约束检查，PASS 或 FAIL\n");
    prompt.push_str("- GATE-YESTERDAY-SOFT：按开仓条件中定义的昨日高温参考约束检查，PASS 或 FAIL\n\n");
    prompt.push_str("**关键规则：任何一项为 FAIL 的档位，绝对不能出现在最终 JSON 的 actions 数组中。**\n\n");
    prompt.push_str("**禁止在 JSON 字符串值中使用 LaTeX 格式（如 $\\le$、$\\ge$、$\\rightarrow$），使用纯文本（如 <=, >=, ->）。**\n\n");
    prompt.push_str("### 第二步：输出最终 JSON\n");
    prompt.push_str("在完成所有档位的逐条检查后，仅输出一个 JSON 格式的结果，不要输出多个 JSON：\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"actions\": [\n");
    prompt.push_str("    {\n");
    prompt.push_str("      \"threshold_label\": \"档位标签如 25°C\",\n");
    prompt.push_str("      \"side\": \"NO\",\n");
    prompt.push_str("      \"reason\": \"简要说明分析理由\"\n");
    prompt.push_str("    }\n");
    prompt.push_str("  ],\n");
    prompt.push_str("  \"summary\": \"总体分析说明\"\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n");
    prompt.push_str("所有 GATE 检查全部 PASS 的档位才能放入 actions。如果没有档位通过所有 GATE 检查，actions 返回空数组 []，并在 summary 中说明原因。\n");
    prompt.push_str("一次可以推荐多个档位，只要每个档位都满足所有开仓条件即可。\n");

    prompt
}

/// 获取默认 LLM 助记词
#[tauri::command]
pub async fn get_default_llm_prompt() -> Result<String, String> {
    Ok(default_llm_prompt().to_string())
}

/// 不可编辑的附加数据模板（每次分析时自动拼接在提示词之后）
fn llm_prompt_suffix_template() -> &'static str {
    r#"以下为每次分析时自动追加的数据，不可编辑：

## 已持仓档位（无需再分析，直接跳过）
- {已持仓档位标签}（已开仓）
以上档位已经开仓，不要重复推荐。请分析其余未持仓的档位是否也值得开仓。

## 当前城市数据
- 城市: {城市名}
- 时区: {时区}
- 当地时间: {当地时间}
- 温度单位: °C / °F

### 操作栏参数（用户设定的交易约束）
- BID/ASK: 只允许买入 bid 和 ask 都在 [{bid_ask_min}, {bid_ask_max}] 区间内的档位。
- OFFSET 约束：规则详见上方开仓条件中的定义，此处仅展示当前数据供 LLM 按规则计算。
- GATE-YESTERDAY 约束：规则详见上方开仓条件中的定义，此处仅展示当前数据供 LLM 按规则判断。

### ST 温度（市场结算判定标准，最高优先级）
格式为：最高温度/实时温度 天气状况。判定规则详见上方开仓条件。
- ST: {high}/{current}{unit} {condition}
- 当天最高温度（ST）: {high}{unit} —— 这是判断最终最高温的首要依据
- 当前实时温度（ST）: {current}{unit}
- 天气状况: {condition}

### AWC 实况温度
- 当天已检测到的最高温度: {max}°{unit}
- 最高温度出现在当地时间 {hour}:00
- 当前实时温度: {current}°{unit}
- 按小时观测温度:
  - {hour}:00 -> {temp}°{unit}
  ...

### MET 预报温度 (10:00-17:00)
- {hour}:00 -> {temp}°{unit}
  ...

### 温度档位及价格（概率）
| 档位 | YES概率(价格) | NO概率(价格) | bid | ask | mid |
|------|---------------|-------------|-----|-----|-----|
| {label} | {yes_price} | {no_price} | {bid} | {ask} | {mid} |
  ...

## 输出要求（两步推理结构，严格遵守）

### 第一步：逐条约束检查
对每个有 ask 价格的档位，逐条列出以下检查结果：
- GATE-PRICE：bid 和 ask 是否都在 [BID_ASK_MIN, BID_ASK_MAX] 区间内？PASS 或 FAIL
- GATE-OFFSET：按开仓条件中定义的 OFFSET 约束检查，PASS 或 FAIL
- GATE-YESTERDAY：按开仓条件中定义的 GATE-YESTERDAY 约束检查，PASS 或 FAIL
- GATE-YESTERDAY-SOFT：按开仓条件中定义的昨日高温参考约束检查，PASS 或 FAIL

**关键规则：任何一项为 FAIL 的档位，绝对不能出现在最终 JSON 的 actions 数组中。**

**禁止在 JSON 字符串值中使用 LaTeX 格式（如 $\le$、$\ge$、$\rightarrow$），使用纯文本（如 <=, >=, ->）。**

### 第二步：输出最终 JSON
在完成所有档位的逐条检查后，仅输出一个 JSON 格式的结果，不要输出多个 JSON：
```json
{
  "actions": [
    {
      "threshold_label": "档位标签如 25°C",
      "side": "NO",
      "reason": "简要说明分析理由"
    }
  ],
  "summary": "总体分析说明"
}
```
所有 GATE 检查全部 PASS 的档位才能放入 actions。如果没有档位通过所有 GATE 检查，actions 返回空数组 []，并在 summary 中说明原因。
一次可以推荐多个档位，只要每个档位都满足所有开仓条件即可。
"#
}

/// 获取不可编辑的附加数据模板
#[tauri::command]
pub async fn get_llm_prompt_suffix() -> Result<String, String> {
    Ok(llm_prompt_suffix_template().to_string())
}

/// 测试当前选中 LLM provider 的连通性
///
/// 向当前 provider 发送一条简短消息，验证 API Key、URL 和模型是否可用。
/// 返回 success + 响应摘要，或失败原因。
#[tauri::command]
pub async fn test_llm_connection(
    form: SettingsForm,
    state: State<'_, AppState>,
) -> Result<TestLlmResult, String> {
    let provider = form
        .llm_provider
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("qianfan");

    // 根据 provider 选择 URL / model / api_key（与 analyze_city 一致）
    let (api_url, model, api_key): (String, String, Option<String>) = match provider {
        "bailian" => {
            let key = form
                .bailian_api_key
                .as_ref()
                .filter(|s| !s.is_empty())
                .ok_or("Bailian API key not configured")?;
            let m = form
                .bailian_model
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(BAILIAN_DEFAULT_MODEL);
            (BAILIAN_CHAT_URL.to_string(), m.to_string(), Some(key.clone()))
        }
        "ollama" => {
            let key = form
                .ollama_api_key
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned();
            let url = form
                .ollama_url
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(if key.is_some() { OLLAMA_DEFAULT_URL } else { OLLAMA_LOCAL_URL });
            let m = form
                .ollama_model
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(OLLAMA_DEFAULT_MODEL);
            if key.is_none() && !is_local_url(url) {
                return Err("Ollama API key not configured".to_string());
            }
            (url.to_string(), m.to_string(), key)
        }
        _ => {
            let key = form
                .qianfan_api_key
                .as_ref()
                .filter(|s| !s.is_empty())
                .ok_or("Qianfan API key not configured")?;
            (QIANFAN_CHAT_URL.to_string(), QIANFAN_MODEL.to_string(), Some(key.clone()))
        }
    };

    // 多 Key 支持：按换行拆分为多个 Key，逐个测试
    let keys: Vec<String> = match api_key {
        Some(ref raw) => raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        None => vec![],
    };
    // 本机 Ollama 无需 Key，用一个空串占位使下面的测试循环照常跑一轮
    let keys: Vec<String> = if keys.is_empty() {
        if is_local_url(&api_url) {
            vec![String::new()]
        } else {
            return Err(format!("{} API key not configured", provider));
        }
    } else {
        keys
    };

    // 千帆/百炼是国内服务，本机 Ollama 是回环地址，都用直连；只有 Ollama Cloud 走代理
    let http = if provider == "ollama" && !is_local_url(&api_url) {
        state.http.read().await.clone()
    } else {
        state.direct_http.clone()
    };

    let total_start = std::time::Instant::now();
    let mut key_results: Vec<KeyTestResult> = Vec::with_capacity(keys.len());

    for (i, key) in keys.iter().enumerate() {
        let chat_req = LlmChatRequest {
            model: model.clone(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: "ping".to_string(),
            }],
            temperature: 0.1,
            top_p: 0.8,
        };

        let key_start = std::time::Instant::now();
        let prefix: String = key.chars().take(8).collect();

        let mut req = http
            .post(&api_url)
            .header("Content-Type", "application/json");
        if !key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let result = req.json(&chat_req).send().await;

        let duration_ms = key_start.elapsed().as_millis() as u64;

        match result {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    let short: String = body.chars().take(200).collect();
                    key_results.push(KeyTestResult {
                        key_index: i + 1,
                        key_prefix: prefix,
                        success: false,
                        message: format!("HTTP {} - {}", status.as_u16(), short),
                        duration_ms,
                    });
                } else {
                    match resp.json::<LlmChatResponse>().await {
                        Ok(parsed) => {
                            let content = parsed
                                .choices
                                .first()
                                .map(|c| c.message.content.clone())
                                .unwrap_or_default();
                            let preview: String = content.chars().take(80).collect();
                            key_results.push(KeyTestResult {
                                key_index: i + 1,
                                key_prefix: prefix,
                                success: true,
                                message: format!("OK: \"{}\"", preview),
                                duration_ms,
                            });
                        }
                        Err(e) => {
                            key_results.push(KeyTestResult {
                                key_index: i + 1,
                                key_prefix: prefix,
                                success: false,
                                message: format!("Response parse failed: {}", e),
                                duration_ms,
                            });
                        }
                    }
                }
            }
            Err(e) => {
                key_results.push(KeyTestResult {
                    key_index: i + 1,
                    key_prefix: prefix,
                    success: false,
                    message: format!("Connection failed: {}", e),
                    duration_ms,
                });
            }
        }
    }

    let total_duration_ms = total_start.elapsed().as_millis() as u64;
    let passed = key_results.iter().filter(|r| r.success).count();
    let all_passed = passed == key_results.len();
    let message = format!("{}/{} keys passed", passed, key_results.len());

    Ok(TestLlmResult {
        success: all_passed,
        provider: provider.to_string(),
        model,
        message,
        duration_ms: total_duration_ms,
        key_results,
    })
}

/// 解析 LLM 返回的 JSON 响应
///
/// 两步推理输出中，第一步的约束检查文本常含 LaTeX（`\text{C}`、`$\ge$`）等花括号/反斜杠，
/// 直接取"第一个 {"会截到推理文本里的 `{C}`（复盘中 17% 的分析因此失败）。
/// 提取顺序：
/// 1. 最后一个 ```json 围栏块中的对象；
/// 2. 否则全文中最后一个包含 "actions" 键的完整 JSON 对象；
/// 3. 否则退回第一个完整 JSON 对象。
/// 提取后先清洗字符串值中的 LaTeX 与非法转义，再交给 serde_json。
fn parse_llm_response(raw: &str) -> Result<AnalyzeCityResult, String> {
    let mut candidates: Vec<&str> = Vec::new();

    if let Some(fenced) = extract_last_json_fence(raw) {
        if let Some(obj) = extract_first_json_object(fenced) {
            candidates.push(obj);
        }
    }
    if let Some(obj) = extract_last_json_object_containing(raw, "\"actions\"") {
        candidates.push(obj);
    }
    if let Some(obj) = extract_first_json_object(raw) {
        candidates.push(obj);
    }

    if candidates.is_empty() {
        return Err(format!("No JSON object found in LLM response | raw: {}", raw));
    }

    let mut last_err = String::new();
    for cand in candidates {
        let sanitized = sanitize_json_escapes(&strip_latex_in_strings(cand));
        match serde_json::from_str::<AnalyzeCityResult>(&sanitized) {
            Ok(parsed) => return Ok(parsed),
            Err(e) => last_err = e.to_string(),
        }
    }

    Err(format!(
        "Failed to parse LLM response as JSON: {} | raw: {}",
        last_err, raw
    ))
}

/// 提取最后一个 ```json ... ``` 围栏块的内容（不含围栏标记）。
fn extract_last_json_fence(raw: &str) -> Option<&str> {
    let start_tag = "```json";
    let start = raw.rfind(start_tag)?;
    let body_start = start + start_tag.len();
    let rest = &raw[body_start..];
    let end = rest.find("```").unwrap_or(rest.len());
    let body = rest[..end].trim();
    if body.is_empty() { None } else { Some(body) }
}

/// 从后往前找包含 `needle` 的完整 JSON 对象（花括号深度配对，忽略字符串内的括号）。
fn extract_last_json_object_containing<'a>(raw: &'a str, needle: &str) -> Option<&'a str> {
    let bytes = raw.as_bytes();
    // 记录每个顶层对象 [start, end]
    let mut objects: Vec<(usize, usize)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape { escape = false; continue; }
        if in_string {
            if b == b'\\' { escape = true; }
            else if b == b'"' { in_string = false; }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => stack.push(i),
            b'}' => {
                if let Some(s) = stack.pop() {
                    if stack.is_empty() {
                        objects.push((s, i));
                    }
                }
            }
            _ => {}
        }
    }
    objects
        .into_iter()
        .rev()
        .map(|(s, e)| &raw[s..=e])
        .find(|obj| obj.contains(needle))
}

/// 去掉 JSON 字符串值内部的 LaTeX 行内公式标记 `$...$`，并把常见 LaTeX 符号替换为纯文本。
/// 仅处理字符串内部，不改动 JSON 结构。
fn strip_latex_in_strings(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape = false;
    let mut buf = String::new();

    let flush = |buf: &mut String, out: &mut String| {
        if buf.is_empty() { return; }
        let mut s = buf.clone();
        // 注意顺序：长命令在前，避免 \geq 被 \ge 先替换成 ">=q"
        for (from, to) in [
            ("\\geq", ">="), ("\\ge", ">="), ("\\leq", "<="), ("\\le", "<="),
            ("\\rightarrow", "->"), ("\\notin", "not in"), ("\\neq", "!="), ("\\ne", "!="),
            ("\\times", "x"), ("\\circ", "°"), ("\\to", "->"), ("\\in", "in"),
        ] {
            s = s.replace(from, to);
        }
        // \text{...} → ...
        while let Some(p) = s.find("\\text{") {
            let after = p + "\\text{".len();
            if let Some(close) = s[after..].find('}') {
                let inner = s[after..after + close].to_string();
                s.replace_range(p..after + close + 1, &inner);
            } else {
                break;
            }
        }
        s = s.replace('$', "");
        out.push_str(&s);
        buf.clear();
    };

    for ch in input.chars() {
        if in_string {
            if escape {
                buf.push(ch);
                escape = false;
                continue;
            }
            if ch == '\\' {
                buf.push(ch);
                escape = true;
                continue;
            }
            if ch == '"' {
                flush(&mut buf, &mut out);
                out.push(ch);
                in_string = false;
                continue;
            }
            buf.push(ch);
        } else {
            out.push(ch);
            if ch == '"' { in_string = true; }
        }
    }
    flush(&mut buf, &mut out);
    out
}

/// 清洗 JSON 字符串中的非法转义序列。
/// 仅处理 JSON 字符串值内部的反斜杠转义，保留字符串外部的结构不变。
/// 合法 JSON 转义：\" \\ \/ \b \f \n \r \t \uXXXX
/// 非法转义（如 \l \g \r \a 等）会被替换为 \\l \\g \\r \\a（双反斜杠 + 原字符）
fn sanitize_json_escapes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(input.len());
    let mut in_string = false;
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        if !in_string {
            result.push(b);
            if b == b'"' {
                in_string = true;
            }
            i += 1;
            continue;
        }

        // In string
        if b == b'\\' {
            if i + 1 < bytes.len() {
                let next = bytes[i + 1];
                // Check if this is a valid JSON escape
                let is_valid = matches!(next,
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u'
                );
                if is_valid {
                    // Keep as-is
                    result.push(b);
                    result.push(next);
                    i += 2;
                } else {
                    // Invalid escape: replace \x with \\x (escaped backslash + char)
                    result.push(b'\\');
                    result.push(b'\\');
                    result.push(next);
                    i += 2;
                }
            } else {
                // Trailing backslash at end - escape it
                result.push(b'\\');
                result.push(b'\\');
                i += 1;
            }
            continue;
        }

        if b == b'"' {
            in_string = false;
        }
        result.push(b);
        i += 1;
    }

    String::from_utf8(result).unwrap_or_else(|_| input.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复盘样本：推理文本含 LaTeX `\text{C}`，旧解析器取到 `{C}` 报 "key must be a string"
    #[test]
    fn parse_skips_latex_braces_in_reasoning() {
        let raw = "### 第一步\n- YES峰值档位（mid 最小）：$27^\\circ\\text{C}$ (mid = 0.535)\n- GATE-PRICE: bid 0.999 $\\rightarrow$ **FAIL**\n\n### 第二步\n```json\n{\n  \"actions\": [],\n  \"summary\": \"所有档位均未能通过 GATE-PRICE 约束检查。\"\n}\n```";
        let r = parse_llm_response(raw).expect("should parse");
        assert!(r.actions.is_empty());
        assert!(r.summary.contains("GATE-PRICE"));
    }

    /// 复盘样本：summary 字符串里含 `$\ge 3$`，旧解析器报 "invalid escape"
    #[test]
    fn parse_strips_latex_inside_string_values() {
        let raw = "```json\n{\n  \"actions\": [],\n  \"summary\": \"偏移量 (2) 未达到条件1要求的 $\\ge 3$，故被排除。\"\n}\n```";
        let r = parse_llm_response(raw).expect("should parse");
        assert_eq!(r.summary, "偏移量 (2) 未达到条件1要求的 >= 3，故被排除。");
    }

    /// 无围栏、前文有干扰对象时，取最后一个含 actions 的对象
    #[test]
    fn parse_prefers_last_actions_object_without_fence() {
        let raw = "检查 {C} 完毕。{\"foo\": 1}\n{\"actions\":[{\"threshold_label\":\"25°C\",\"side\":\"NO\",\"reason\":\"r\"}],\"summary\":\"s\"}";
        let r = parse_llm_response(raw).expect("should parse");
        assert_eq!(r.actions.len(), 1);
        assert_eq!(r.actions[0].threshold_label, "25°C");
    }

    #[test]
    fn parse_handles_escaped_quotes_in_strings() {
        let raw = "{\"actions\":[],\"summary\":\"he said \\\"no\\\" and $x \\le 1$\"}";
        let r = parse_llm_response(raw).expect("should parse");
        assert_eq!(r.summary, "he said \"no\" and x <= 1");
    }

    #[test]
    fn parse_errors_when_no_json() {
        assert!(parse_llm_response("nothing here").is_err());
    }
}

/// 从字符串中提取第一个完整的 JSON 对象（基于花括号深度配对）
fn extract_first_json_object(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut depth = 0i32;
    let mut start = None;
    let mut in_string = false;
    let mut escape = false;

    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if b == b'\\' {
            escape = true;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if b == b'{' {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                if let Some(s) = start {
                    return Some(&raw[s..=i]);
                }
            }
        }
    }
    None
}
