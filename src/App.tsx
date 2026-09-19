import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CityTempMarkets, TempMarketType, TempThreshold } from "./types/temperature";
import SettingsModal from "./components/SettingsModal";
import TradeStatsModal from "./components/TradeStatsModal";
import CitySelectorModal from "./components/CitySelectorModal";

import { PnlChart } from "./components/PnlChart";
import CityCard, { type CityRow } from "./components/CityCard";
import { rlog, nextRound } from "./lib/reviewLog";

// Toast 通知类型
interface ToastMsg {
  id: number;
  type: "success" | "error";
  message: string;
}

function cityDisplayName(slug: string): string {
  return slug.split("-").map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(" ");
}

interface FlatRow {
  city: string;
  city_tz: string;
  market_type: TempMarketType;
  event_slug: string;
  threshold: TempThreshold;
}

function flattenAndFilter(cities: CityTempMarkets[]): FlatRow[] {
  const rows: FlatRow[] = [];
  for (const c of cities) {
    if (!c.highest) continue;
    for (const t of c.highest.thresholds) {
      rows.push({ city: c.city, city_tz: c.city_tz, market_type: c.highest.market_type, event_slug: c.highest.event_slug, threshold: t });
    }
  }
  return rows;
}

function tzOffsetHours(tz: string): number | null {
  try {
    const now = new Date();
    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone: tz,
      year: "numeric", month: "2-digit", day: "2-digit",
      hour: "2-digit", minute: "2-digit", second: "2-digit",
      hour12: false,
    }).formatToParts(now);
    const get = (type: string) => parts.find((p) => p.type === type)?.value ?? "0";
    let hour = parseInt(get("hour"), 10);
    if (hour === 24) hour = 0;
    const localMs = Date.UTC(
      parseInt(get("year"), 10),
      parseInt(get("month"), 10) - 1,
      parseInt(get("day"), 10),
      hour,
      parseInt(get("minute"), 10),
      parseInt(get("second"), 10),
    );
    const diffMs = localMs - now.getTime();
    return diffMs / 3_600_000;
  } catch {
    return null;
  }
}

function localHour(gmt: Date, offsetHours: number): { hour: string; dayOffset: number } {
  const total = gmt.getUTCHours() + Math.round(offsetHours);
  const h = ((total % 24) + 24) % 24;
  const dayOffset = total < 0 ? -1 : total >= 24 ? 1 : 0;
  return { hour: String(h).padStart(2, "0"), dayOffset };
}

// ── 开仓安全闸（代码硬校验，独立于 LLM 判断） ──
//
// 复盘（2026-09-16/17，32 笔平仓）：31 胜共 +181，1 败 -487（denver 88-89°F）。
// 买 NO@0.99 的收益上限约 1%，单次亏损约 100%，盈亏比要求胜率 > 99%。
// denver 亏损发生在当地 12:00：ST 最高温 74.7°F、MET 预报峰值 82.8°F、市场峰值档 82-83°F，
// 开仓档位 88-89°F 距各参考峰值仅 2~3 个档位；实际当日最高 89.2°F（MET 低估 6.4°F）。
// 复盘 49 个城市日：MET 中午预报峰值被实际最高温超过的比例 61%，p90 误差约 4 度，最大 5.9 度。
// 因此在峰值尚未观测到之前，档位下限必须高出所有参考峰值至少 3 个档位；
// 峰值已过（16:00 后且温度不再上升）后，只需高出已观测最高温 1 个完整档位且高出 MET 峰值半档。
//
// 回测（backtest/simulate.py，51 城 × 2026-08-10~09-16，1938 个城市日，10585 个候选盘口）：
//   旧 LLM 偏移规则            1772 笔 / 25 亏 / +0.50%/笔
//   本闸门（15:00 版）          1638 笔 / 23 亏 / +0.74%/笔
//   本闸门（16:00 + MET 半档）  1102 笔 / 10 亏 / +1.05%/笔；ask 上限收到 0.99 后 855 笔 / 7 亏 / +1.68%/笔
//   ∩ LLM 推荐（实际线上）      ask≤0.99：474 笔 / 2 亏 / +1.85%/笔；ask≤0.98：240 笔 / 1 亏 / +2.80%/笔
//
// v2.2.0 重测（2026-09-19，同一份 51 城 20 天真实盘口，资金层 $2000 / 10% 仓位 / 亏损率压力 +1.4% × 200 次）：
//   按买入价分带隔离闸门放行的机会（笔数 / 单笔收益 / 实盘 NO 侧深度）：
//     0.92–0.95   234 / +4.15% / $51.8k      ← 毛利最厚，此前被 askMin=0.95 全部挡掉
//     0.95–0.98   523 / +2.19% / $111.4k
//     0.98–0.99   561 / +1.17% / $48.3k      ← 此前被 askMax=0.98 全部挡掉
//     0.99–0.995  539 / +0.27% / $60.9k      ← 收益薄到被费用吃光，不要
//     0.995–1.00 5949 / −0.31% / $173.0k     ← 明确亏损带
//   资金层对照（32 天收益 / 压力中位 / 压力 10 分位 / 压力回撤）：
//     0.95–0.98 + LLM 硬否决   243 机会 →  +76% / +29.1% /  +3.6% / 14.0%（v2.1.1 线上）
//     0.92–0.99 + LLM 硬否决   516 机会 →  +88% / +23.6% /  −6.6% / 18.0%  只放宽价格反而恶化尾部
//     0.92–0.99 + 闸门自主    1016 机会 → +151% / +56.2% / +17.7% / 17.1%  ← 采用
//     0.92–0.995 + 闸门自主   1260 机会 →  +83% / +17.1% / −13.1% / 22.3%  上限不能到 0.995
//   结论：askMin=0.92、askMax=0.99，且 LLM 降为建议（不再作为硬否决），候选池由闸门遍历全部档位生成。
//   只改价格区间而保留 LLM 硬否决是负向的——LLM 专门挡掉 0.92–0.95 这批单笔 +4.15% 的肥单。
//   止损：0.95/0.90 会把大量最终盈利的仓位在 -1%~-10% 处打掉，净收益反而大幅下降；
//        0.50 的"灾难止损"略优于不止损，故默认 SL=0.50。

interface SafetyGateInput {
  /** 档位标签，如 "88-89°F" / "25°C" / "28°C or higher" / "9°C or below" */
  label: string;
  /** 温度单位 "°C" / "°F" */
  unit: string;
  /** 当地小时（0-23） */
  localHour: number;
  /** ST 当天最高温 / 实时温度 */
  stHigh: number | null;
  stCurrent: number | null;
  /** AWC 当天最高温 */
  awcMax: number | null;
  /** MET 预报 10:00-17:00 最高值 */
  metPeak: number | null;
  /** 市场 YES 峰值档（mid 最小）的下限 */
  marketPeakLower: number | null;
  /** 昨日最高温出现小时（预估今日峰值时间），缺失默认 16 */
  yesterdayHighHour: number | null;
}

interface SafetyGateResult {
  pass: boolean;
  reason: string;
  detail: Record<string, number | string | null>;
}

/** 解析档位下限："88-89°F"→88，"25°C"→25，"28°C or higher"→28，"9°C or below"→null */
function parseThresholdLower(label: string): number | null {
  if (/or below/i.test(label)) return null;
  const m = label.match(/-?\d+(?:\.\d+)?/);
  return m ? parseFloat(m[0]) : null;
}

function thresholdStep(unit: string): number {
  return unit.includes("F") ? 2 : 1;
}

/** 当地几点起允许按"峰值已过"规则开仓（回测最优：16） */
const POST_PEAK_MIN_HOUR = 16;
/** 峰值已过时档位下限需高出 MET 峰值的档数（回测最优：0.5） */
const POST_PEAK_MET_MARGIN_STEPS = 0.5;

function checkOpenSafetyGate(inp: SafetyGateInput): SafetyGateResult {
  const step = thresholdStep(inp.unit);
  const lower = parseThresholdLower(inp.label);
  const detail: Record<string, number | string | null> = {
    lower, step, local_hour: inp.localHour,
    st_high: inp.stHigh, st_current: inp.stCurrent, awc_max: inp.awcMax,
    met_peak: inp.metPeak, market_peak_lower: inp.marketPeakLower,
  };

  if (lower === null) {
    return { pass: false, reason: "开放低端档位（or below）不适用 NO 高位策略", detail };
  }
  // 没有任何观测温度时拒绝：无法判断安全边际
  if (inp.stHigh === null && inp.awcMax === null) {
    return { pass: false, reason: "无 ST/AWC 观测最高温，无法评估安全边际", detail };
  }

  const observedHigh = Math.max(
    inp.stHigh ?? -Infinity,
    inp.stCurrent ?? -Infinity,
    inp.awcMax ?? -Infinity,
  );
  const marketPeakUpper = inp.marketPeakLower !== null ? inp.marketPeakLower + step : null;
  const refMax = Math.max(
    observedHigh,
    inp.metPeak ?? -Infinity,
    marketPeakUpper ?? -Infinity,
  );
  detail.observed_high = observedHigh;
  detail.ref_max = refMax;

  // 回测（51 城 × 38 天，见 backtest/）：15:00 判定"峰值已过"过早——ST 观测滞后约 1 小时，
  // 大量城市在 15:00 后仍再升 1-3°C，15:00 开仓的亏损占全部亏损的一半以上；推迟到 16:00 后
  // 亏损次数从 23 降到 10，单笔收益率从 0.74% 升到 1.05%。
  // v2.1.1：删除原先的 wellPastPeak 旁路（localHour > yesterdayHighHour + 1）。
  // 该旁路会绕过 POST_PEAK_MIN_HOUR：昨日峰值出现在 12:00 的城市，当地 14:00 就被判为
  // 峰值已过。9/17 样本外数据里它造成 cape-town 20°C（当地 14:00，ST 19、MET 18.3，
  // 实际 20.0）整笔归零，当天单笔收益从 +3.00% 变成 −1.00%。
  // 回测对照（8/29-9/17 走步样本外）：带旁路 251 笔 3 亏 +2.05%/笔；去掉后 147 笔 0 亏 +3.07%/笔。
  // 昨日峰值小时仅作为诊断信息记录，不再参与判定。
  detail.yesterday_high_hour = inp.yesterdayHighHour;
  const postPeak = inp.localHour >= POST_PEAK_MIN_HOUR
    && inp.stCurrent !== null && inp.stHigh !== null && inp.stCurrent <= inp.stHigh;
  detail.phase = postPeak ? "post_peak" : "pre_peak";

  if (postPeak) {
    // 峰值已过：高出已观测最高温 1 个完整档位，且高出 MET 峰值至少半档
    // （回测中 MET 峰值与档位下限仅差 0.1-0.3° 的开仓多次被反超，加半档边际后亏损再减 3 笔）
    const need = observedHigh + step;
    detail.required_lower = need;
    if (lower < need) {
      return { pass: false, reason: `峰值已过但档位下限 ${lower} < 已观测最高温 ${observedHigh} + ${step}`, detail };
    }
    if (inp.metPeak !== null) {
      const metNeed = inp.metPeak + POST_PEAK_MET_MARGIN_STEPS * step;
      detail.met_required_lower = metNeed;
      if (lower < metNeed) {
        return { pass: false, reason: `峰值已过但档位下限 ${lower} < MET 峰值 ${inp.metPeak} + ${POST_PEAK_MET_MARGIN_STEPS} 档`, detail };
      }
    }
    return { pass: true, reason: "post_peak: 高于已观测最高温 1 档且高出 MET 峰值半档", detail };
  }

  // 峰值未到：必须高出所有参考峰值 ≥ 3 个档位
  const need = refMax + 3 * step;
  detail.required_lower = need;
  if (lower < need) {
    return {
      pass: false,
      reason: `峰值未到，档位下限 ${lower} 距参考峰值 ${refMax.toFixed(1)} 不足 3 档（需 ≥ ${need.toFixed(1)}）`,
      detail,
    };
  }
  return { pass: true, reason: "pre_peak: 高于所有参考峰值 ≥ 3 档", detail };
}

function FlipChar({ ch }: { ch: string }) {
  const [state, setState] = useState({ current: ch, leaving: null as string | null });

  // ch 变化时立即更新，无需等 useEffect
  if (ch !== state.current) {
    setState({ current: ch, leaving: state.current });
  }

  // 动画结束后清除 leaving 元素
  useEffect(() => {
    if (state.leaving !== null) {
      const timer = setTimeout(() => {
        setState((prev) => ({ ...prev, leaving: null }));
      }, 400);
      return () => clearTimeout(timer);
    }
  }, [state.leaving]);

  return (
    <span className="flip-slot">
      {state.leaving !== null && (
        <span key={`leave-${state.leaving}`} className="flip-leave">{state.leaving}</span>
      )}
      <span key={`enter-${state.current}`} className="flip-enter">{state.current}</span>
    </span>
  );
}

function FlipTime({ text }: { text: string }) {
  return (
    <span style={{ fontFamily: "'SF Mono','Menlo','Monaco','Consolas','Liberation Mono','Courier New',monospace" }}>
      {text.split("").map((ch, i) => (
        <FlipChar key={i} ch={ch} />
      ))}
    </span>
  );
}

