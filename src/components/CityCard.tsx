import { memo, useEffect, useMemo, useRef, useState } from "react";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import type { CityTempMarkets, TempThreshold } from "../types/temperature";

/// SQLite cities 表对应的行
export interface CityRow {
  slug: string;
  city_name: string;
  utc_offset: string;
  iana_tz: string;
  unit: string;
station_name: string | null;
  station_code: string | null;
  station_url: string | null;
  avatar: string | null;
  lat: number | null;
  lon: number | null;
  updated_at: string;
}

interface TokenPrice {
  bid: number;
  ask: number;
  mid: number;
  timestamp: number;
  source?: "gamma" | "ws" | "rest";
}

type PriceMap = Map<string, TokenPrice>;

/// 持仓数据
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

interface CityCardProps {
  index: number;
  sqliteCity: CityRow;
  marketData: CityTempMarkets | null;
  priceMap: PriceMap;
  weather: CityWeather | null;
  positions: Position[];
  onOpenPosition: (threshold: TempThreshold, city: string, cityTz: string, askPrice: number) => Promise<void>;
  onClosePosition: (tokenId: string) => Promise<void>;
  onAnalyzeOne?: (slug: string) => void;
  isAnalyzing?: boolean;
  analyzingResult?: string | null;
  cardRef?: (el: HTMLDivElement | null) => void;
  tpVal?: number;
  slVal?: number;
  /** 全局时间 tick（由 App 级单一 interval 驱动），替代每卡片独立 setInterval */
  timeTick?: number;
}

function avatarColor(name: string): string {
  const hue = [...name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7);
  return `hsl(${hue}, 65%, 45%)`;
}

function cityDisplayName(slug: string): string {
  return slug
    .split("-")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

function pct(price: number): string {
  return (price * 100).toFixed(1) + "%";
}

function localTime(tz: string): string {
  try {
    return new Intl.DateTimeFormat("en-GB", {
      timeZone: tz,
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }).format(new Date()).replace(",", " ");
  } catch {
    return "-- --:--";
  }
}

function formatLabel(label: string): string {
  return label.replace(" or higher", "+").replace(" or below", "-");
}

/// 温度显示与 NWS timeseries 页面口径一致：Math.round 取整显示
/// （页面 obs.js 对温度列用 Math.round 渲染，不显示小数）；
/// 原始浮点（如 WE 观测 80.6...）不直出，取整后与页面所见一致
function fmtTemp(v: number | null | undefined): string {
  return v != null ? String(Math.round(v)) : "--";
}


interface ThresholdItem {
  threshold: TempThreshold;
  marketType: "highest";
  eventSlug: string;
}

/// 单个温度档位方块，独立跟踪概率变化触发闪烁
function ThresholdChip({
  threshold: t,
  priceMap,
  idx,
  onDoubleClick,
  hasPosition,
}: {
  threshold: TempThreshold;
  priceMap: PriceMap;
  idx: number;
  onDoubleClick: () => void;
  hasPosition: boolean;
}) {
  const price = priceMap.get(t.no_token_id);
  const prob = price?.mid ?? t.no_price;
  const bid = price?.bid;
  const ask = price?.ask;
  const hasFullPrice = bid != null && ask != null;
  const probPct = prob * 100;
  const isSettled = prob >= 0.999;
  const settledColor = "#475569";
  const grayColor = "#475569";
  const probColor = isSettled
    ? settledColor
    : !hasFullPrice
      ? grayColor
      : probPct > 85 ? "#10b981" : probPct > 50 ? "#f59e0b" : "#f43f5e";

  // 跟踪 mid 值变化，变化时触发闪烁
  // 使用 ref 直接操作 DOM class，避免 setState 导致重渲染和 flashKey 无限增长
  const prevMidRef = useRef<number | null>(null);
  const lastFlashTimeRef = useRef<number>(0);
  const flashRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const currentMid = price?.mid ?? null;
    const prevMid = prevMidRef.current;
    if (prevMid !== null && currentMid !== null && prevMid !== currentMid) {
      const now = Date.now();
      if (now - lastFlashTimeRef.current >= 800) {
        lastFlashTimeRef.current = now;
        // 直接操作 DOM 触发 CSS 动画重播，不经过 React setState
        const el = flashRef.current;
        if (el) {
          el.classList.remove("flash-active");
          // 强制 reflow 使动画可重播
          void el.offsetWidth;
          el.classList.add("flash-active");
        }
      }
    }
    prevMidRef.current = currentMid;
  }, [price?.mid]);

  return (
    <div
      key={t.no_token_id}
      className="threshold-chip"
      onDoubleClick={isSettled ? undefined : onDoubleClick}
      style={{
        position: "relative" as const,
        display: "flex",
        flexDirection: "column" as const,
        alignItems: "center",
        justifyContent: "center",
        width: "52px",
        height: "48px",
        padding: "2px 1px",
        background: hasPosition ? "#1a3a2a" : "#0f172a",
        borderRadius: "4px",
        fontSize: "10px",
        fontVariantNumeric: "tabular-nums",
        fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
        border: hasPosition ? "1px solid #10b98155" : "1px solid #1e293b",
        gap: "1px",
        animationDelay: `${Math.min(idx * 0.04, 0.6)}s`,
        cursor: isSettled ? "default" : "pointer",
      }}
    >
      {/* 概率变化时的背景闪烁 overlay（通过 ref 直接操作 class，不触发 React 重渲染）*/}
      <span ref={flashRef} className="threshold-chip-flash-overlay" />
      {/* 第一行：温度档位 */}
      <span style={{ color: isSettled ? settledColor : !hasFullPrice ? grayColor : "#cbd5e1", fontWeight: 600, lineHeight: 1.1 }}>
        {formatLabel(t.label)}
      </span>
      {/* 第二行：概率 */}
      <span style={{ color: probColor, fontWeight: 600, lineHeight: 1.1 }}>
        {pct(prob)}
      </span>
      {/* 第三行：买一价 / 卖一价 */}
      <span style={{ color: isSettled ? settledColor : !hasFullPrice ? grayColor : "#64748b", fontSize: "9px", lineHeight: 1.1 }}>
        {bid != null ? bid.toFixed(3) : "--"}/{ask != null ? ask.toFixed(3) : "--"}
      </span>
    </div>
  );
}