function formatElapsed(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

interface TokenPrice {
  bid: number;
  ask: number;
  mid: number;
  timestamp: number;
  /** 价格来源: gamma(快照) / ws(实时) / rest(回填) */
  source?: "gamma" | "ws" | "rest";
}

type PriceMap = Map<string, TokenPrice>;

/// AWC 实况温度数据
interface AwcData {
  current: number | null;
  max: number | null;
  hourlyTemps: Record<number, number>;
  lat: number | null;
  lon: number | null;
}

/// MET 预报条目
interface MetForecast {
  temp: number;
}

/// ST (Source Temperature) 天气数据
/// 根据 station_url 从 weather.gov 或 wunderground.com 获取
interface StData {
  current: number | null;
  high: number | null;
  low: number | null;
  condition: string | null;
  yesterdayHigh: number | null;
  yesterdayHighHour: number | null;
  yesterdayCondition: string | null;
  fetchedAt: string | null;
}

/// 城市天气数据
interface CityWeather {
  city: string;
  awc: AwcData | null;
  met_forecast: MetForecast[];
  st: StData | null;
}

type WeatherMap = Map<string, CityWeather>;

interface AccountSummary {
  bound: boolean;
  wallet_address: string;
  username: string | null;
  avatar_url: string | null;
  pusd_balance: number | null;
  portfolio_value: number | null;
}

/// 持仓（后端 Position 序列化格式）
interface Position {
  token_id: string;
  market_id: string;
  question: string;
  side: string;
  size: number;
  avg_price: number;
  cur_price: number;
  realized_pnl: number;
  unrealized_pnl: number;
  status: string;
}

/// 空数组常量，保证无持仓城市的 positions prop 引用稳定（避免 CityCard memo 失效）
const EMPTY_POSITIONS: Position[] = [];

/// 空价格 Map 常量，无市场数据城市的 priceMap prop 引用稳定
const EMPTY_PRICE_MAP: Map<string, TokenPrice> = new Map();

/// 交易记录（后端 TradeRecord 序列化格式）
interface TradeRecord {
  id: string;
  market_id: string;
  question: string;
  city: string;
  side: string;
  token_id: string;
  threshold: string;
  entry_price: number;
  size: number;
  cost: number;
  timestamp: string;
  exit_price: number | null;
  exit_timestamp: string | null;
  realized_pnl: number | null;
  status: string;
}

function rowKey(row: FlatRow): string {
  return `${row.city}-${row.market_type}-${row.threshold.label}`;
}

function AnimatedValue({ value, color }: { value: string; color: string }) {
  const [displayValue, setDisplayValue] = useState(value);
  const [phase, setPhase] = useState<"in" | "out">("in");
  const prevValueRef = useRef(value);

  useEffect(() => {
    if (value !== prevValueRef.current) {
      setPhase("out");
      const timer = setTimeout(() => {
        setDisplayValue(value);
        setPhase("in");
        prevValueRef.current = value;
      }, 200);
      return () => clearTimeout(timer);
    }
  }, [value]);

  return (
    <span
      key={phase}
      className={phase === "out" ? "value-fade-out" : "value-fade-in"}
      style={{ fontSize: "14px", fontWeight: 700, color, fontVariantNumeric: "tabular-nums" }}
    >
      {displayValue}
    </span>
  );
}

function AccountInline({ account }: { account: AccountSummary }) {
  const { bound, username, avatar_url, pusd_balance, portfolio_value } = account;

  // 构建从右到左的元素列表，每个元素带递增延迟（右侧先出现，左侧后出现）
  // delay 越大越晚出现 = 越靠左
  const items: { key: string; delay: number; node: React.ReactNode }[] = [];

  if (bound && pusd_balance !== null) {
    items.push({
      key: "cash",
      delay: 0,
      node: (
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <span style={{ color: "#64748b", fontSize: "12px" }}>现金</span>
          <AnimatedValue value={`$${(pusd_balance ?? 0).toFixed(2)}`} color="#34d399" />
        </div>
      ),
    });
  }

  if (bound && portfolio_value !== null) {
    items.push({
      key: "portfolio",
      delay: 0.1,
      node: (
        <>
          <span style={{ color: "#334155", fontSize: "14px" }}>|</span>
          <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
            <span style={{ color: "#64748b", fontSize: "12px" }}>持仓市值</span>
            <AnimatedValue value={`$${portfolio_value.toFixed(2)}`} color="#38bdf8" />
          </div>
        </>
      ),
    });
  }

  if (bound && username) {
    items.push({
      key: "username",
      delay: 0.2,
      node: <span style={{ color: "#e2e8f0" }}>{username}</span>,
    });
  }

  if (bound && avatar_url) {
    items.push({
      key: "avatar",
      delay: 0.3,
      node: (
        <img
          src={avatar_url}
          alt="avatar"
          style={{ width: "24px", height: "24px", borderRadius: "50%", objectFit: "cover", border: "1px solid #334155", flexShrink: 0 }}
        />
      ),
    });
  }

  // DOM 顺序：头像 → 用户名 → Portfolio → Cash（从左到右）
  // 动画顺序：Cash(delay=0) → Portfolio(delay=0.1) → Username(delay=0.2) → Avatar(delay=0.3)
  // 效果：右侧先出现，左侧后出现 = 从右向左淡入
  const domOrder = ["avatar", "username", "portfolio", "cash"];
  const delayMap = new Map(items.map((it) => [it.key, it.delay]));
  const nodeMap = new Map(items.map((it) => [it.key, it.node]));

  return (
    <div style={{ display: "flex", alignItems: "center", gap: "10px", fontSize: "13px" }}>
      {domOrder
        .filter((k) => delayMap.has(k))
        .map((k) => {
          if (k === "portfolio") {
            return (
              <div key={k} className="account-fade-in" style={{ display: "flex", alignItems: "center", gap: "8px", animationDelay: `${delayMap.get(k)}s` }}>
                {nodeMap.get(k)}
              </div>
            );
          }
          return (
            <div key={k} className="account-fade-in" style={{ display: "flex", alignItems: "center", animationDelay: `${delayMap.get(k)}s` }}>
              {nodeMap.get(k)}
            </div>
          );
        })}
    </div>
  );
}

export default function App() {
  const [state, setState] = useState<"loading" | "success" | "error">("loading");
  const [cities, setCities] = useState<CityTempMarkets[]>([]);
  const citiesRef = useRef(cities);
  citiesRef.current = cities;
  const [sqliteCities, setSqliteCities] = useState<CityRow[]>([]);
  const sqliteCitiesRef = useRef(sqliteCities);
  sqliteCitiesRef.current = sqliteCities;
  // 全量城市列表（供 CitySelectorModal 使用，mount 时加载一次）
  const [allCities, setAllCities] = useState<CityRow[]>([]);
  const allCitiesRef = useRef<CityRow[]>([]);
  allCitiesRef.current = allCities;
  // 辅助：根据选中状态从全量列表中筛选出可见城市（UTC 偏移降序）
  const getVisibleCities = useCallback((all: CityRow[], selected: Set<string> | null): CityRow[] => {
    const filtered = selected ? all.filter((c) => selected.has(c.slug)) : all;
    return [...filtered].sort((a, b) => {
      const pa = parseInt(a.utc_offset.replace("UTC", "").replace("+", ""), 10);
      const pb = parseInt(b.utc_offset.replace("UTC", "").replace("+", ""), 10);
      return pb - pa;
    });
  }, []);
  // 城市筛选：选中的 slug 集合，持久化到 localStorage。null = 全选（首次使用）
  const CITY_FILTER_KEY = "woolbrush_city_filter";
  const [selectedCitySlugs, setSelectedCitySlugs] = useState<Set<string> | null>(() => {
    try {
      const raw = localStorage.getItem(CITY_FILTER_KEY);
      if (raw !== null) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) {
          if (parsed.length === 0) return null; // 空数组视为全选，避免刷新后无城市显示
          return new Set(parsed);
        }
      }
    } catch {}
    return null; // 无存储记录或异常 = 全选（首次使用）
  });
  const selectedCitySlugsRef = useRef(selectedCitySlugs);
  selectedCitySlugsRef.current = selectedCitySlugs;
  // 选择变化时同步可见城市列表（只在 allCities 已加载时生效）
  useEffect(() => {
    if (allCitiesRef.current.length > 0) {
      setSqliteCities(getVisibleCities(allCitiesRef.current, selectedCitySlugs));
    }
  }, [selectedCitySlugs, getVisibleCities]);
  // 城市筛选持久化：null 存为特殊标记 "__all__"
  useEffect(() => {
    try {
      const toSave = selectedCitySlugs ? [...selectedCitySlugs] : "__all__";
      localStorage.setItem(CITY_FILTER_KEY, JSON.stringify(toSave));
    } catch {}
  }, [selectedCitySlugs]);
  // 城市筛选操作
  const toggleCity = useCallback((slug: string) => {
    setSelectedCitySlugs((prev) => {
      // null = 全选：以所有城市为 base，移除被点击的城市
      if (prev === null) {
        const next = new Set<string>(allCitiesRef.current.map((c) => c.slug));
        next.delete(slug);
        return next;
      }
      const next = new Set(prev);
      if (next.has(slug)) next.delete(slug);
      else next.add(slug);
      return next;
    });
  }, []);
  const selectAllCities = useCallback(() => setSelectedCitySlugs(null), []);
  const selectNoCities = useCallback(() => setSelectedCitySlugs(new Set()), []);
  const [progress, setProgress] = useState<{ processed: number; total: number }>({ processed: 0, total: 0 });
  const [weatherProgress, setWeatherProgress] = useState<{ processed: number; total: number }>({ processed: 0, total: 0 });
  const [throttledPriceMap, setThrottledPriceMap] = useState<PriceMap>(new Map());
  const throttledPriceMapRef = useRef<PriceMap>(throttledPriceMap);
  throttledPriceMapRef.current = throttledPriceMap;
  const [weatherMap, setWeatherMap] = useState<WeatherMap>(new Map());
  const weatherMapRef = useRef(weatherMap);
  weatherMapRef.current = weatherMap;

  // 全局时间 tick：单一 interval 驱动所有 CityCard 的本地时间刷新，替代 94 个独立 setInterval
  const [timeTick, setTimeTick] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => setTimeTick((t) => t + 1), 10_000);
    return () => clearInterval(timer);
  }, []);

  // 定期强制 V8 GC：WebView2 已带 --expose-gc 标志，每 2 分钟回收一次
  // 配合后端 mimalloc collect 双管齐下，防止 V8 堆碎片化导致内存只增不减
  useEffect(() => {
    const GC_INTERVAL = 120_000; // 2 分钟
    const gc = (window as unknown as { gc?: () => void }).gc;
    const timer = setInterval(() => {
      if (typeof gc === "function") {
        gc();
      }
    }, GC_INTERVAL);
    return () => clearInterval(timer);
  }, []);
  const [askMin, setAskMin] = useState("0.920");
  const [askMax, setAskMax] = useState("0.990");
  const [gmtNow, setGmtNow] = useState(() => new Date());
  const gmtNowRef = useRef(gmtNow);
  gmtNowRef.current = gmtNow;
  const [showSettings, setShowSettings] = useState(false);
  const [showTradeStats, setShowTradeStats] = useState(false);
  const [showCitySelector, setShowCitySelector] = useState(false);
  const [account, setAccount] = useState<AccountSummary | null>(null);

  // 操作栏状态（默认值，启动后从后端文件异步恢复）
  const [orderMode, setOrderMode] = useState<"fixed" | "percent">("percent");
  const [orderAmount, setOrderAmount] = useState("5");
  const [orderPercent, setOrderPercent] = useState("5");
  const [tpAmount, setTpAmount] = useState("0.500");
  const [slAmount, setSlAmount] = useState("0.999");
  const [timeHour, setTimeHour] = useState("11");
  const [scanInterval, setScanInterval] = useState("1.0");
  const [isRunning, setIsRunning] = useState(false);
  // 工具栏参数是否已从后端恢复：load 完成前禁止 save，避免默认值覆盖文件
  const [toolbarLoaded, setToolbarLoaded] = useState(false);

  // 限制输入为 0~1 的数字，最多三位小数
  const clampDecimal = (v: string): string => {
    let s = v.replace(/[^0-9.]/g, "");
    const parts = s.split(".");
    if (parts.length > 2) s = parts[0] + "." + parts.slice(1).join("");
    const [intPart, decPart] = s.split(".");
    let result = intPart === "" ? "" : intPart.replace(/^0+/, "");
    if (result === "") result = "0";
    if (decPart !== undefined) result += "." + decPart.slice(0, 3);
    const num = parseFloat(result);
    if (isNaN(num)) return "0";
    if (num > 1) return "1";
    return result;
  };
  // 限制输入为正整数
  const clampPositiveInt = (v: string): string => {
    const s = v.replace(/[^0-9]/g, "");
    if (s === "") return "1";
    return String(parseInt(s, 10));
  };
  // 限制输入为 0~24 的正整数
  const clampHour = (v: string): string => {
    const s = v.replace(/[^0-9]/g, "");
    if (s === "") return "0";
    const n = parseInt(s, 10);
    if (n > 24) return "24";
    return String(n);
  };

  // ── 启动时从后端文件恢复工具栏参数 ──
  useEffect(() => {
    (async () => {
      try {
        const obj = await invoke<Record<string, any>>("load_toolbar");
        if (obj.askMin != null) setAskMin(obj.askMin);
        if (obj.askMax != null) setAskMax(obj.askMax);
        if (obj.orderMode != null) setOrderMode(obj.orderMode);
        if (obj.orderAmount != null) setOrderAmount(obj.orderAmount);
        if (obj.orderPercent != null) setOrderPercent(obj.orderPercent);
        if (obj.tpAmount != null) setTpAmount(obj.tpAmount);
        if (obj.slAmount != null) setSlAmount(obj.slAmount);
        if (obj.timeHour != null) setTimeHour(obj.timeHour);
        if (obj.scanInterval != null) setScanInterval(obj.scanInterval);
      } catch { /* ignore */ }
      setToolbarLoaded(true);
    })();
  }, []);

  // 工具栏值变更时持久化到后端文件（load 完成后才允许写入）
  useEffect(() => {
    if (!toolbarLoaded) return;
    const data = { orderMode, orderAmount, orderPercent, tpAmount, slAmount, timeHour, askMin, askMax, scanInterval };
    invoke("save_toolbar", { data }).catch(() => { /* ignore */ });
  }, [toolbarLoaded, orderMode, orderAmount, orderPercent, tpAmount, slAmount, timeHour, askMin, askMax, scanInterval]);

  // 柱状图刷新 tick：平仓成功后递增以触发 PnlChart 重新拉取数据
  const [pnlRefreshTick, setPnlRefreshTick] = useState(0);

  // 自动分析引擎状态
  const [analyzingSlug, setAnalyzingSlug] = useState<string | null>(null);
  const [analyzingResult, setAnalyzingResult] = useState<string | null>(null);
  const cityCardRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  const analyzeOneRef = useRef<((slug: string) => Promise<void>) | null>(null);
  const analyzeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hourlyTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const analyzeIdxRef = useRef(0);

  // 工具栏参数 ref（自动开仓 effect 用 interval 读取，不触发重渲染）
  const toolbarRef = useRef({ askMin, askMax, timeHour, orderMode, orderAmount, orderPercent, scanInterval });
  toolbarRef.current = { askMin, askMax, timeHour, orderMode, orderAmount, orderPercent, scanInterval };
  const tpAmountRef = useRef(tpAmount);
  tpAmountRef.current = tpAmount;
  const slAmountRef = useRef(slAmount);
  slAmountRef.current = slAmount;
  const accountRef = useRef(account);
  accountRef.current = account;
  const [progressBarVisible, setProgressBarVisible] = useState(true);
  const [progressBarOpacity, setProgressBarOpacity] = useState(1);



  // 已买入置顶的行 key 集合
  const [pinnedKeys, setPinnedKeys] = useState<Set<string>>(new Set());
  const pinnedKeysRef = useRef(pinnedKeys);
  pinnedKeysRef.current = pinnedKeys;
  // 记录最近卖出的 key，防止 refreshAccount 因链上延迟把已平仓的行重新置顶
  const recentSoldKeysRef = useRef<Set<string>>(new Set());
  // Toast 通知
  const [toasts, setToasts] = useState<ToastMsg[]>([]);
  const toastIdRef = useRef(0);

  const showToast = useCallback((type: "success" | "error", message: string) => {
    const id = ++toastIdRef.current;
    setToasts((prev) => [...prev, { id, type, message }]);
    setTimeout(() => {
      setToasts((prev) => prev.filter((t) => t.id !== id));
    }, 5000);
  }, []);

  // 刷新账户概要 + 同步持仓到 pinnedKeys（10秒防抖，等待链上数据同步）
  const refreshAccountTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const refreshAccount = useCallback(async () => {
    // 防抖：10秒内多次调用只执行最后一次，统一等待链上数据同步
    if (refreshAccountTimerRef.current) {
      clearTimeout(refreshAccountTimerRef.current);
    }
    refreshAccountTimerRef.current = setTimeout(async () => {
      try {
        const [summary, apiPositions] = await Promise.all([
          invoke<AccountSummary>("get_account_summary"),
          invoke<Position[]>("get_positions"),
        ]);
        setAccount(summary);

        // merge：API 返回的持仓可能因链上同步延迟 avg_price=0，
        // 保留本地 fillPositionFromRecord 写入的正确 entry_price/size。
        // 同时保留本地有但 API 尚未返回的新开仓。
        const localMap = new Map(positionsRef.current.map((p) => [p.token_id, p]));
        const merged = apiPositions.map((api) => {
          const local = localMap.get(api.token_id);
          if (local && (api.avg_price === 0 || api.size === 0)) {
            return { ...api, avg_price: local.avg_price, size: local.size, cur_price: local.cur_price };
          }
          return api;
        });
        // 保留本地有但 API 未返回的持仓（链上延迟未上链）
        const apiTokenIds = new Set(apiPositions.map((p) => p.token_id));
        for (const [tokenId, localPos] of localMap) {
          if (!apiTokenIds.has(tokenId)) {
            merged.push(localPos);
          }
        }

        positionsRef.current = merged;
        setPositionsState(merged);

        // 用持仓的 token_id 匹配行数据，设置 pinnedKeys
        // 买入：保留当前已有 pinned key 防止链上延迟丢失
        // 卖出：跳过 recentSoldKeys 中记录的 key，防止链上延迟导致已平仓行被重新置顶
        const heldTokenIds = new Set(merged.map((p) => p.token_id));
        setPinnedKeys((prev) => {
          const next = new Set<string>();
          // 保留前端已确认的 pinned key（排除最近卖出的）
          for (const key of prev) {
            if (!recentSoldKeysRef.current.has(key)) {
              next.add(key);
            }
          }
          // 合并后端确认的持仓（排除最近卖出的）
          for (const row of flattenAndFilter(citiesRef.current)) {
            const key = rowKey(row);
            if (recentSoldKeysRef.current.has(key)) continue;
            if (heldTokenIds.has(row.threshold.no_token_id)) {
              next.add(key);
            }
          }
          return next;
        });
      } catch (e) {
        console.error("Failed to refresh account:", e);
      }
    }, 10_000);
  }, []);

  // 用开仓返回的 TradeRecord 立即填充 positionsState，无需等待链上同步
  const fillPositionFromRecord = useCallback((record: TradeRecord) => {
    const newPos: Position = {
      token_id: record.token_id,
      market_id: record.market_id,
      question: record.question,
      side: record.side,
      size: record.size,
      avg_price: record.entry_price,
      cur_price: record.entry_price,
      realized_pnl: 0,
      unrealized_pnl: 0,
      status: "open",
    };
    setPositionsState((prev) => {
      const idx = prev.findIndex((p) => p.token_id === record.token_id);
      if (idx >= 0) {
        const next = [...prev];
        next[idx] = newPos;
        return next;
      }
      return [...prev, newPos];
    });
    positionsRef.current = [...positionsRef.current.filter((p) => p.token_id !== record.token_id), newPos];
  }, []);

  // 平仓后立即从 positionsState/positionsRef 移除持仓，UI 即时更新
  const removePosition = useCallback((tokenId: string) => {
    positionsRef.current = positionsRef.current.filter((p) => p.token_id !== tokenId);
    setPositionsState((prev) => prev.filter((p) => p.token_id !== tokenId));
  }, []);

  // 手动开仓（双击温度档位触发）
  const handleAnalyzeOne = useCallback(async (slug: string) => {
    const fn = analyzeOneRef.current;
    if (!fn) {
      showToast("error", "分析功能未就绪，请稍后重试");
      return;
    }
    await fn(slug);
  }, []);

  const handleManualOpen = useCallback(async (threshold: TempThreshold, city: string, cityTz: string, askPrice: number) => {
    const { orderMode: om, orderAmount: oa, orderPercent: op } = toolbarRef.current;
    const acct = accountRef.current;
    const availableCash = acct?.pusd_balance ?? 0;
    const totalValue = acct?.portfolio_value ?? 0;
    const MIN_ORDER_COST = 5 * 0.995;
    let amount: number;
    if (om === "fixed") {
      amount = parseFloat(oa) || 0;
    } else {
      amount = totalValue * (parseFloat(op) || 0) / 100;
      if (amount > 0 && amount < MIN_ORDER_COST && availableCash >= MIN_ORDER_COST) {
        amount = MIN_ORDER_COST;
      }
    }
    if (amount <= 0) {
      showToast("error", "下单金额必须大于 0");
      return;
    }
    const opp = {
      market_id: threshold.market_id,
      question: threshold.question,
      city,
      city_tz: cityTz,
      side: "NO" as const,
      token_id: threshold.no_token_id,
      threshold: threshold.label,
      current_price: askPrice,
      expected_profit: 0,
      confirmed_threshold: null,
      strategy_label: "Manual" as const,
    };
    try {
      const record = await invoke<TradeRecord>("open_position", { opp, amount });
      fillPositionFromRecord(record);
      const key = `${city}-highest-${threshold.label}`;
      setPinnedKeys((prev) => new Set(prev).add(key));
      showToast("success", `已开仓: ${cityDisplayName(city)} ${threshold.label} @ ${askPrice.toFixed(3)}`);
      refreshAccount();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      showToast("error", `开仓失败: ${cityDisplayName(city)} ${threshold.label} - ${msg}`);
    }
  }, [fillPositionFromRecord, showToast, refreshAccount]);

  // 手动平仓（双击平仓按钮触发）
  const handleManualClose = useCallback(async (tokenId: string) => {
    try {
      await invoke<string>("close_position", { tokenId });
      // 立即移除持仓
      removePosition(tokenId);
      setPnlRefreshTick((t) => t + 1);
      // 从 pinnedKeys 移除
      for (const row of flattenAndFilter(citiesRef.current)) {
        if (row.threshold.no_token_id === tokenId) {
          const rk = rowKey(row);
          setPinnedKeys((prev) => {
            const next = new Set(prev);
            next.delete(rk);
            return next;
          });
          recentSoldKeysRef.current.add(rk);
          setTimeout(() => { recentSoldKeysRef.current.delete(rk); }, 15000);
          break;
        }
      }
      showToast("success", `已平仓: ${tokenId.slice(0, 8)}...`);
      refreshAccount();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      showToast("error", `平仓失败: ${msg}`);
    }
  }, [removePosition, showToast, refreshAccount]);

  // 启动持仓监控器（随引擎启动）
  const startMonitor = useCallback(async () => {
    try {
      // tpAmount = 止损价, slAmount = 止盈价（历史命名，语义相反）
      const stopLossPrice = parseFloat(tpAmountRef.current) || 0;
      const takeProfitPrice = parseFloat(slAmountRef.current) || 0;
      await invoke("start_position_monitor", { stopLossPrice, takeProfitPrice });
      showToast("success", `持仓监控已启动 (止损≤${stopLossPrice} 止盈≥${takeProfitPrice})`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      showToast("error", `持仓监控启动失败: ${msg}`);
    }
  }, [showToast]);

  // 停止持仓监控器（随引擎停止）
  const stopMonitor = useCallback(async () => {
    try {
      await invoke("stop_position_monitor");
    } catch (e) {
      console.error("stop_position_monitor failed:", e);
    }
  }, []);


  // 节流后的 priceMap：避免 WS 频繁推送导致排序动画重叠
  const priceUpdateBatchRef = useRef<Map<string, TokenPrice>>(new Map());
  // WS 事件时间戳，轮询兜底据此判断是否需要拉取（WS 正常时不轮询，避免双重更新）
  const lastWsEventRef = useRef<number>(Date.now());
  const [runSeconds, setRunSeconds] = useState(0);
  const [exitAnim, setExitAnim] = useState(false);

  // Start 按钮单击/双击区分：
  //   单击 → 整点遍历（等待整点 → 遍历 → 循环）
  //   双击 → 单次遍历（立即遍历一轮 → 自动停止）
  const singleRunRef = useRef(false);        // true = 单次模式，遍历完自动停止
  const clickTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isRunningRef = useRef(false);         // 同步 isRunning 到 ref，避免闭包陷阱
  isRunningRef.current = isRunning;

const handleUpdateCities = useCallback(async () => {
    try {
      const result = await invoke<{
        discovered: string[];
        added: string[];
        removed: string[];
        station_changed: string[];
        total_before: number;
        total_after: number;
      }>("update_cities");
      const parts: string[] = [];
      if (result.removed.length > 0) {
        parts.push(`removed ${result.removed.length}: ${result.removed.join(", ")}`);
      }
      if (result.added.length > 0) {
        parts.push(`added ${result.added.length}: ${result.added.join(", ")}`);
      }
      if (result.station_changed.length > 0) {
        parts.push(`station changed ${result.station_changed.length}: ${result.station_changed.join(", ")}`);
      }
      // 清理城市筛选中的已删除残留 slug
      if (result.removed.length > 0) {
        const removedSet = new Set(result.removed);
        setSelectedCitySlugs((prev) => {
          if (prev === null) return prev;
          const next = new Set([...prev].filter((s) => !removedSet.has(s)));
          return next.size === prev.size ? prev : next;
        });
      }
      if (parts.length > 0) {
        showToast("success", `城市列表已更新: ${result.total_before} -> ${result.total_after} (${parts.join("; ")})`);
      } else {
        showToast("success", `城市列表已是最新: ${result.total_before} 个城市, 发现 ${result.discovered.length} 个`);
      }
      // 刷新全量城市列表 + 可见列表（保持用户的城市筛选）
      const refreshed = await invoke<CityRow[]>("get_cities");
      setAllCities(refreshed);
      setSqliteCities(getVisibleCities(refreshed, selectedCitySlugsRef.current));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      showToast("error", `更新城市列表失败: ${msg}`);
    }
  }, [showToast, getVisibleCities]);

  // 气象站编号修改后，同步更新前端 allCities / sqliteCities 中的对应字段
  const handleStationCodeUpdated = useCallback(
    (slug: string, newCode: string | null) => {
      setAllCities((prev) =>
        prev.map((c) =>
          c.slug === slug ? { ...c, station_code: newCode } : c
        )
      );
      setSqliteCities((prev) =>
        prev.map((c) =>
          c.slug === slug ? { ...c, station_code: newCode } : c
        )
      );
    },
    []
  );

  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const isScrollingRef = useRef(false);
  const scrollTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 排序节流：避免 WS 频繁推送导致动画重叠抖动
  const sortTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // 流式加载：收集按城市分组的 token_id
  const cityTokenGroupsRef = useRef<Map<string, string[]>>(new Map());

  // 持久 city-loaded 监听器 unlisten 函数（跨天重载也通过此监听器更新）
  const cityUnlistenRef = useRef<UnlistenFn | null>(null);

  // 防重入：StrictMode / HMR 可能连续触发 loadData
  const loadingRef = useRef(false);

  // 自动开仓：本轮已尝试的 key 集合，防止重复开仓
  const autoTradeAttemptedRef = useRef<Set<string>>(new Set());
  // 自动平仓：本轮已触发平仓的 key 集合，防止重复
  const autoCloseAttemptedRef = useRef<Set<string>>(new Set());
  // 持仓快照 ref，供闭包读取最新值
  const positionsRef = useRef<Position[]>([]);
  const [positionsState, setPositionsState] = useState<Position[]>([]);

  // 持仓统计：总持仓数、总盈利额、总盈利率
  const positionStats = useMemo(() => {
    const count = positionsState.length;
    const totalPnl = positionsState.reduce((sum, p) => sum + (p.unrealized_pnl + p.realized_pnl), 0);
    const totalCost = positionsState.reduce((sum, p) => sum + p.avg_price * p.size, 0);
    const pnlPct = totalCost > 0 ? (totalPnl / totalCost) * 100 : 0;
    return { count, totalPnl, pnlPct };
  }, [positionsState]);

  // GMT 时钟（降频到 10 秒，仅用于底栏显示和遍历引擎的时间判断）
  useEffect(() => {
    const timer = setInterval(() => setGmtNow(new Date()), 10_000);
    return () => clearInterval(timer);
  }, []);

  // 持久 temperature://city-loaded 监听器
  // 初始加载和后端跨天重载都通过此监听器更新 cities，无需重建遍历引擎
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    (async () => {
      unlisten = await listen<CityTempMarkets>("temperature://city-loaded", (event) => {
        const cityData = event.payload;
        // 按城市分组收集 token_ids，用于跨天后重启 WS
        if (cityData.highest) {
          const tids = cityData.highest.thresholds.map((t) => t.no_token_id);
          if (tids.length > 0) {
            cityTokenGroupsRef.current.set(cityData.city, tids);
          } else {
            cityTokenGroupsRef.current.delete(cityData.city);
          }
        } else {
          cityTokenGroupsRef.current.delete(cityData.city);
        }
        setCities((prev) => {
          const idx = prev.findIndex((c) => c.city === cityData.city);
          if (idx >= 0) {
            const next = [...prev];
            next[idx] = cityData;
            return next;
          }
          return [...prev, cityData];
        });
      });
      cityUnlistenRef.current = unlisten;
    })();
    return () => {
      if (unlisten) unlisten();
      cityUnlistenRef.current = null;
    };
  }, []);

  // 后端跨天检测：后端已自动重载跨天城市的市场数据并通过 temperature://city-loaded 推送
  // 前端需：刷新天气 + 重启 WS 订阅新 token_ids + 同步持仓 + 重放缓存价格
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    (async () => {
      unlisten = await listen<{ count: number; cities: Array<{ city: string; tz: string; old_date: string; new_date: string }> }>(
        "date://rollover",
        async (event) => {
          const { cities } = event.payload;
          // [date rollover] city reload logged via backend tracing

          // 1. 刷新天气数据（批量）
          if (cities.length > 0) {
            invoke("fetch_weather_batch_cmd", { citySlugs: cities.map((c) => c.city) }).catch((e) => {
              console.warn(`[date rollover] Failed to refresh weather:`, e);
            });
          }

          // 2. 重启 WS 价格流：跨天后 token_ids 已变化，需重新订阅
          //    先快照旧缓存价格（stop 会清空缓存），再停旧 WS、启动新 WS
          let oldCached: { token_id: string; bid: number; ask: number; mid: number; timestamp: number }[] = [];
          try {
            oldCached = await invoke<{ token_id: string; bid: number; ask: number; mid: number; timestamp: number }[]>("get_cached_prices");
          } catch { /* ignore */ }

          try {
            await invoke("stop_price_stream");
          } catch (e) {
            console.warn("[date rollover] stop_price_stream failed:", e);
          }
          // 等待 200ms 确保旧连接清理完毕
          await new Promise((r) => setTimeout(r, 200));

          const groups = Array.from(cityTokenGroupsRef.current.entries()).map(([city, token_ids]) => ({ city, token_ids }));
          if (groups.length > 0) {
            try {
              await invoke("start_price_stream", { cityGroups: groups });
              // [date rollover] WS restart confirmed

              // 3. 重放价格：先回填旧缓存中仍然有效的 token 价格，再读新 WS 缓存
              if (oldCached.length > 0) {
                const validTokenIds = new Set(groups.flatMap((g) => g.token_ids));
                setThrottledPriceMap((prev) => {
                  const next = new Map(prev);
                  for (const p of oldCached) {
                    if (validTokenIds.has(p.token_id)) next.set(p.token_id, p);
                  }
                  return next;
                });
              }
              // 新 WS 连接后 book snapshot 会异步写入缓存，延迟读取补齐
              setTimeout(async () => {
                try {
                  const fresh = await invoke<{ token_id: string; bid: number; ask: number; mid: number; timestamp: number }[]>("get_cached_prices");
                  if (fresh && fresh.length > 0) {
                    setThrottledPriceMap((prev) => {
                      const next = new Map(prev);
                      for (const p of fresh) next.set(p.token_id, p);
                      return next;
                    });
                  }
                } catch { /* ignore */ }
              }, 3000);
            } catch (e) {
              console.error("[date rollover] restart WS failed:", e);
            }
          }

          // 4. 同步持仓状态（新一天的市场可能有不同的 token_ids）
          try {
            const [summary, positions] = await Promise.all([
              invoke<AccountSummary>("get_account_summary"),
              invoke<Position[]>("get_positions"),
            ]);
            setAccount(summary);
            positionsRef.current = positions;
            setPositionsState(positions);
            const heldTokenIds = new Set(positions.map((p) => p.token_id));
            const next = new Set<string>();
            for (const row of flattenAndFilter(citiesRef.current)) {
              if (heldTokenIds.has(row.threshold.no_token_id)) {
                next.add(rowKey(row));
              }
            }
            setPinnedKeys(next);
          } catch (e) {
            console.error("[date rollover] sync positions failed:", e);
          }
        }
      );
    })();
    return () => { if (unlisten) unlisten(); };
  }, []);

  // 运行计时器
  useEffect(() => {
    if (!isRunning) return;
    const timer = setInterval(() => setRunSeconds((s) => s + 1), 1000);
    return () => clearInterval(timer);
  }, [isRunning]);

  // ─── Start 按钮启动逻辑 ─────────────────────────────────────
  // 单击 → 整点遍历（等待整点 → 遍历 → 循环）
  // 双击 → 单次遍历（立即遍历一轮 → 自动停止）
  // 运行中点击 → 停止

  const doStart = useCallback((singleRun: boolean) => {
    singleRunRef.current = singleRun;
    setExitAnim(false);
    setRunSeconds(0);
    autoTradeAttemptedRef.current.clear();
    autoCloseAttemptedRef.current.clear();
    setIsRunning(true);
    startMonitor();
  }, [startMonitor]);

  const doStop = useCallback(() => {
    setExitAnim(true);
    stopMonitor();
  }, [stopMonitor]);

  const handleStartClick = useCallback(() => {
    // 运行中 → 停止
    if (isRunningRef.current) {
      // 如果有 pending 的单击定时器，取消它
      if (clickTimerRef.current) {
        clearTimeout(clickTimerRef.current);
        clickTimerRef.current = null;
      }
      doStop();
      return;
    }

    // 未运行 → 等待 250ms 判断单击还是双击
    if (clickTimerRef.current) {
      // 第二次点击（双击）→ 单次遍历
      clearTimeout(clickTimerRef.current);
      clickTimerRef.current = null;
      doStart(true);   // singleRun = true
    } else {
      // 第一次点击 → 等 250ms，无第二次点击则单击
      clickTimerRef.current = setTimeout(() => {
        clickTimerRef.current = null;
        doStart(false);  // singleRun = false → 整点遍历
      }, 250);
    }
  }, [doStart, doStop]);

  // 加载账户概要
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const summary = await invoke<AccountSummary>("get_account_summary");
        if (!cancelled) setAccount(summary);
      } catch (e) {
        console.error("Account summary failed:", e);
      }
    })();
    return () => { cancelled = true; };
  }, []);

  // 流式加载市场数据 + 启动 WS 价格流
  // city-loaded 监听器已提取为持久 useEffect，此处不再注册/注销
  // citySlugs: 可选的城市过滤列表。null = 加载全部，string[] = 只加载指定城市
  const loadData = useCallback(async (citySlugs: string[] | null = null) => {
    if (loadingRef.current) return;
    loadingRef.current = true;

    const progressUnlisten = await listen<{ processed: number; total: number }>("temperature://progress", (event) => {
      setProgress(event.payload);
    });

    // 先 await 注册 all-loaded 监听器，确保不遗漏事件
    let allLoadedResolve!: () => void;
    let allLoadedReject!: (e: Error) => void;
    const allLoadedPromise = new Promise<void>((resolve, reject) => {
      allLoadedResolve = resolve;
      allLoadedReject = reject;
    });

    // 超时保护：60 秒后若 all-loaded 事件未触发，自动 reject
    const allLoadedTimeout = setTimeout(() => {
      allLoadedReject(new Error("Stream temperature cities timeout"));
    }, 60000);

    const allLoadedUnlisten = await listen("temperature://all-loaded", async () => {
      clearTimeout(allLoadedTimeout);
      // 市场进度走到头
      setProgress((prev) => ({ processed: prev.total, total: prev.total }));
      // 不立即淡出，等天气也加载完后再淡出
      setState("success");
      // WS 价格流由后端在 stream_temperature_cities 完成后自动启动
      // 重放后端缓存的 prices：WS 在城市流式加载期间已收到 book 快照，
      // 但前端可能因 StrictMode unlisten/re-mount 丢失了早期事件
      try {
        const cached = await invoke<{ token_id: string; bid: number; ask: number; mid: number; timestamp: number }[]>("get_cached_prices");
        if (cached && cached.length > 0) {
          setThrottledPriceMap((prev) => {
            let changed = false;
            const next = new Map(prev);
            for (const p of cached) {
              const old = next.get(p.token_id);
              if (!old || old.bid !== p.bid || old.ask !== p.ask || old.mid !== p.mid) {
                next.set(p.token_id, p);
                changed = true;
              }
            }
            return changed ? next : prev;
          });
        }
      } catch (e) {
        console.warn("Failed to replay cached prices:", e);
      }
      // 数据加载完成后同步持仓状态
      // 等待 React state 更新完成，确保 citiesRef.current 拿到最新 cities
      await new Promise((r) => setTimeout(r, 100));
      try {
        const [summary, positions] = await Promise.all([
          invoke<AccountSummary>("get_account_summary"),
          invoke<Position[]>("get_positions"),
        ]);
        setAccount(summary);
        positionsRef.current = positions;
        setPositionsState(positions);
        const heldTokenIds = new Set(positions.map((p) => p.token_id));

        const next = new Set<string>();
        for (const row of flattenAndFilter(citiesRef.current)) {
          if (heldTokenIds.has(row.threshold.no_token_id)) {
            next.add(rowKey(row));
          }
        }
        setPinnedKeys(next);
      } catch (e) {
        console.error("Failed to sync positions after load:", e);
      }
      allLoadedResolve();
    });

    try {
      setState("loading");
      setCities([]);
      setThrottledPriceMap(new Map());
      cityTokenGroupsRef.current = new Map();
      setProgress({ processed: 0, total: 0 });
      setWeatherProgress({ processed: 0, total: 0 });
      setProgressBarVisible(true);
      setProgressBarOpacity(1);

      // 加载 SQLite 城市列表（不阻塞市场数据流式加载）
      invoke<CityRow[]>("get_cities")
        .then((rows) => {
          // 保存全量列表 + 按当前筛选生成可见列表（UTC 偏移降序）
          setAllCities(rows);
          setSqliteCities(getVisibleCities(rows, selectedCitySlugsRef.current));
          setState("success");
        })
        .catch((e) => {
          console.error("Failed to load cities from DB:", e);
        });

      // 停止旧的 WS 价格流
      try {
        await invoke("stop_price_stream");
      } catch (e) {
        console.error("Failed to stop price stream:", e);
      }

      // 调用流式命令，后端会逐城市通过事件推送（传入城市过滤）
      await invoke("stream_temperature_cities", { citySlugs });
      // 等待 all-loaded 事件
      await allLoadedPromise;
    } catch (e) {
      console.error("Failed to stream temperature cities:", e);
      setState("error");
    } finally {
      progressUnlisten();
      allLoadedUnlisten();
      loadingRef.current = false;
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (!cancelled) {
        // 按当前城市筛选加载（首次使用为全选 null）
        const slugs = selectedCitySlugsRef.current
          ? Array.from(selectedCitySlugsRef.current)
          : null;
        await loadData(slugs);
      }
    })();
    return () => { cancelled = true; };
  }, [loadData]);

  // 加载天气数据（AWC 实况 + MET 预报）
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let progressUnlisten: UnlistenFn | null = null;
    let allLoadedUnlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        unlisten = await listen<CityWeather>("weather://city-loaded", (event) => {
          const w = event.payload;
          setWeatherMap((prev) => {
            const next = new Map(prev);
            next.set(w.city, w);
            return next;
          });
        });
        progressUnlisten = await listen<{ processed: number; total: number }>("weather://progress", (event) => {
          setWeatherProgress(event.payload);
        });
        allLoadedUnlisten = await listen("weather://all-loaded", async () => {
          setWeatherProgress((prev) => ({ processed: prev.total, total: prev.total }));
          await new Promise((r) => setTimeout(r, 3000));
          setProgressBarOpacity(0);
          await new Promise((r) => setTimeout(r, 600));
          setProgressBarVisible(false);
        });
        if (cancelled) { unlisten(); progressUnlisten(); allLoadedUnlisten(); return; }
        // 延迟 3 秒再调用，避免与市场数据流式加载竞争
        await new Promise((r) => setTimeout(r, 3000));
        if (cancelled) return;
        // 仅抓取选中城市的天气
        const weatherSlugs = selectedCitySlugsRef.current
          ? Array.from(selectedCitySlugsRef.current)
          : null;
        await invoke("fetch_weather_batch_cmd", { citySlugs: weatherSlugs });
      } catch (e) {
        console.error("Failed to fetch weather:", e);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      if (progressUnlisten) progressUnlisten();
      if (allLoadedUnlisten) allLoadedUnlisten();
    };
  }, []);

  // 监听 WS 价格更新
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const fn = await listen<{ token_id: string; bid: number; ask: number; mid: number; timestamp: number }[]>("price://update-batch", (event) => {
          const batch = event.payload;
          if (!batch || batch.length === 0) return;
          lastWsEventRef.current = Date.now();
          // 批量收集，延迟刷新 throttledPriceMap（触发排序动画）
          for (const { token_id, bid, ask, mid, timestamp } of batch) {
            priceUpdateBatchRef.current.set(token_id, { bid, ask, mid, timestamp });
          }
          if (!sortTimerRef.current) {
            sortTimerRef.current = setTimeout(() => {
              sortTimerRef.current = null;
              const batch = priceUpdateBatchRef.current;
              priceUpdateBatchRef.current = new Map();
              if (batch.size > 0) {
                setThrottledPriceMap((prev) => {
                  let changed = false;
                  const next = new Map(prev);
                  batch.forEach((v, k) => {
                    const old = next.get(k);
                    if (!old || old.bid !== v.bid || old.ask !== v.ask || old.mid !== v.mid) {
                      next.set(k, v);
                      changed = true;
                    }
                  });
                  return changed ? next : prev; // 无变化时保留引用
                });
              }
            }, 3000);
          }
        });
        if (cancelled) { fn(); return; }
        unlisten = fn;

        // 监听器注册完成后，拉取后端缓存的 prices 重放
        // （弥补 StrictMode unlisten → re-mount 窗口期丢失的 WS 事件）
        try {
          const cached = await invoke<{ token_id: string; bid: number; ask: number; mid: number; timestamp: number }[]>("get_cached_prices");
          if (cached && cached.length > 0) {
            setThrottledPriceMap((prev) => {
              let changed = false;
              const next = new Map(prev);
              for (const p of cached) {
                const old = next.get(p.token_id);
                if (!old || old.bid !== p.bid || old.ask !== p.ask || old.mid !== p.mid) {
                  next.set(p.token_id, p);
                  changed = true;
                }
              }
              return changed ? next : prev;
            });
          }
        } catch (e) {
          console.warn("Failed to get cached prices:", e);
        }
      } catch (e) {
        console.error("Failed to listen price://update-batch:", e);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // 轮询兜底：仅当 WS 事件超过 30 秒未到达时才拉取一次缓存，避免与事件路径叠加
  useEffect(() => {
    let active = true;
    const POLL_INTERVAL = 15_000;         // 每 15 秒检查一次
    const WS_STALE_THRESHOLD = 30_000;    // 超过 30 秒没有 WS 事件才拉取
    const poll = async () => {
      if (!active) return;
      const now = Date.now();
      if (now - lastWsEventRef.current < WS_STALE_THRESHOLD) return; // WS 正常，跳过
      try {
        const cached = await invoke<{ token_id: string; bid: number; ask: number; mid: number; timestamp: number }[]>("get_cached_prices");
        if (!active || !cached || cached.length === 0) return;
        setThrottledPriceMap((prev) => {
          let changed = false;
          const next = new Map(prev);
          for (const p of cached) {
            const old = next.get(p.token_id);
            if (!old || old.bid !== p.bid || old.ask !== p.ask || old.mid !== p.mid) {
              next.set(p.token_id, p);
              changed = true;
            }
          }
          return changed ? next : prev; // 无变化时保留引用，跳过重渲染
        });
      } catch {
        // 后端可能尚未就绪，静默重试
      }
    };
    const id = setInterval(poll, POLL_INTERVAL);
    return () => { active = false; clearInterval(id); };
  }, []);

  // 监听 WS 连接状态
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const fn = await listen<{ connected: boolean }>("ws://status", () => {
          // WS status changes handled implicitly via price updates
        });
        if (cancelled) { fn(); return; }
        unlisten = fn;
      } catch (e) {
        console.error("Failed to listen ws://status:", e);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // 监听 WS 重连完成：触发 REST backfill 补齐缺失价格
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const fn = await listen("ws://reconnected", async () => {
          // WS reconnected - triggering REST backfill silently
          const tokenIds = Array.from(cityTokenGroupsRef.current.values()).flat();
          if (tokenIds.length > 0) {
            try {
              await invoke("backfill_prices", { tokenIds });
            } catch (e) {
              console.error("backfill_prices failed:", e);
            }
          }
        });
        if (cancelled) { fn(); return; }
        unlisten = fn;
      } catch (e) {
        console.error("Failed to listen ws://reconnected:", e);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // 监听持仓监控器自动平仓事件
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const fn = await listen<{
          token_id: string;
          city: string;
          size: number;
          exit_price: number;
          proceeds: number;
          realized_pnl: number;
          reason: string;
        }>("position://auto-closed", async (event) => {
          const { token_id, city, exit_price, realized_pnl, reason } = event.payload;
          // 移除持仓 UI
          removePosition(token_id);
          setPnlRefreshTick((t) => t + 1);
          // 从 pinnedKeys 移除
          for (const row of flattenAndFilter(citiesRef.current)) {
            if (row.threshold.no_token_id === token_id) {
              const rk = rowKey(row);
              setPinnedKeys((prev) => {
                const next = new Set(prev);
                next.delete(rk);
                return next;
              });
              recentSoldKeysRef.current.add(rk);
              setTimeout(() => { recentSoldKeysRef.current.delete(rk); }, 15000);
              break;
            }
          }
          const pnlStr = realized_pnl >= 0 ? `+${realized_pnl.toFixed(2)}` : realized_pnl.toFixed(2);
          const reasonLabel = reason === "stop_loss" ? "止损" : reason === "take_profit" ? "止盈" : reason;
          showToast("success", `${city} ${reasonLabel}平仓 @${exit_price.toFixed(3)} 盈亏 ${pnlStr}`);
          refreshAccount();
        });
        if (cancelled) { fn(); return; }
        unlisten = fn;
      } catch (e) {
        console.error("Failed to listen position://auto-closed:", e);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [removePosition, showToast, refreshAccount]);


  // 持仓按城市 slug 预分组（避免每次渲染为每个城市执行 filter）
  const positionsByCity = useMemo(() => {
    // 建 token_id → slug 映射
    const tokenToSlug = new Map<string, string>();
    for (const c of cities) {
      if (c.highest) {
        for (const t of c.highest.thresholds) {
          tokenToSlug.set(t.no_token_id, c.city);
          tokenToSlug.set(t.yes_token_id, c.city);
        }
      }
    }
    const map = new Map<string, Position[]>();
    for (const p of positionsState) {
      const slug = tokenToSlug.get(p.token_id);
      if (slug) {
        const arr = map.get(slug);
        if (arr) arr.push(p);
        else map.set(slug, [p]);
      }
    }
    return map;
  }, [cities, positionsState]);

  // 按城市拆分 priceMap：每城市只含该城市 token 的价格子集
  // 引用稳定性：仅当该城市有价格变化时才生成新 Map，使 CityCard memo 真正生效
  const cityPriceMaps = useMemo(() => {
    const result = new Map<string, Map<string, TokenPrice>>();
    for (const c of cities) {
      if (!c.highest?.thresholds.length) continue;
      const sub = new Map<string, TokenPrice>();
      let hasAny = false;
      for (const t of c.highest.thresholds) {
        const p = throttledPriceMap.get(t.no_token_id);
        if (p) {
          sub.set(t.no_token_id, p);
          hasAny = true;
        }
        const yp = throttledPriceMap.get(t.yes_token_id);
        if (yp) {
          sub.set(t.yes_token_id, yp);
          hasAny = true;
        }
      }
      // 有持仓的 token 也需要包含（持仓 token_id 可能不在 thresholds 中）
      const cityPositions = positionsByCity.get(c.city);
      if (cityPositions) {
        for (const pos of cityPositions) {
          const p = throttledPriceMap.get(pos.token_id);
          if (p) {
            sub.set(pos.token_id, p);
            hasAny = true;
          }
        }
      }
      result.set(c.city, hasAny ? sub : EMPTY_PRICE_MAP);
    }
    return result;
  }, [cities, throttledPriceMap, positionsByCity]);

  // cityPriceMaps 的稳定 ref，供分析循环等非渲染路径读取
  const cityPriceMapsRef = useRef(cityPriceMaps);
  cityPriceMapsRef.current = cityPriceMaps;



  // 自动分析引擎：isRunning 时按排序顺序逐个城市处理
  // 遍历到哪个城市就处理哪个城市的开仓和平仓，高亮当前行并滚动到屏幕中间
  useEffect(() => {
    let cancelled = false;

    // 检查城市的温度档位是否价格就绪：
    // 1. 收集所有非灰色（有完整 bid/ask）、未结算的档位
    // 2. 排除其中卖一价最低的档位（通常为已高度确认的档位）
    // 3. 其余档位中至少有一个卖一价在 BID/ASK 区间内，才算价格就绪
    const isCityPriceReady = (slug: string): { ready: boolean; reason?: string } => {
      const md = citiesRef.current.find((c) => c.city === slug);
      if (!md?.highest?.thresholds.length) return { ready: false };

      const priceMap = throttledPriceMapRef.current;
      const bidAskMin = parseFloat(toolbarRef.current.askMin);
      const bidAskMax = parseFloat(toolbarRef.current.askMax);
      if (isNaN(bidAskMin) || isNaN(bidAskMax)) return { ready: false };

      // 检查是否至少有 1 个有效档位（非灰色、未结算）的卖一价在 BID/ASK 区间内
      let inRangeCount = 0;
      for (const t of md.highest.thresholds) {
        const price = priceMap.get(t.no_token_id);
        if (price?.bid == null || price?.ask == null) continue; // 灰色，跳过
        const mid = price.mid ?? t.no_price;
        if (mid >= 0.999) continue; // 已结算，跳过
        if (price.ask >= bidAskMin && price.ask <= bidAskMax) {
          inRangeCount++;
        }
      }

      if (inRangeCount === 0) {
        return { ready: false, reason: "无档位卖一价在 BID/ASK 区间内" };
      }

      return { ready: true };
    };

    const analyzeNext = async () => {
      if (cancelled) return;

      // 获取当前可见的城市列表（有市场数据的城市）
      const visibleSlugs = sqliteCitiesRef.current
        .filter((sc) => {
          const md = citiesRef.current.find((c) => c.city === sc.slug);
          return md?.highest?.thresholds.length;
        })
        .map((sc) => sc.slug);

      if (visibleSlugs.length === 0) {
        analyzeTimerRef.current = setTimeout(analyzeNext, 1000);
        return;
      }

      // 新一轮遍历开始
      if (analyzeIdxRef.current === 0) {
        nextRound();
        await rlog(null, "round_start", { cities: visibleSlugs, total: visibleSlugs.length, utc_time: new Date().toISOString() });
      }

      // 遍历完所有城市
      if (analyzeIdxRef.current >= visibleSlugs.length) {
        setAnalyzingSlug(null);

        // 单次模式：遍历完一轮后自动停止
        if (singleRunRef.current) {
          // [analyze] single-run completion logged via rlog
          setAnalyzingResult("单次遍历完成");
          singleRunRef.current = false;
          setExitAnim(true);
          await rlog(null, "round_end", { mode: "single", cities_count: visibleSlugs.length });
          return;
        }

        // 定时模式：安排下一次触发（按 scanInterval 间隔）
        const intervalHours = parseFloat(toolbarRef.current.scanInterval) || 1.0;
        const now = new Date();
        const next = new Date(now);
        if (intervalHours >= 1.0) {
          // 1.0h 模式：对齐到下一个整点
          next.setMinutes(0, 0, 0);
          next.setHours(next.getHours() + 1);
        } else {
          // 0.5h 模式：对齐到下一个半点或整点
          next.setSeconds(0, 0);
          if (next.getMinutes() < 30) {
            next.setMinutes(30);
          } else {
            next.setMinutes(0, 0, 0);
            next.setHours(next.getHours() + 1);
          }
        }
        const msUntilNext = next.getTime() - now.getTime();
        const fmtTime = `${String(next.getHours()).padStart(2, "0")}:${String(next.getMinutes()).padStart(2, "0")}`;
        setAnalyzingResult(`本轮完成，等待触发: ${fmtTime}`);
        await rlog(null, "round_end", { mode: "scheduled", cities_count: visibleSlugs.length, next_trigger: next.toISOString(), interval: intervalHours });
        hourlyTimerRef.current = setTimeout(() => {
          if (cancelled) return;
          hourlyTimerRef.current = null;
          analyzeIdxRef.current = 0;
          analyzeNext();
        }, msUntilNext);
        return;
      }

      const slug = visibleSlugs[analyzeIdxRef.current];
      setAnalyzingSlug(slug);
      setAnalyzingResult(null);

      // 天气数据由后端定时调度器每 60 秒自动刷新，无需遍历时单独获取

      // 步骤1：滚动到屏幕中间 + 高亮
      const el = cityCardRefs.current.get(slug);
      if (el) {
        el.scrollIntoView({ behavior: "smooth", block: "center" });
      }

      // ── 复盘日志：城市数据快照 ──
      {
        const md0 = citiesRef.current.find((c) => c.city === slug);
        const pm = throttledPriceMapRef.current;
        const weather = weatherMapRef.current.get(slug) ?? null;
        const positions = positionsRef.current.filter((p) => {
          return flattenAndFilter(citiesRef.current)
            .filter((r) => r.city === slug)
            .some((r) => r.threshold.no_token_id === p.token_id || r.threshold.yes_token_id === p.token_id);
        });
        const localOffset = md0?.city_tz ? tzOffsetHours(md0.city_tz) : null;
        const localTimeStr = localOffset !== null
          ? (() => { const { hour } = localHour(gmtNowRef.current, localOffset); return `${hour}:00`; })()
          : "unknown";

        // 盘口深度采集：回测长期只有成交价、没有档位可吃金额，导致"机会数"始终是
        // 高估的上限。这里把决策时点的真实盘口一并落盘，攒够样本后重跑带深度约束的回测。
        // 失败不影响主流程，深度字段留空即可。
        const depthByToken = new Map<string, { asks: { price: number; size: number }[]; bids: { price: number; size: number }[] }>();
        try {
          const tokenIds = (md0?.highest?.thresholds ?? []).map((t) => t.no_token_id).filter(Boolean);
          if (tokenIds.length > 0) {
            const snaps = await invoke<{ token_id: string; asks: { price: number; size: number }[]; bids: { price: number; size: number }[] }[]>(
              "fetch_depth_snapshot", { tokenIds, depth: 10 },
            );
            for (const d of snaps) depthByToken.set(d.token_id, { asks: d.asks, bids: d.bids });
          }
        } catch (e) {
          console.warn("Failed to fetch depth snapshot:", e);
        }

        const thresholds = md0?.highest?.thresholds.map((t) => {
          const price = pm.get(t.no_token_id);
          const depth = depthByToken.get(t.no_token_id);
          return {
            label: t.label,
            yes_price: t.yes_price,
            no_price: t.no_price,
            bid: price?.bid ?? null,
            ask: price?.ask ?? null,
            mid: price?.mid ?? null,
            // NO 侧卖盘：买入时能吃到的价量；bids 用于评估平仓时的退出深度
            depth_asks: depth?.asks ?? null,
            depth_bids: depth?.bids ?? null,
          };
        }) ?? [];

        await rlog(slug, "city_snapshot", {
          idx: analyzeIdxRef.current,
          total: visibleSlugs.length,
          local_time: localTimeStr,
          city_tz: md0?.city_tz ?? null,
          thresholds,
          positions: positions.map((p) => ({
            token_id: p.token_id,
            side: p.side,
            size: p.size,
            avg_price: p.avg_price,
            cur_price: p.cur_price,
            unrealized_pnl: p.unrealized_pnl,
          })),
          awc: weather?.awc ? {
            current: weather.awc.current,
            max: weather.awc.max,
            hourly_temps: weather.awc.hourlyTemps,
          } : null,
          st: weather?.st ? {
            current: weather.st.current,
            high: weather.st.high,
            condition: weather.st.condition,
            yesterdayHigh: weather.st.yesterdayHigh,
            yesterdayHighHour: weather.st.yesterdayHighHour,
            yesterdayCondition: weather.st.yesterdayCondition,
          } : null,
          met_forecast: weather?.met_forecast ?? [],
          account: {
            pusd_balance: accountRef.current?.pusd_balance ?? null,
            portfolio_value: accountRef.current?.portfolio_value ?? null,
          },
          toolbar: {
            askMin: toolbarRef.current.askMin,
            askMax: toolbarRef.current.askMax,
            timeHour: toolbarRef.current.timeHour,
            orderMode: toolbarRef.current.orderMode,
            orderAmount: toolbarRef.current.orderAmount,
            orderPercent: toolbarRef.current.orderPercent,
            tpAmount: tpAmountRef.current,
            slAmount: slAmountRef.current,
          },
        });
      }

      // 步骤2：处理当前城市的平仓
      await processCityClose(slug);

      // 步骤3：TIME条件判断——当地时间 < TIME阈值则跳过开仓
      const md = citiesRef.current.find((c) => c.city === slug);
      if (md?.city_tz) {
        const offset = tzOffsetHours(md.city_tz);
        if (offset !== null) {
          const { hour } = localHour(gmtNowRef.current, offset);
          const localHourNum = parseInt(hour, 10);
          const timeHourVal = parseInt(toolbarRef.current.timeHour, 10);
          if (localHourNum < timeHourVal) {
            setAnalyzingResult(`跳过: 当地时间 ${hour}:00 < TIME阈值 ${timeHourVal}:00`);
            await rlog(slug, "skip", { reason: "time_threshold", local_hour: localHourNum, time_threshold: timeHourVal });
            analyzeIdxRef.current++;
            analyzeTimerRef.current = setTimeout(analyzeNext, 1000);
            return;
          }
        }
      }

      // 步骤4：BID/ASK 区间检查——至少1个有效档位卖一价在区间内才进入 LLM
      const priceCheck = isCityPriceReady(slug);

      if (!priceCheck.ready) {
        const reason = priceCheck.reason ?? "价格未就绪";
        setAnalyzingResult(`跳过: ${reason}`);
        await rlog(slug, "skip", { reason: "price_not_ready", detail: reason });
        analyzeIdxRef.current++;
        analyzeTimerRef.current = setTimeout(analyzeNext, 1000);
        return;
      }

      // 步骤5：调用 LLM 分析开仓
      const delay = await processCityOpen(slug);

      // 步骤6：切换下一个城市，按返回的停留时间等待
      analyzeIdxRef.current++;
      analyzeTimerRef.current = setTimeout(analyzeNext, delay || 100);
    };

    // 处理单个城市的平仓
    const processCityClose = async (slug: string) => {
      const tpVal = parseFloat(tpAmountRef.current) || 0;
      const slVal = parseFloat(slAmountRef.current) || 0;
      const positions = positionsRef.current.filter((p) => {
        // 只处理属于该城市的持仓
        return flattenAndFilter(citiesRef.current)
          .filter((r) => r.city === slug)
          .some((r) => r.threshold.no_token_id === p.token_id || r.threshold.yes_token_id === p.token_id);
      });

      const priceMap = throttledPriceMapRef.current;

      for (const pos of positions) {
        const key = `${pos.market_id}:${pos.token_id}`;
        if (autoCloseAttemptedRef.current.has(key)) continue;

        const price = priceMap.get(pos.token_id);
        const bid = price?.bid ?? 0;
        const ask = price?.ask ?? 0;

        let trigger = false;
        let reason = "";

        // 止损只看 bid（实际卖出价）：崩盘时盘口拉宽，ask 长期高于止损价会导致止损失效
        if (tpVal > 0 && bid > 0 && bid <= tpVal) {
          trigger = true;
          reason = `SL@${bid.toFixed(3)}/${ask.toFixed(3)}`;
        }

        if (slVal > 0 && bid > 0 && bid >= slVal) {
          trigger = true;
          reason = `TP@${bid.toFixed(3)}`;
        }

        if (!trigger) continue;

        autoCloseAttemptedRef.current.add(key);

        try {
          await invoke<string>("close_position", { tokenId: pos.token_id });
          showToast("success", `自动平仓 ${reason}: ${pos.question}`);
          await rlog(slug, "close_attempt", {
            token_id: pos.token_id,
            side: pos.side,
            size: pos.size,
            avg_price: pos.avg_price,
            bid, ask,
            trigger: reason,
            result: "success",
          });
          removePosition(pos.token_id);
          setPnlRefreshTick((t) => t + 1);
          for (const row of flattenAndFilter(citiesRef.current)) {
            if (row.threshold.no_token_id === pos.token_id) {
              const rk = rowKey(row);
              setPinnedKeys((prev) => {
                const next = new Set(prev);
                next.delete(rk);
                return next;
              });
              recentSoldKeysRef.current.add(rk);
              setTimeout(() => { recentSoldKeysRef.current.delete(rk); }, 15000);
              break;
            }
          }
          refreshAccount();
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          // 链上与 DB 均无持仓：说明后端监控器已先行平仓，前端持仓缓存过期，直接移除避免每轮重复报错
          const stale = /No position found/i.test(msg);
          if (stale) {
            removePosition(pos.token_id);
          } else {
            // 其它错误（网络/盘口）允许下一轮重试
            autoCloseAttemptedRef.current.delete(key);
            showToast("error", `自动平仓失败: ${pos.question} - ${msg}`);
          }
          await rlog(slug, "close_attempt", {
            token_id: pos.token_id,
            side: pos.side,
            size: pos.size,
            avg_price: pos.avg_price,
            bid, ask,
            trigger: reason,
            result: stale ? "stale_position" : "error",
            error: msg,
          });
        }
      }
    };

    // 处理单个城市的开仓：组装数据→调用 LLM 分析→按建议执行开仓
    // 返回 true 表示有分析结果（需停留查看），false 表示无结果
    const processCityOpen = async (slug: string): Promise<number> => {
      try {
        const { orderMode, orderAmount, orderPercent } = toolbarRef.current;
        const acct = accountRef.current;
        const availableCash = acct?.pusd_balance ?? 0;
        const totalValue = acct?.portfolio_value ?? 0;
        const MIN_ORDER_COST = 5 * 0.995; // 最小开仓成本 = 5 shares × 最高价 0.995

        let amount: number;
        if (orderMode === "fixed") {
          amount = parseFloat(orderAmount) || 0;
        } else {
          // 百分比模式：按总资产计算，不足最小开仓成本时降级为最小开仓金额
          amount = totalValue * (parseFloat(orderPercent) || 0) / 100;
          if (amount > 0 && amount < MIN_ORDER_COST && availableCash >= MIN_ORDER_COST) {
            amount = MIN_ORDER_COST;
          }
        }

        // 余额预检：可用资金不足开单时跳过 LLM 分析，进入下一个城市
        if (amount <= 0 || amount < MIN_ORDER_COST || availableCash < MIN_ORDER_COST) {
          setAnalyzingResult(`余额不足: 可用 $${availableCash.toFixed(2)}, 需 ≥ $${MIN_ORDER_COST.toFixed(2)}`);
          await rlog(slug, "open_preflight", { result: "insufficient_balance", available_cash: availableCash, min_cost: MIN_ORDER_COST, planned_amount: amount });
          return 1000;
        }

        // 组装城市数据传给 LLM 分析
        const md = citiesRef.current.find((c) => c.city === slug);
        if (!md?.highest?.thresholds.length) return 0;

        const priceMap = throttledPriceMapRef.current;
        const sqliteCity = sqliteCitiesRef.current.find((sc) => sc.slug === slug);
        const weather = weatherMapRef.current.get(slug) ?? null;

        // 计算当地时间
        const offset = tzOffsetHours(md.city_tz);
        const now = gmtNowRef.current;
        const localTimeStr = offset !== null
          ? (() => {
              const { hour, dayOffset } = localHour(now, offset);
              const dayStr = dayOffset === -1 ? "(前一天)" : dayOffset === 1 ? "(第二天)" : "";
              return `${dayStr} ${hour}:00`;
            })()
          : "unknown";

        // 收集当前城市已持仓的档位标签
        const heldThresholds = md.highest.thresholds
          .filter((t) => pinnedKeysRef.current.has(`${slug}-highest-${t.label}`))
          .map((t) => t.label);

        const analyzeData = {
          city: slug,
          city_tz: md.city_tz,
          local_time: localTimeStr,
          unit: sqliteCity?.unit ?? "\u00B0C",
          bid_ask_min: parseFloat(toolbarRef.current.askMin),
          bid_ask_max: parseFloat(toolbarRef.current.askMax),
          thresholds: md.highest.thresholds.map((t) => {
            const price = priceMap.get(t.no_token_id);
            return {
              label: t.label,
              yes_price: t.yes_price,
              no_price: t.no_price,
              bid: price?.bid ?? null,
              ask: price?.ask ?? null,
              mid: price?.mid ?? null,
            };
          }),
          awc: weather?.awc ? {
            current: weather.awc.current,
            max: weather.awc.max,
            hourly_temps: weather.awc.hourlyTemps,
          } : null,
          st: weather?.st ? {
            current: weather.st.current,
            high: weather.st.high,
            condition: weather.st.condition,
            yesterdayHigh: weather.st.yesterdayHigh,
            yesterdayHighHour: weather.st.yesterdayHighHour,
            yesterdayCondition: weather.st.yesterdayCondition,
          } : null,
          met_forecast: weather?.met_forecast ?? [],
          held_thresholds: heldThresholds,
        };

        await rlog(slug, "open_preflight", {
          result: "proceed_to_llm",
          available_cash: availableCash,
          planned_amount: amount,
          order_mode: orderMode,
          local_time: localTimeStr,
          held_thresholds: heldThresholds,
        });

        // 调用后端 analyze_city
        let analysis;
        try {
          analysis = await invoke<{
            actions: { threshold_label: string; side: string; reason: string }[];
            summary: string;
          }>("analyze_city", { data: analyzeData });
        } catch (e) {
          let msg = e instanceof Error ? e.message : String(e);
          // 截断原始 JSON，防止在 UI 上显示冗长内容
          const rawIdx = msg.indexOf(" | raw:");
          if (rawIdx !== -1) msg = msg.slice(0, rawIdx);
          setAnalyzingResult(`分析失败: ${msg}`);
          // Toast 只显示城市名，不展示 LLM 原始错误详情
          showToast("error", `分析失败: ${cityDisplayName(slug)}`);
          await rlog(slug, "error", { stage: "llm_analyze", error: msg, raw_error: e instanceof Error ? e.message : String(e) });
          return 3000;
        }

        await rlog(slug, "open_llm_result", {
          actions: analysis.actions,
          summary: analysis.summary,
        });

        // v2.2.0：LLM 降为建议。候选池不再由 LLM 决定，而是由安全闸门遍历全部 NO 档位自行生成；
        // LLM 的推荐与理由仍然记入 review 日志，用于事后对账，但不再拥有否决权。
        const llmActions = analysis.actions ?? [];
        const llmPicked = new Set(llmActions.map((a) => a.threshold_label));
        const llmReasonOf = new Map(llmActions.map((a) => [a.threshold_label, a.reason]));
        type OpenCandidate = { threshold_label: string; side: string; reason: string; origin: "llm" | "gate" };
        const candidates: OpenCandidate[] = llmActions.map((a) => ({ ...a, origin: "llm" as const }));
        // 闸门自主候选按卖一价升序排列：资金受限时优先吃毛利最厚的单子（0.92–0.95 带单笔 +4.15%）
        const gateCandidates = md.highest.thresholds
          .filter((t) => !llmPicked.has(t.label))
          .map((t) => ({ t, ask: priceMap.get(t.no_token_id)?.ask ?? t.no_price }))
          .filter((x) => x.ask != null)
          .sort((a, b) => (a.ask as number) - (b.ask as number))
          .map((x) => ({ threshold_label: x.t.label, side: "NO", reason: "gate_enumerated", origin: "gate" as const }));
        candidates.push(...gateCandidates);

        await rlog(slug, "open_candidates", {
          llm_count: llmActions.length,
          gate_count: gateCandidates.length,
          llm_labels: llmActions.map((a) => a.threshold_label),
        });

        if (candidates.length === 0) {
          setAnalyzingResult(`不开仓: ${analysis.summary}`);
          return 3000;
        }

        // 安全闸所需的城市级参考量（每次分析计算一次）
        const gateLocalHour = offset !== null ? parseInt(localHour(now, offset).hour, 10) : -1;
        // MET 预报按 10:00-17:00 逐小时排列；只取当前小时及之后的预报峰值（已过去的时段以观测为准）
        const metRemaining = analyzeData.met_forecast.filter((_, i) => 10 + i >= gateLocalHour);
        const metForPeak = metRemaining.length ? metRemaining : analyzeData.met_forecast;
        const gateMetPeak = metForPeak.length
          ? Math.max(...metForPeak.map((f) => f.temp))
          : null;
        let gateMarketPeakLower: number | null = null;
        {
          let minMid = Infinity;
          for (const t of analyzeData.thresholds) {
            if (t.bid == null || t.ask == null || t.mid == null) continue;
            const lo = parseThresholdLower(t.label);
            if (lo === null) continue;
            if (t.mid < minMid) { minMid = t.mid; gateMarketPeakLower = lo; }
          }
        }

        // 逐个处理候选档位（LLM 建议 + 闸门自主枚举）
        let openedCount = 0;
        let firstLabel = "";
        for (const action of candidates) {
          const targetThreshold = md.highest.thresholds.find(
            (t) => t.label === action.threshold_label
          );
          if (!targetThreshold) {
            // [analyze] threshold not found - logged via rlog
            await rlog(slug, "open_execute", { result: "threshold_not_found", label: action.threshold_label, side: action.side });
            continue;
          }

          const key = `${slug}-highest-${targetThreshold.label}`;
          if (pinnedKeysRef.current.has(key)) {
            // [analyze] already held - logged via rlog
            await rlog(slug, "open_execute", { result: "already_held", label: action.threshold_label, side: action.side });
            continue;
          }
          if (autoTradeAttemptedRef.current.has(key)) {
            // [analyze] already attempted - logged via rlog
            await rlog(slug, "open_execute", { result: "already_attempted", label: action.threshold_label, side: action.side });
            continue;
          }

          // 检查剩余余额是否够开仓
          const currentCash = accountRef.current?.pusd_balance ?? 0;
          const currentTotalValue = accountRef.current?.portfolio_value ?? 0;
          let currentAmount: number;
          if (orderMode === "fixed") {
            currentAmount = parseFloat(orderAmount) || 0;
          } else {
            // 百分比模式：始终按总资产计算，与初始计算保持一致
            currentAmount = currentTotalValue * (parseFloat(orderPercent) || 0) / 100;
            if (currentAmount > 0 && currentAmount < MIN_ORDER_COST && currentCash >= MIN_ORDER_COST) {
              currentAmount = MIN_ORDER_COST;
            }
          }
          if (currentAmount <= 0 || currentAmount < MIN_ORDER_COST) {
            // [analyze] insufficient balance - logged via rlog
            await rlog(slug, "open_execute", { result: "insufficient_balance_mid_loop", label: action.threshold_label, side: action.side, current_cash: currentCash });
            break;
          }

          autoTradeAttemptedRef.current.add(key);

          // 计算卖一价
          const price = priceMap.get(targetThreshold.no_token_id);
          const askPrice = action.side === "NO"
            ? (price?.ask ?? targetThreshold.no_price)
            : (price?.bid != null ? 1 - price.bid : 1 - targetThreshold.no_price);

          // BID/ASK 区间硬校验
          const bidAskMin = parseFloat(toolbarRef.current.askMin);
          const bidAskMax = parseFloat(toolbarRef.current.askMax);
          if (!isNaN(bidAskMin) && !isNaN(bidAskMax) && (askPrice < bidAskMin || askPrice > bidAskMax)) {
            // [analyze] ask price out of range - logged via rlog
            autoTradeAttemptedRef.current.delete(key);
            await rlog(slug, "open_execute", { result: "out_of_range", label: action.threshold_label, side: action.side, ask_price: askPrice, range: [bidAskMin, bidAskMax] });
            continue;
          }

          // 开仓安全闸（代码硬校验）：LLM 只负责推荐，最终是否允许开仓由确定性规则决定
          if (action.side === "NO") {
            const gate = checkOpenSafetyGate({
              label: targetThreshold.label,
              unit: analyzeData.unit,
              localHour: gateLocalHour,
              stHigh: weather?.st?.high ?? null,
              stCurrent: weather?.st?.current ?? null,
              awcMax: weather?.awc?.max ?? null,
              metPeak: gateMetPeak,
              marketPeakLower: gateMarketPeakLower,
              yesterdayHighHour: weather?.st?.yesterdayHighHour ?? null,
            });
            if (!gate.pass) {
              autoTradeAttemptedRef.current.delete(key);
              await rlog(slug, "open_execute", {
                result: "safety_gate_fail",
                label: action.threshold_label,
                side: action.side,
                ask_price: askPrice,
                reason: gate.reason,
                gate: gate.detail,
                origin: action.origin,
                llm_reason: llmReasonOf.get(action.threshold_label) ?? null,
              });
              continue;
            }
            await rlog(slug, "safety_gate_pass", {
              label: action.threshold_label,
              reason: gate.reason,
              gate: gate.detail,
              origin: action.origin,
              llm_reason: llmReasonOf.get(action.threshold_label) ?? null,
            });
          }

          if (!firstLabel) firstLabel = action.threshold_label;

          const opp = {
            market_id: targetThreshold.market_id,
            question: targetThreshold.question,
            city: slug,
            city_tz: md.city_tz,
            side: action.side,
            token_id: action.side === "NO" ? targetThreshold.no_token_id : targetThreshold.yes_token_id,
            threshold: targetThreshold.label,
            current_price: askPrice,
            expected_profit: 0,
            confirmed_threshold: null,
            // 区分成交来源，便于事后统计 LLM 建议单 vs 闸门自主单的表现差异
            strategy_label: action.origin === "llm" ? "LLM" : "Gate",
          };

          try {
            // maxPrice：后端用实时 best_ask 与此上限比对，盘口上移则拒单不追价
            const record = await invoke<TradeRecord>("open_position", {
              opp,
              amount: currentAmount,
              maxPrice: isNaN(bidAskMax) ? null : bidAskMax,
            });
            fillPositionFromRecord(record);
            setPinnedKeys((prev) => new Set(prev).add(key));
            showToast("success", `LLM 判断: ${cityDisplayName(slug)} ${targetThreshold.label} @ ${askPrice.toFixed(3)}`);
            refreshAccount();
            openedCount++;
            await rlog(slug, "open_execute", {
              result: "success",
              label: action.threshold_label,
              side: action.side,
              ask_price: askPrice,
              amount: currentAmount,
              token_id: opp.token_id,
              trade_id: record.id,
              entry_price: record.entry_price,
              size: record.size,
              cost: record.cost,
            });
          } catch (e) {
            autoTradeAttemptedRef.current.delete(key);
            const msg = e instanceof Error ? e.message : String(e);
            showToast("error", `LLM 开仓失败: ${cityDisplayName(slug)} ${targetThreshold.label} - ${msg}`);
            await rlog(slug, "open_execute", {
              result: "error",
              label: action.threshold_label,
              side: action.side,
              ask_price: askPrice,
              amount: currentAmount,
              token_id: opp.token_id,
              error: msg,
            });
          }
        }

        if (openedCount > 0) {
          setAnalyzingResult(`开仓 ${openedCount} 笔: ${analysis.actions.map(a => a.threshold_label).join(", ")} - ${analysis.summary}`);
        } else {
          setAnalyzingResult(`未开仓: ${analysis.summary}`);
        }
        return 3000;
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        setAnalyzingResult(`开仓异常: ${msg}`);
        await rlog(slug, "error", { stage: "processCityOpen", error: msg });
        return 3000;
      }
    };

    // 单条手动分析：复用遍历引擎的单城市处理逻辑，不依赖 cancelled
    const analyzeOne = async (slug: string) => {
      setAnalyzingSlug(slug);
      setAnalyzingResult(null);

      // 滚动到屏幕中间
      const el = cityCardRefs.current.get(slug);
      if (el) {
        el.scrollIntoView({ behavior: "smooth", block: "center" });
      }

      // 复盘日志：城市数据快照
      {
        const md0 = citiesRef.current.find((c) => c.city === slug);
        const pm = throttledPriceMapRef.current;
        const weather = weatherMapRef.current.get(slug) ?? null;
        const positions = positionsRef.current.filter((p) => {
          return flattenAndFilter(citiesRef.current)
            .filter((r) => r.city === slug)
            .some((r) => r.threshold.no_token_id === p.token_id || r.threshold.yes_token_id === p.token_id);
        });
        const localOffset = md0?.city_tz ? tzOffsetHours(md0.city_tz) : null;
        const localTimeStr = localOffset !== null
          ? (() => { const { hour } = localHour(gmtNowRef.current, localOffset); return `${hour}:00`; })()
          : "unknown";

        // 盘口深度采集：回测长期只有成交价、没有档位可吃金额，导致"机会数"始终是
        // 高估的上限。这里把决策时点的真实盘口一并落盘，攒够样本后重跑带深度约束的回测。
        // 失败不影响主流程，深度字段留空即可。
        const depthByToken = new Map<string, { asks: { price: number; size: number }[]; bids: { price: number; size: number }[] }>();
        try {
          const tokenIds = (md0?.highest?.thresholds ?? []).map((t) => t.no_token_id).filter(Boolean);
          if (tokenIds.length > 0) {
            const snaps = await invoke<{ token_id: string; asks: { price: number; size: number }[]; bids: { price: number; size: number }[] }[]>(
              "fetch_depth_snapshot", { tokenIds, depth: 10 },
            );
            for (const d of snaps) depthByToken.set(d.token_id, { asks: d.asks, bids: d.bids });
          }
        } catch (e) {
          console.warn("Failed to fetch depth snapshot:", e);
        }

        const thresholds = md0?.highest?.thresholds.map((t) => {
          const price = pm.get(t.no_token_id);
          const depth = depthByToken.get(t.no_token_id);
          return {
            label: t.label,
            yes_price: t.yes_price,
            no_price: t.no_price,
            bid: price?.bid ?? null,
            ask: price?.ask ?? null,
            mid: price?.mid ?? null,
            // NO 侧卖盘：买入时能吃到的价量；bids 用于评估平仓时的退出深度
            depth_asks: depth?.asks ?? null,
            depth_bids: depth?.bids ?? null,
          };
        }) ?? [];

        await rlog(slug, "city_snapshot", {
          idx: -1,
          total: 1,
          local_time: localTimeStr,
          city_tz: md0?.city_tz ?? null,
          thresholds,
          positions: positions.map((p) => ({
            token_id: p.token_id,
            side: p.side,
            size: p.size,
            avg_price: p.avg_price,
            cur_price: p.cur_price,
            unrealized_pnl: p.unrealized_pnl,
          })),
          awc: weather?.awc ? {
            current: weather.awc.current,
            max: weather.awc.max,
            hourly_temps: weather.awc.hourlyTemps,
          } : null,
          st: weather?.st ? {
            current: weather.st.current,
            high: weather.st.high,
            condition: weather.st.condition,
            yesterdayHigh: weather.st.yesterdayHigh,
            yesterdayHighHour: weather.st.yesterdayHighHour,
            yesterdayCondition: weather.st.yesterdayCondition,
          } : null,
          met_forecast: weather?.met_forecast ?? [],
          account: {
            pusd_balance: accountRef.current?.pusd_balance ?? null,
            portfolio_value: accountRef.current?.portfolio_value ?? null,
          },
          toolbar: {
            askMin: toolbarRef.current.askMin,
            askMax: toolbarRef.current.askMax,
            timeHour: toolbarRef.current.timeHour,
            orderMode: toolbarRef.current.orderMode,
            orderAmount: toolbarRef.current.orderAmount,
            orderPercent: toolbarRef.current.orderPercent,
            tpAmount: tpAmountRef.current,
            slAmount: slAmountRef.current,
          },
        });
      }

      // 步骤1：处理平仓
      await processCityClose(slug);

      // 步骤2：TIME条件判断——当地时间 < TIME阈值则跳过开仓
      const md = citiesRef.current.find((c) => c.city === slug);
      if (md?.city_tz) {
        const offset = tzOffsetHours(md.city_tz);
        if (offset !== null) {
          const { hour } = localHour(gmtNowRef.current, offset);
          const localHourNum = parseInt(hour, 10);
          const timeHourVal = parseInt(toolbarRef.current.timeHour, 10);
          if (localHourNum < timeHourVal) {
            setAnalyzingResult(`跳过: 当地时间 ${hour}:00 < TIME阈值 ${timeHourVal}:00`);
            await rlog(slug, "skip", { reason: "time_threshold", local_hour: localHourNum, time_threshold: timeHourVal });
            setTimeout(() => { setAnalyzingSlug((prev) => prev === slug ? null : prev); }, 1000);
            return;
          }
        }
      }

      // 步骤3：BID/ASK 区间检查——至少1个有效档位卖一价在区间内才进入 LLM
      const priceCheck = isCityPriceReady(slug);
      if (!priceCheck.ready) {
        const reason = priceCheck.reason ?? "价格未就绪";
        setAnalyzingResult(`跳过: ${reason}`);
        await rlog(slug, "skip", { reason: "price_not_ready", detail: reason });
        setTimeout(() => { setAnalyzingSlug((prev) => prev === slug ? null : prev); }, 1000);
        return;
      }

      // 步骤4：开仓判断
      await processCityOpen(slug);

      // 3秒后清除高亮
      setTimeout(() => { setAnalyzingSlug((prev) => prev === slug ? null : prev); }, 3000);
    };
    analyzeOneRef.current = analyzeOne;

    if (!isRunning) {
      if (analyzeTimerRef.current) {
        clearTimeout(analyzeTimerRef.current);
        analyzeTimerRef.current = null;
      }
      setAnalyzingSlug(null);
      setAnalyzingResult(null);
      analyzeIdxRef.current = 0;
      return;
    }

    // 启动分析循环
    const scheduleRun = () => {
      // 单次模式：立即开始遍历，不等整点
      if (singleRunRef.current) {
        // [analyze] single-run mode started
        setAnalyzingResult("单次遍历中...");
        analyzeIdxRef.current = 0;
        analyzeNext();
        return;
      }

      // 定时模式：等待到下一个触发点再开始遍历
      const intervalHours = parseFloat(toolbarRef.current.scanInterval) || 1.0;
      const now = new Date();
      const next = new Date(now);
      if (intervalHours >= 1.0) {
        // 1.0h 模式：对齐到下一个整点
        next.setMinutes(0, 0, 0);
        next.setHours(next.getHours() + 1);
      } else {
        // 0.5h 模式：对齐到下一个半点或整点
        next.setSeconds(0, 0);
        if (next.getMinutes() < 30) {
          next.setMinutes(30);
        } else {
          next.setMinutes(0, 0, 0);
          next.setHours(next.getHours() + 1);
        }
      }
      const msUntilNext = next.getTime() - now.getTime();
      const fmtTime = `${String(next.getHours()).padStart(2, "0")}:${String(next.getMinutes()).padStart(2, "0")}`;
      setAnalyzingResult(`等待触发: ${fmtTime}`);

      hourlyTimerRef.current = setTimeout(() => {
        hourlyTimerRef.current = null;
        analyzeIdxRef.current = 0;
        analyzeNext();
      }, msUntilNext);
    };

    scheduleRun();

    return () => {
      cancelled = true;
      if (analyzeTimerRef.current) {
        clearTimeout(analyzeTimerRef.current);
        analyzeTimerRef.current = null;
      }
      if (hourlyTimerRef.current) {
        clearTimeout(hourlyTimerRef.current);
        hourlyTimerRef.current = null;
      }
      setAnalyzingSlug(null);
    };
  }, [isRunning]);


  // 滚动节流
  useEffect(() => {
    const container = scrollContainerRef.current;
    if (!container) return;
    const onScroll = () => {
      isScrollingRef.current = true;
      if (scrollTimerRef.current) clearTimeout(scrollTimerRef.current);
      scrollTimerRef.current = setTimeout(() => { isScrollingRef.current = false; }, 200);
    };
    container.addEventListener("scroll", onScroll, { passive: true });
    return () => container.removeEventListener("scroll", onScroll);
  }, []);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100vh", overflow: "hidden", background: "#0d0d1a", fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif", userSelect: "none" }}>
      {/* 标题栏 */}
      <div style={{ flexShrink: 0, padding: "10px 16px", background: "#0f172a", borderBottom: "1px solid #334155", display: "flex", justifyContent: "space-between", alignItems: "center", position: "relative" }}>
        <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
          <h1 style={{
            fontSize: "26px",
            fontWeight: 800,
            letterSpacing: "1px",
            background: "linear-gradient(135deg, #22d3ee 0%, #818cf8 50%, #c084fc 100%)",
            WebkitBackgroundClip: "text",
            backgroundClip: "text",
            WebkitTextFillColor: "transparent",
          }}>
            WoolBrush
          </h1>
          <span title="版本号 · 构建时间（北京时间 月日-时分）" style={{ fontSize: "11px", color: "#64748b", fontFamily: "monospace", marginLeft: "6px", alignSelf: "flex-end", paddingBottom: "5px" }}>
            v{__APP_VERSION__} · {__BUILD_TIME__}
          </span>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
          <PnlChart refreshTick={pnlRefreshTick} portfolioValue={account?.portfolio_value ?? null} onReset={() => setPnlRefreshTick((t) => t + 1)} orderMode={orderMode} />
          <div style={{ width: "8px" }} />
          {account && <AccountInline account={account} />}
          <button onClick={() => setShowSettings(true)} style={{ padding: "4px 12px", fontSize: "12px", background: "#1e293b", color: "#94a3b8", border: "1px solid #334155", borderRadius: "4px", cursor: "pointer", transition: "all 0.15s" }} onMouseEnter={(e) => { e.currentTarget.style.color = "#38bdf8"; e.currentTarget.style.borderColor = "#38bdf8"; }} onMouseLeave={(e) => { e.currentTarget.style.color = "#94a3b8"; e.currentTarget.style.borderColor = "#334155"; }}>
            设置
          </button>
        </div>

        {/* 进度线：市场 + 天气合并 */}
        {progressBarVisible && (() => {
          const marketPct = progress.processed / Math.max(progress.total, 1);
          const weatherPct = weatherProgress.total > 0 ? weatherProgress.processed / weatherProgress.total : 0;
          // 天气延迟3秒启动，按完成比例合并：各占50%
          const overall = (marketPct + weatherPct) / 2;
          return (
            <div style={{ position: "absolute", bottom: 0, left: 0, right: 0, height: "1px", background: state === "loading" ? `linear-gradient(90deg, #7c3aed ${overall * 100}%, transparent ${overall * 100}%)` : "#334155", opacity: progressBarOpacity, transition: "background 0.3s ease, opacity 0.6s ease" }} />
          );
        })()}
      </div>

      {/* 操作栏 */}
      <div style={{ flexShrink: 0, padding: "6px 12px", background: "#1e293b", borderBottom: "1px solid #334155", display: "flex", alignItems: "center", gap: "10px", fontSize: "12px" }}>
        {/* 订单额 */}
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <span style={{ color: "#94a3b8", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.5px", fontSize: "11px" }}>OA</span>
          <div style={{ display: "flex", borderRadius: "4px", overflow: "hidden", border: "1px solid #334155" }}>
            <button onClick={() => setOrderMode("fixed")} style={{ padding: "4px 10px", fontSize: "11px", fontWeight: 600, border: "none", borderRadius: 0, cursor: "pointer", background: orderMode === "fixed" ? "#94a3b8" : "#0f172a", color: orderMode === "fixed" ? "#0f172a" : "#94a3b8" }}>$</button>
            <button onClick={() => setOrderMode("percent")} style={{ padding: "4px 10px", fontSize: "11px", fontWeight: 600, border: "none", borderLeft: "1px solid #334155", borderRadius: 0, cursor: "pointer", background: orderMode === "percent" ? "#94a3b8" : "#0f172a", color: orderMode === "percent" ? "#0f172a" : "#94a3b8" }}>%</button>
          </div>
          <input type="text" value={orderMode === "fixed" ? orderAmount : orderPercent} onChange={(e) => { const clamped = clampPositiveInt(e.target.value); if (orderMode === "fixed") setOrderAmount(clamped); else setOrderPercent(clamped); }} style={{ width: "42px", padding: "4px 4px", fontSize: "12px", lineHeight: "16px", color: "#e2e8f0", background: "#0f172a", border: "1px solid #334155", borderRadius: "4px", outline: "none", textAlign: "center" }} />
        </div>
        <div style={{ width: "1px", height: "20px", background: "#334155", margin: "0 -4px" }} />
        {/* TIME */}
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <span style={{ color: "#94a3b8", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.5px", fontSize: "11px" }}>TIME</span>
          <span style={{ color: "#94a3b8", fontSize: "12px" }}>≥</span>
          <input type="text" value={timeHour} onChange={(e) => setTimeHour(clampHour(e.target.value))} style={{ width: "32px", padding: "4px 4px", fontSize: "12px", lineHeight: "16px", color: "#e2e8f0", background: "#0f172a", border: "1px solid #334155", borderRadius: "4px", outline: "none", textAlign: "center" }} />
        </div>
        <div style={{ width: "1px", height: "20px", background: "#334155", margin: "0 -4px" }} />
        {/* BID/ASK */}
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <span style={{ color: "#94a3b8", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.5px", fontSize: "11px" }}>BID/ASK</span>
          <input value={askMin} onChange={(e) => setAskMin(e.target.value)} style={{ width: "50px", padding: "4px 8px", fontSize: "12px", lineHeight: "16px", color: "#e2e8f0", background: "#0f172a", border: "1px solid #334155", borderRadius: "4px", outline: "none", textAlign: "center" }} />
          <span style={{ color: "#64748b", fontSize: "12px" }}>~</span>
          <input value={askMax} onChange={(e) => setAskMax(e.target.value)} style={{ width: "50px", padding: "4px 8px", fontSize: "12px", lineHeight: "16px", color: "#e2e8f0", background: "#0f172a", border: "1px solid #334155", borderRadius: "4px", outline: "none", textAlign: "center" }} />
        </div>
        <div style={{ width: "1px", height: "20px", background: "#334155", margin: "0 -4px" }} />
        {/* 止损 (TP): 卖一价 ≤ 阈值时平仓 */}
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <span style={{ color: "#94a3b8", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.5px", fontSize: "11px" }}>SL</span>
          <span style={{ color: "#94a3b8", fontSize: "12px" }}>≤</span>
          <input type="text" value={tpAmount} onChange={(e) => setTpAmount(clampDecimal(e.target.value))} style={{ width: "50px", padding: "4px 8px", fontSize: "12px", lineHeight: "16px", color: "#e2e8f0", background: "#0f172a", border: "1px solid #334155", borderRadius: "4px", outline: "none", textAlign: "center" }} />
        </div>
        <div style={{ width: "1px", height: "20px", background: "#334155", margin: "0 -4px" }} />
        {/* 止盈 (SL): 买一价 ≥ 阈值时平仓 */}
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <span style={{ color: "#94a3b8", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.5px", fontSize: "11px" }}>TP</span>
          <span style={{ color: "#94a3b8", fontSize: "12px" }}>≥</span>
          <input type="text" value={slAmount} onChange={(e) => setSlAmount(clampDecimal(e.target.value))} style={{ width: "50px", padding: "4px 8px", fontSize: "12px", lineHeight: "16px", color: "#e2e8f0", background: "#0f172a", border: "1px solid #334155", borderRadius: "4px", outline: "none", textAlign: "center" }} />
        </div>
        <div style={{ width: "1px", height: "20px", background: "#334155", margin: "0 -4px" }} />
        {/* 遍历间隔 */}
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <span style={{ color: "#94a3b8", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.5px", fontSize: "11px" }}>INTV</span>
          <select value={scanInterval} onChange={(e) => setScanInterval(e.target.value)} style={{ width: "55px", padding: "3px 4px", fontSize: "12px", lineHeight: "16px", color: "#e2e8f0", background: "#0f172a", border: "1px solid #334155", borderRadius: "4px", outline: "none", cursor: "pointer" }}>
            <option value="0.5">0.5h</option>
            <option value="1.0">1.0h</option>
          </select>
        </div>
        <div style={{ flex: 1 }} />
        {isRunning && (
          <span onAnimationEnd={() => { if (exitAnim) { setIsRunning(false); setExitAnim(false); } }} style={{ display: "inline-block", color: "#ffffff", textShadow: "0 0 3px #ff9500, 0 0 6px #ff9500, 0 0 12px #ff6a00, 0 0 24px rgba(255,106,0,0.5), 0 0 48px rgba(255,106,0,0.3)", ...(exitAnim ? { animation: "fadeUpOut 0.4s ease forwards" } : {}) }}>
            <FlipTime text={formatElapsed(runSeconds)} />
          </span>
        )}
        {/* 启动/停止按钮 */}
        <button onClick={handleStartClick} title={isRunning ? "点击停止" : "单击：整点遍历 | 双击：单次遍历"} style={{ display: "flex", alignItems: "center", gap: "6px", padding: "6px 18px", fontSize: "13px", fontWeight: 700, borderRadius: "6px", border: "none", cursor: "pointer", background: isRunning ? "#dc2626" : "#059669", color: "#fff", transition: "background 0.2s", overflow: "hidden", userSelect: "none" }}>
          <span key={isRunning ? "stop" : "start"} className="btn-content-in" style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            {isRunning ? (
              <>
                <span style={{ width: "8px", height: "8px", borderRadius: "50%", background: "#fff", animation: "pulse 1s ease-in-out infinite" }} />
                停止
              </>
            ) : (
              <>
                <span style={{ width: "0", height: "0", borderTop: "5px solid transparent", borderBottom: "5px solid transparent", borderLeft: "8px solid #fff" }} />
                开始
              </>
            )}
          </span>
        </button>
      </div>

      {/* 城市卡片列表 */}
      <div ref={scrollContainerRef} style={{ flex: 1, overflow: "auto", overscrollBehavior: "none", paddingTop: "2px" }}>
        {sqliteCities.length === 0 && state === "loading" && (
          <div style={{ padding: "40px", textAlign: "center", color: "#64748b", fontSize: "14px" }}>加载城市中...</div>
        )}
        {sqliteCities.length === 0 && state === "success" && (
          <div style={{ padding: "40px", textAlign: "center", color: "#64748b", fontSize: "14px" }}>数据库中暂无城市</div>
        )}
        {(() => {
          // 预建 city -> marketData 映射，避免 O(n²) 查找
          const mdMap = new Map(cities.map((c) => [c.city, c]));
          let visibleIdx = 0;
          return sqliteCities.map((sc, i) => {
            const md = mdMap.get(sc.slug) ?? null;
            const hasData = !!md?.highest?.thresholds.length;
            const idx = hasData ? ++visibleIdx : i + 1;
            return (
              <CityCard
                key={sc.slug}
                index={idx}
                sqliteCity={sc}
                marketData={md}
                priceMap={cityPriceMaps.get(sc.slug) ?? EMPTY_PRICE_MAP}
                weather={weatherMap.get(sc.slug) ?? null}
                positions={positionsByCity.get(sc.slug) ?? EMPTY_POSITIONS}
                onOpenPosition={handleManualOpen}
                onClosePosition={handleManualClose}
                onAnalyzeOne={handleAnalyzeOne}
                isAnalyzing={analyzingSlug === sc.slug}
                analyzingResult={analyzingSlug === sc.slug ? analyzingResult : null}
                cardRef={(el) => {
                  if (el) {
                    cityCardRefs.current.set(sc.slug, el);
                  } else {
                    cityCardRefs.current.delete(sc.slug);
                  }
                }}
                tpVal={parseFloat(tpAmount) || 0}
                slVal={parseFloat(slAmount) || 0}
                timeTick={timeTick}
              />
            );
          });
        })()}
      </div>

      {/* 状态栏 */}
      <div style={{ flexShrink: 0, padding: "6px 16px", background: "#0f172a", borderTop: "1px solid #334155", fontSize: "12px", color: "#64748b", display: "flex", alignItems: "center", gap: "8px" }}>
        <button onClick={() => loadData(selectedCitySlugs ? Array.from(selectedCitySlugs) : null)} disabled={state === "loading"} title="刷新：重新加载选中城市的市场数据" style={{ padding: "4px 6px", background: "transparent", border: "1px solid #334155", borderRadius: "4px", cursor: state === "loading" ? "not-allowed" : "pointer", color: "#94a3b8", display: "flex", alignItems: "center", justifyContent: "center", opacity: state === "loading" ? 0.5 : 1, transition: "all 0.15s" }} onMouseEnter={(e) => { if (state !== "loading") { e.currentTarget.style.color = "#38bdf8"; e.currentTarget.style.borderColor = "#38bdf8"; } }} onMouseLeave={(e) => { e.currentTarget.style.color = "#94a3b8"; e.currentTarget.style.borderColor = "#334155"; }}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="23 4 23 10 17 10"></polyline>
            <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"></path>
          </svg>
        </button>
        <button
          onClick={() => setShowTradeStats(true)}
          title="交易统计：查看交易记录、同步持仓、导出 Excel"
          style={{
            padding: "4px 10px",
            background: "transparent",
            border: "1px solid #334155",
            borderRadius: "4px",
            cursor: "pointer",
            color: "#94a3b8",
            display: "flex",
            alignItems: "center",
            gap: "4px",
            fontSize: "11px",
            transition: "all 0.15s",
            whiteSpace: "nowrap",
          }}
          onMouseEnter={(e) => { e.currentTarget.style.color = "#38bdf8"; e.currentTarget.style.borderColor = "#38bdf8"; }}
          onMouseLeave={(e) => { e.currentTarget.style.color = "#94a3b8"; e.currentTarget.style.borderColor = "#334155"; }}
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <line x1="18" y1="20" x2="18" y2="10" />
            <line x1="12" y1="20" x2="12" y2="4" />
            <line x1="6" y1="20" x2="6" y2="14" />
          </svg>
          统计
        </button>
        <button
          onClick={() => setShowCitySelector(true)}
          title="编辑城市：选择要加载与交易的城市"
          style={{
            padding: "4px 8px",
            background: "transparent",
            border: "1px solid #334155",
            borderRadius: "4px",
            cursor: "pointer",
            color: "#94a3b8",
            display: "flex",
            alignItems: "center",
            gap: "4px",
            fontSize: "11px",
            transition: "all 0.15s",
            whiteSpace: "nowrap",
          }}
          onMouseEnter={(e) => { e.currentTarget.style.color = "#38bdf8"; e.currentTarget.style.borderColor = "#38bdf8"; }}
          onMouseLeave={(e) => { e.currentTarget.style.color = "#94a3b8"; e.currentTarget.style.borderColor = "#334155"; }}
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z" />
            <circle cx="12" cy="10" r="3" />
          </svg>
          {selectedCitySlugs ? `${selectedCitySlugs.size}/${allCities.length || "--"}` : allCities.length > 0 ? `${allCities.length}/${allCities.length}` : "--"}
          {" "}城市
        </button>
        <span style={{ fontVariantNumeric: "tabular-nums" }}>
          {gmtNow.toISOString().replace("T", " ").slice(0, 16)} GMT
        </span>
        <span>
          {state === "loading"
            ? `加载中 ${progress.total > 0 ? `(${progress.processed}/${progress.total})` : "..."}${weatherProgress.total > 0 ? ` | 天气 (${weatherProgress.processed}/${weatherProgress.total})` : ""}`
            : state === "success"
            ? `${sqliteCities.length} cities${weatherProgress.total > 0 && weatherProgress.processed < weatherProgress.total ? ` | 天气 (${weatherProgress.processed}/${weatherProgress.total})` : ""}`
            : "加载失败"}
        </span>
        <span style={{ marginLeft: "auto", display: "flex", alignItems: "center", gap: "12px", fontVariantNumeric: "tabular-nums", paddingRight: "20px" }}>
          <span style={{ color: "#94a3b8" }}>持仓: <span style={{ color: "#e2e8f0", fontWeight: 600 }}>{positionStats.count}</span></span>
          <span style={{ color: "#94a3b8" }}>盈亏: <span style={{ color: positionStats.totalPnl >= 0 ? "#10b981" : "#ef4444", fontWeight: 600 }}>{positionStats.totalPnl >= 0 ? "+" : ""}${positionStats.totalPnl.toFixed(2)}</span></span>
          <span style={{ color: "#94a3b8" }}>收益率: <span style={{ color: positionStats.pnlPct >= 0 ? "#10b981" : "#ef4444", fontWeight: 600 }}>{positionStats.pnlPct >= 0 ? "+" : ""}{positionStats.pnlPct.toFixed(1)}%</span></span>
        </span>
      </div>

      {/* Settings Modal */}
      {showSettings && <SettingsModal open={showSettings} onClose={() => setShowSettings(false)} />}

      {/* Trade Statistics Modal */}
      <TradeStatsModal open={showTradeStats} onClose={() => setShowTradeStats(false)} cities={allCities} />

      {/* City Selector Modal */}
      <CitySelectorModal
        open={showCitySelector}
        onClose={() => setShowCitySelector(false)}
        cities={allCities}
        selectedSlugs={selectedCitySlugs}
        onToggle={toggleCity}
        onSelectAll={selectAllCities}
        onSelectNone={selectNoCities}
        onSyncFromPolymarket={handleUpdateCities}
        onStationCodeUpdated={handleStationCodeUpdated}
      />


      {/* Toast 通知 */}
      {toasts.length > 0 && (
        <div style={{ position: "fixed", bottom: "40px", right: "16px", zIndex: 1000, display: "flex", flexDirection: "column", gap: "8px" }}>
          {toasts.map((t) => (
            <div
              key={t.id}
              style={{
                padding: "10px 16px",
                borderRadius: "6px",
                fontSize: "13px",
                color: "#fff",
                background: t.type === "success" ? "rgba(16,185,129,0.95)" : "rgba(239,68,68,0.95)",
                boxShadow: "0 4px 12px rgba(0,0,0,0.4)",
                maxWidth: "400px",
                animation: "slideInRight 0.3s ease",
              }}
            >
              {t.message}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