/// CityCard memo 自定义比较：priceMap 引用变化时只检查该城市相关 token 的价格是否实际变化
function cityCardAreEqual(prev: CityCardProps, next: CityCardProps): boolean {
  // priceMap 现在是城市级子集（~12 entries），引用稳定：仅该城市价格变化时才变
  // 因此直接浅比较即可，无需逐 token 遍历
  if (
    prev.index !== next.index ||
    prev.sqliteCity !== next.sqliteCity ||
    prev.marketData !== next.marketData ||
    prev.priceMap !== next.priceMap ||
    prev.weather !== next.weather ||
    prev.positions !== next.positions ||
    prev.onOpenPosition !== next.onOpenPosition ||
    prev.onClosePosition !== next.onClosePosition ||
    prev.onAnalyzeOne !== next.onAnalyzeOne ||
    prev.isAnalyzing !== next.isAnalyzing ||
    prev.analyzingResult !== next.analyzingResult ||
    prev.tpVal !== next.tpVal ||
    prev.slVal !== next.slVal ||
    prev.timeTick !== next.timeTick
  ) {
    return false;
  }
  return true;
}

const CityCard = memo(function CityCard({
  index,
  sqliteCity,
  marketData,
  priceMap,
  weather,
  positions,
  onOpenPosition,
  onClosePosition,
  onAnalyzeOne,
  isAnalyzing,
  analyzingResult,
  cardRef,
  tpVal = 0,
  slVal = 0,
  timeTick = 0,
}: CityCardProps) {
  const { slug, city_name, iana_tz, avatar, unit } = sqliteCity;

  // 本地时间自刷新：由 App 级单一 interval 驱动 timeTick prop，避免 94 个独立 setInterval
  const time = useMemo(() => localTime(iana_tz), [iana_tz, timeTick]);

  // 持仓 token_id 集合，用于标记已有持仓的档位
  const positionTokenIds = useMemo(() => new Set(positions.map((p) => p.token_id)), [positions]);

  // token_id -> threshold label 映射，用于持仓面板显示档位标签
  const tokenIdToLabel = useMemo(() => {
    const m = new Map<string, string>();
    if (marketData?.highest) {
      for (const t of marketData.highest.thresholds) {
        m.set(t.no_token_id, t.label);
        m.set(t.yes_token_id, t.label);
      }
    }
    return m;
  }, [marketData]);

  const displayName = city_name || cityDisplayName(slug);
  const initial = displayName.charAt(0).toUpperCase();
  const bgColor = useMemo(() => avatarColor(slug), [slug]);

  // 头像：优先用市场数据的 icon（小图），fallback image，再 fallback DB avatar
  const avatarUrl = marketData?.highest?.icon
    || marketData?.highest?.image
    || avatar;

  const thresholds = useMemo<ThresholdItem[]>(() => {
    const result: ThresholdItem[] = [];
    if (marketData?.highest) {
      for (const t of marketData.highest.thresholds) {
        result.push({
          threshold: t,
          marketType: "highest",
          eventSlug: marketData.highest.event_slug,
        });
      }
    }
    // 按温度值排序
    result.sort((a, b) => {
      const ta = parseInt(a.threshold.label.match(/-?\d+/)?.[0] ?? "999", 10);
      const tb = parseInt(b.threshold.label.match(/-?\d+/)?.[0] ?? "999", 10);
      return ta - tb;
    });
    return result;
  }, [marketData]);

  // 有温度档位数据时显示，无数据时淡出隐藏
  const hasThresholds = thresholds.length > 0;
  const [visible, setVisible] = useState(hasThresholds);

  // AWC/MET 天气数据淡入：有数据时从暗到亮过渡
  const hasWeather = !!(weather?.awc?.current != null || weather?.awc?.max != null || weather?.met_forecast?.length);
  const [weatherVisible, setWeatherVisible] = useState(hasWeather);

  useEffect(() => {
    if (hasWeather) {
      setWeatherVisible(true);
    } else {
      const timer = setTimeout(() => setWeatherVisible(false), 400);
      return () => clearTimeout(timer);
    }
  }, [hasWeather]);

  // ST 天气数据淡入
  const hasStWeather = !!(weather?.st?.high != null || weather?.st?.current != null);
  const [stVisible, setStVisible] = useState(hasStWeather);

  useEffect(() => {
    if (hasStWeather) {
      setStVisible(true);
    } else {
      const timer = setTimeout(() => setStVisible(false), 400);
      return () => clearTimeout(timer);
    }
  }, [hasStWeather]);

  useEffect(() => {
    if (hasThresholds) {
      setVisible(true);
    } else {
      const timer = setTimeout(() => setVisible(false), 400);
      return () => clearTimeout(timer);
    }
  }, [hasThresholds]);

  if (!visible && !hasThresholds) return null;

  const fadeClass = hasThresholds ? "city-card-in" : "city-card-out";

  return (
    <div
      ref={cardRef}
      className={fadeClass}
      style={{
        animationDelay: hasThresholds ? `${Math.min(index * 0.025, 0.8)}s` : "0s",
        background: "#111827",
        borderRadius: "8px",
        margin: "3px 8px",
        padding: "7px 12px",
        border: "1px solid #1e293b",
        outline: "2px solid",
        outlineOffset: "-1px",
        outlineColor: isAnalyzing ? "#fbbf24" : "rgba(251, 191, 36, 0)",
        boxShadow: isAnalyzing
          ? "0 0 12px rgba(251, 191, 36, 0.4), 0 0 24px rgba(251, 191, 36, 0.2)"
          : "0 0 12px rgba(251, 191, 36, 0), 0 0 24px rgba(251, 191, 36, 0)",
        transition: "outline-color 0.6s ease, box-shadow 0.6s ease",
        position: "relative" as const,
        // 屏幕外卡片懒渲染：跳过 layout/paint，仅保留占位空间
        contentVisibility: "auto" as const,
        containIntrinsicSize: "64px",
      }}
    >
      {/* 单条执行按钮 — 绝对定位到卡片左上角，不占行宽度 */}
      <button
        onClick={(e) => {
          e.stopPropagation();
          onAnalyzeOne?.(slug);
        }}
        disabled={isAnalyzing}
        title={isAnalyzing ? "分析中..." : "执行单条开仓判断"}
        style={{
          position: "absolute" as const,
          top: "2px",
          left: "2px",
          width: "18px",
          height: "18px",
          border: "none",
          background: "transparent",
          cursor: isAnalyzing ? "wait" : "pointer",
          color: isAnalyzing ? "#fbbf24" : "#64748b",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          padding: 0,
          zIndex: 10,
          transition: "color 0.15s",
        }}
        onMouseEnter={(e) => { if (!isAnalyzing) e.currentTarget.style.color = "#38bdf8"; }}
        onMouseLeave={(e) => { if (!isAnalyzing) e.currentTarget.style.color = "#64748b"; }}
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
          <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"></polygon>
        </svg>
      </button>
      {/* 三列布局：城市信息 | 温度档位+天气 | 持仓 */}
      <div style={{ display: "flex", alignItems: "stretch", gap: "8px", minHeight: "48px" }}>
        {/* 第一列：序号 + 头像 + 城市名 + 当地时间 */}
        <div style={{ display: "flex", alignItems: "center", gap: "8px", flexShrink: 0 }}>
        <span
          style={{
            fontSize: "11px",
            color: "#475569",
            minWidth: "20px",
            textAlign: "right",
            fontVariantNumeric: "tabular-nums",
            marginLeft: "-5px",
          }}
        >
          {index}
        </span>
        {/* 城市头像 */}
        {avatarUrl ? (
          <img
            src={avatarUrl}
            alt={displayName}
            loading="lazy"
            decoding="async"
            onError={(e) => {
              const target = e.currentTarget;
              target.style.display = "none";
              const fallback = target.nextElementSibling as HTMLElement | null;
              if (fallback) fallback.style.display = "flex";
            }}
            style={{
              width: "26px",
              height: "26px",
              borderRadius: "50%",
              objectFit: "cover",
              flexShrink: 0,
            }}
          />
        ) : null}
        <div
          style={{
            width: "26px",
            height: "26px",
            borderRadius: "50%",
            background: bgColor,
            display: avatarUrl ? "none" : "flex",
            alignItems: "center",
            justifyContent: "center",
            fontSize: "12px",
            fontWeight: 700,
            color: "#fff",
            flexShrink: 0,
            textShadow: "0 1px 2px rgba(0,0,0,0.4)",
          }}
        >
          {initial}
        </div>
        {/* 城市名 + 当地时间（垂直排列，固定宽度对齐温度档位） */}
        <div style={{ display: "flex", flexDirection: "column", gap: "0px", flexShrink: 0, width: "90px" }}>
          <a
            href="#"
            onClick={(e) => {
              e.preventDefault();
              const ev = marketData?.highest;
              if (ev) openUrl(`https://polymarket.com/event/${ev.event_slug}`);
            }}
            style={{
              fontSize: "13px",
              fontWeight: 600,
              color: marketData ? "#e2e8f0" : "#64748b",
              textDecoration: "none",
              cursor: marketData ? "pointer" : "default",
              whiteSpace: "nowrap",
              lineHeight: 1.2,
            }}
          >
            {displayName}
          </a>
          <span
            style={{
              fontSize: "10px",
              color: "#94a3b8",
              fontVariantNumeric: "tabular-nums",
              fontFamily: "'SF Mono','Menlo','Consolas',monospace",
              lineHeight: 1.2,
              marginTop: "1px",
            }}
          >
            {time}
          </span>
        </div>
        </div>
        {/* 第二列：温度档位 + AWC/MET 天气（垂直堆叠） */}
        <div style={{ display: "flex", flexDirection: "column", gap: "2px", flex: 1 }}>
        {/* 温度档位方块 */}
        <div style={{ display: "flex", gap: "4px", flexWrap: "wrap" as const, flex: "0 0 auto" }}>
          {thresholds.length === 0 && (
            <span style={{ fontSize: "10px", color: "#374151" }}>No active markets</span>
          )}
          {thresholds.map(({ threshold: t }, idx) => {
            const price = priceMap.get(t.no_token_id);
            const hasPosition = positionTokenIds.has(t.no_token_id);
            return (
              <ThresholdChip
                key={t.no_token_id}
                threshold={t}
                priceMap={priceMap}
                idx={idx}
                hasPosition={hasPosition}
                onDoubleClick={() => {
                  if (hasPosition) return;
                  const askPrice = price?.ask ?? 0;
                  if (askPrice <= 0) return;
                  onOpenPosition(t, slug, iana_tz, askPrice);
                }}
              />
            );
          })}
        </div>
        {/* AWC/MET 天气数据 */}
        <div style={{ display: "flex", alignItems: "center", gap: "10px", fontSize: "10px" }}>
        {/* ST 天气数据（weather.gov 或 wunderground.com） */}
        <div style={{ display: "flex", alignItems: "center", gap: "3px" }}>
          <span
            style={{
              color: "#475569",
              fontWeight: 700,
              letterSpacing: "0.5px",
            }}
          >
            ST
          </span>
          <span
            style={{
              color: weather?.st?.high != null || weather?.st?.current != null ? "#93c5fd" : "#374151",
              fontVariantNumeric: "tabular-nums",
              fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
              opacity: stVisible ? 1 : 0,
              transition: "opacity 0.4s ease",
            }}
          >
            {weather?.st
              ? `${fmtTemp(weather.st.high)}/${fmtTemp(weather.st.current)}${unit}`
              : `--${unit}`}
          </span>
          {weather?.st?.condition && (
            <span
              style={{
                color: "#64748b",
                fontSize: "9px",
                fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
                opacity: stVisible ? 1 : 0,
                transition: "opacity 0.4s ease",
              }}
            >
              {weather.st.condition}
            </span>
          )}
        </div>
        <div style={{ width: "1px", height: "10px", background: "#1e293b" }} />
        {/* AWC 实况温度 */}
        <div style={{ display: "flex", alignItems: "center", gap: "3px" }}>
          <span
            style={{
              color: "#475569",
              fontWeight: 700,
              letterSpacing: "0.5px",
            }}
          >
            AWC
          </span>
          <span
            style={{
              color: weather?.awc?.current != null || weather?.awc?.max != null ? "#93c5fd" : "#374151",
              fontVariantNumeric: "tabular-nums",
              fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
              minWidth: "44px",
              opacity: weatherVisible ? 1 : 0,
              transition: "opacity 0.4s ease",
            }}
          >
            {weather?.awc
              ? `${fmtTemp(weather.awc.max)}/${fmtTemp(weather.awc.current)}${unit}`
              : `--${unit}`}
          </span>
        </div>
        <div style={{ width: "1px", height: "10px", background: "#1e293b" }} />
        {/* MET Norway 逐小时预报 */}
        <div style={{ display: "flex", alignItems: "center", gap: "3px" }}>
          <span
            style={{
              color: "#475569",
              fontWeight: 700,
              letterSpacing: "0.5px",
            }}
          >
            MET
          </span>
          <span
            style={{
              color: weather?.met_forecast?.length ? "#93c5fd" : "#374151",
              fontVariantNumeric: "tabular-nums",
              fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
              display: "inline-flex",
              gap: "8px",
              opacity: weatherVisible ? 1 : 0,
              transition: "opacity 0.4s ease",
            }}
          >
            {weather?.met_forecast?.length
              ? (() => {
                  const maxTemp = Math.max(...weather.met_forecast.map((f) => f.temp));
                  return weather.met_forecast.map((f, i) => (
                    <span
                      key={i}
                      style={{ color: f.temp === maxTemp ? "#8b1a1a" : undefined }}
                    >
                      {fmtTemp(f.temp)}{unit}
                    </span>
                  ));
                })()
              : "--"}
          </span>
        </div>
        <div style={{ width: "1px", height: "10px", background: "#1e293b" }} />
        {/* 昨天温度 + 天气状况 */}
        <div style={{ display: "flex", alignItems: "center", gap: "3px" }}>
          <span
            style={{
              color: "#475569",
              fontWeight: 700,
              letterSpacing: "0.5px",
            }}
          >
            YDA
          </span>
          <span
            style={{
              color: weather?.st?.yesterdayHigh != null ? "#93c5fd" : "#374151",
              fontVariantNumeric: "tabular-nums",
              fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
              opacity: stVisible ? 1 : 0,
              transition: "opacity 0.4s ease",
            }}
          >
            {weather?.st?.yesterdayHigh != null
              ? `${fmtTemp(weather.st.yesterdayHigh)}${unit}`
              : `--${unit}`}
          </span>
          {weather?.st?.yesterdayHighHour != null && (
            <span
              style={{
                color: "#64748b",
                fontSize: "9px",
                fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
                opacity: stVisible ? 1 : 0,
                transition: "opacity 0.4s ease",
              }}
            >
              {`${weather.st.yesterdayHighHour}:00`}
            </span>
          )}
          {weather?.st?.yesterdayCondition && (
            <span
              style={{
                color: "#64748b",
                fontSize: "9px",
                fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
                opacity: stVisible ? 1 : 0,
                transition: "opacity 0.4s ease",
              }}
            >
              {weather.st.yesterdayCondition}
            </span>
          )}
        </div>
        </div>
        </div>
        {/* 第三列：持仓数据 */}
        {positions.length > 0 && (
          <div style={{ display: "flex", flexDirection: "column", gap: "3px", marginLeft: "auto", alignSelf: "stretch", justifyContent: "center", flexShrink: 0 }}>
            {positions.map((pos) => {
              // 防御性：如果 avg_price 或 size 为 0，说明数据尚未同步，显示 Pending
              const isPending = pos.avg_price === 0 || pos.size === 0;
              const curBid = priceMap.get(pos.token_id)?.bid ?? pos.cur_price;
              const cost = pos.avg_price * pos.size;
              const currentValue = curBid * pos.size;
              const pnl = currentValue - cost;
              const pnlPct = cost > 0 ? (pnl / cost) * 100 : 0;
              const isProfit = pnl >= 0;
              // 止损/止盈边框：bid <= tpVal 红色，bid >= slVal 绿色
              const hitStopLoss = tpVal > 0 && curBid > 0 && curBid <= tpVal;
              const hitTakeProfit = slVal > 0 && curBid > 0 && curBid >= slVal;
              const borderColor = hitStopLoss ? "#ef4444" : hitTakeProfit ? "#22c55e" : "#1e293b";
              return (
                <div
                  key={pos.token_id}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: "6px",
                    padding: "3px 8px",
                    background: "#0f172a",
                    border: `1px solid ${borderColor}`,
                    borderRadius: "4px",
                    fontSize: "10px",
                    fontVariantNumeric: "tabular-nums",
                    fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
                  }}
                >
                  {/* 档位标签 */}
                  <span style={{ color: "#94a3b8", fontWeight: 600, whiteSpace: "nowrap" }}>
                    {pos.side === "NO" ? "\u2717" : "\u2713"}{formatLabel(tokenIdToLabel.get(pos.token_id) || "")}
                  </span>
                  <span style={{ width: "1px", height: "12px", background: "#334155" }} />
                  {isPending ? (
                    <span style={{ color: "#f59e0b", fontWeight: 600 }}>Syncing...</span>
                  ) : (
                    <>
                      {/* 开仓价格 */}
                      <span style={{ color: "#64748b" }}>@
                        <span style={{ color: "#cbd5e1", fontWeight: 600 }}>{pos.avg_price.toFixed(3)}</span>
                      </span>
                      {/* 交易金额 */}
                      <span style={{ color: "#64748b" }}>${cost.toFixed(2)}</span>
                      <span style={{ width: "1px", height: "12px", background: "#334155" }} />
                      {/* 盈利额 */}
                      <span style={{ color: isProfit ? "#10b981" : "#f43f5e", fontWeight: 600 }}>
                        {isProfit ? "+" : ""}${pnl.toFixed(2)}
                      </span>
                      {/* 盈利率 */}
                      <span style={{ color: isProfit ? "#10b981" : "#f43f5e", fontWeight: 600 }}>
                        ({isProfit ? "+" : ""}{pnlPct.toFixed(1)}%)
                      </span>
                    </>
                  )}
                  {/* 平仓按钮（圆圈叉，双击平仓） */}
                  <button
                    onDoubleClick={() => onClosePosition(pos.token_id)}
                    title="Double-click to close"
                    style={{
                      width: "14px",
                      height: "14px",
                      borderRadius: "50%",
                      border: "1px solid #6b7280",
                      background: "transparent",
                      color: "#6b7280",
                      fontSize: "8px",
                      padding: 0,
                      cursor: "pointer",
                      userSelect: "none",
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      alignSelf: "center",
                      flexShrink: 0,
                      transition: "all 0.2s",
                    }}
                    onMouseEnter={(e) => {
                      e.currentTarget.style.borderColor = "#dc2626";
                      e.currentTarget.style.color = "#dc2626";
                    }}
                    onMouseLeave={(e) => {
                      e.currentTarget.style.borderColor = "#6b7280";
                      e.currentTarget.style.color = "#6b7280";
                    }}
                  >
                    ✕
                  </button>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* LLM 分析结果遮罩层 — 绝对定位覆盖在当前行上 */}
      {isAnalyzing && analyzingResult && (
        <div style={{
          position: "absolute" as const,
          top: "0",
          left: "0",
          right: "0",
          bottom: "0",
          background: "rgba(15, 23, 42, 0.6)",
          borderRadius: "8px",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          padding: "0 12px",
          fontSize: "11px",
          color: "#fbbf24",
          fontFamily: "'Segoe UI','Microsoft YaHei',sans-serif",
          lineHeight: 1.4,
          zIndex: 10,
          backdropFilter: "blur(2px)",
        }}>
          {analyzingResult}
        </div>
      )}
    </div>
  );
}, cityCardAreEqual);

export default CityCard;
