import { useState, useMemo, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

// ---- 类型 ----
interface DailyPnl {
  date: string;
  pnl: number;
}

// ---- 本金持久化 ----
const PRINCIPAL_KEY = "woolbrush_principal_v2";

function loadPrincipal(): number | null {
  try {
    const raw = localStorage.getItem(PRINCIPAL_KEY);
    if (raw) return parseFloat(raw);
    // 兼容旧 key
    const old = localStorage.getItem("woolbrush_principal");
    if (old) return parseFloat(old);
  } catch { /* ignore */ }
  return null;
}

function savePrincipal(value: number) {
  try { localStorage.setItem(PRINCIPAL_KEY, String(value)); } catch { /* ignore */ }
}

// ---- CSS keyframes ----
const STYLE_ID = "pnl-chart-styles";
function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
    @keyframes pnlBarGrow {
      from { transform: scaleY(0); }
      to   { transform: scaleY(1); }
    }
    @keyframes pnlFadeIn {
      from { opacity: 0; }
      to   { opacity: 1; }
    }
    @keyframes pnlBorderFlow {
      0%   { background-position: 0% 50%; }
      50%  { background-position: 100% 50%; }
      100% { background-position: 0% 50%; }
    }
    @keyframes pnlDotPulse {
      0%, 100% { opacity: 0.3; }
      50%      { opacity: 0.6; }
    }
    .pnl-container {
      animation: pnlFadeIn 0.5s ease;
    }
    .pnl-bar-rect {
      animation: pnlBarGrow 0.5s ease-out both;
    }
    .pnl-border-glow {
      background: linear-gradient(90deg, transparent, #6366f140, #a855f740, #6366f140, transparent);
      background-size: 200% 100%;
      animation: pnlBorderFlow 5s ease infinite;
    }
    .pnl-today-pulse {
      animation: pnlDotPulse 2s ease-in-out infinite;
    }
  `;
  document.head.appendChild(style);
}

// ---- 组件 ----
export function PnlChart({ refreshTick, portfolioValue, onReset, orderMode }: { refreshTick?: number; portfolioValue: number | null; onReset?: () => void; orderMode?: "fixed" | "percent" }) {
  const [hovered, setHovered] = useState<number | null>(null);
  const [rateMode, setRateMode] = useState<"annual" | "monthly">("annual");
  const [chartData, setChartData] = useState<DailyPnl[]>([]);
  const [principal, setPrincipal] = useState<number | null>(loadPrincipal);
  const [resetting, setResetting] = useState(false);

  // 如果 localStorage 中没有本金记录，用当前总资产初始化并持久化
  useEffect(() => {
    if (principal === null && portfolioValue != null && portfolioValue > 0) {
      savePrincipal(portfolioValue);
      setPrincipal(portfolioValue);
    }
  }, [principal, portfolioValue]);

  useEffect(() => {
    injectStyles();
  }, []);

  const prevTickRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (prevTickRef.current === refreshTick) return;
    prevTickRef.current = refreshTick;
    invoke<DailyPnl[]>("get_daily_pnl", { days: 30 })
      .then((rows) => {
        setChartData(rows);
      })
      .catch((e) => {
        console.warn("Failed to load daily pnl:", e);
      });
  }, [refreshTick]);

  const H = 48;
  const padX = 4;
  const padY = 6;
  const barW = 7;      // 固定柱子宽度
  const barStep = 10;   // 每根柱子步长(含间距)

  // 根据数据量动态计算宽度，确保30天数据全部展开
  const W = padX * 2 + Math.max(chartData.length, 1) * barStep;

  const { bars, totalPnl, midY, annualRate, monthlyRate } = useMemo(() => {
    const maxAbs = Math.max(...chartData.map(d => Math.abs(d.pnl)), 1);
    const innerH = H - padY * 2;
    const midY = padY + innerH / 2;

    const gap = barStep;
    const bars = chartData.map((d, i) => {
      const x = padX + i * gap + (gap - barW) / 2;
      const barH = Math.abs(d.pnl / maxAbs) * (innerH / 2 - 2);
      const y = d.pnl >= 0 ? midY - barH : midY;
      return { x, y, w: barW, h: barH, ...d };
    });

    const totalPnl = chartData.reduce((s, d) => s + d.pnl, 0);

    // 使用重置时记录的本金计算年化/月化收益率
    const p = principal ?? 100;
    const n = chartData.length;
    const dailyAvgPnl = totalPnl / n;

    let annualRate: number;
    let monthlyRate: number;

    if (orderMode === "percent") {
      // % 模式：复利计算
      // 日均收益率 r_d = (final/initial)^(1/n) - 1，其中 final = p + totalPnl
      const ratio = (p + totalPnl) / p;
      const dailyRate = ratio > 0 ? Math.pow(ratio, 1 / n) - 1 : -1;
      annualRate = (Math.pow(1 + dailyRate, 365) - 1) * 100;
      monthlyRate = (Math.pow(1 + dailyRate, 30) - 1) * 100;
    } else {
      // $ 模式：线性计算（原逻辑）
      annualRate = (dailyAvgPnl * 365 / p) * 100;
      monthlyRate = (dailyAvgPnl * 30 / p) * 100;
    }

    return { bars, totalPnl, midY, annualRate, monthlyRate };
  }, [chartData, barW, barStep, principal, orderMode]);

  const isPositive = totalPnl >= 0;
  const currentRate = rateMode === "annual" ? annualRate : monthlyRate;
  const rateLabel = rateMode === "annual" ? "APR" : "MPR";

  // 无数据时隐藏整个组件
  if (chartData.length === 0) return null;

  const handleReset = async () => {
    if (resetting) return;
    setResetting(true);
    try {
      await invoke("clear_trades");
      const pv = portfolioValue ?? 100;
      savePrincipal(pv);
      setPrincipal(pv);
      setChartData([]);
      onReset?.();
    } catch (e) {
      console.warn("Failed to clear trades:", e);
    } finally {
      setResetting(false);
    }
  };

  return (
    <div
      className="pnl-container"
      style={{
        display: "flex",
        alignItems: "center",
        gap: "10px",
        padding: "4px 0",
        borderRadius: "8px",
        background: "transparent",
        position: "relative",
      }}
    >
      {/* 重置按钮 */}
      <button
        onClick={handleReset}
        disabled={resetting}
        title={`清空所有交易记录，并把当前持仓市值设为新的本金基准${principal != null ? `（当前 $${principal.toFixed(2)}）` : ""}`}
        style={{
          background: "none",
          border: "none",
          borderRadius: "4px",
          cursor: resetting ? "wait" : "pointer",
          padding: "2px 6px",
          fontSize: "10px",
          color: "#64748b",
          fontWeight: 600,
          lineHeight: "16px",
          flexShrink: 0,
          opacity: resetting ? 0.5 : 1,
          transition: "color 0.2s, border-color 0.2s",
          alignSelf: "flex-start",
        }}
        onMouseEnter={(e) => { if (!resetting) { e.currentTarget.style.color = "#f59e0b"; e.currentTarget.style.borderColor = "#f59e0b"; } }}
        onMouseLeave={(e) => { e.currentTarget.style.color = "#64748b"; e.currentTarget.style.borderColor = "#334155"; }}
      >
        ⟲
      </button>
      {/* 柱状图 */}
      <div style={{ position: "relative", flexShrink: 0 }}>
        <svg
          width={W}
          height={H}
          style={{ display: "block", overflow: "visible" }}
          onMouseLeave={() => setHovered(null)}
        >
          <defs>
            <linearGradient id="pnlBarP" x1="0%" y1="0%" x2="0%" y2="100%">
              <stop offset="0%" stopColor="#2dd4bf" stopOpacity="0.4" />
              <stop offset="60%" stopColor="#14b8a6" stopOpacity="0.3" />
              <stop offset="100%" stopColor="#0d9488" stopOpacity="0.1" />
            </linearGradient>
            <linearGradient id="pnlBarN" x1="0%" y1="0%" x2="0%" y2="100%">
              <stop offset="0%" stopColor="#fb7185" stopOpacity="0.1" />
              <stop offset="40%" stopColor="#f43f5e" stopOpacity="0.3" />
              <stop offset="100%" stopColor="#e11d48" stopOpacity="0.4" />
            </linearGradient>
            <filter id="pnlGlowS" x="-50%" y="-50%" width="200%" height="200%">
              <feGaussianBlur stdDeviation="1.2" result="blur" />
              <feMerge>
                <feMergeNode in="blur" />
                <feMergeNode in="SourceGraphic" />
              </feMerge>
            </filter>
          </defs>

          {/* 零线 */}
          <line
            x1={padX} y1={midY} x2={W - padX} y2={midY}
            stroke="rgba(148, 163, 184, 0.12)"
            strokeWidth="1"
            strokeDasharray="2 4"
          />

          {/* 柱子 */}
          {bars.map((b, i) => {
            const isHovered = hovered === i;
            const isPos = b.pnl >= 0;
            const color = isPos ? "#2dd4bf" : "#fb7185";
            const isToday = i === bars.length - 1;
            return (
              <g key={i}>
                {/* 热区 */}
                <rect
                  x={b.x - 2} y={0} width={b.w + 4} height={H}
                  fill="transparent"
                  onMouseEnter={() => setHovered(i)}
                />
                {/* 柱体 */}
                <rect
                  className="pnl-bar-rect"
                  x={b.x}
                  y={b.y}
                  width={b.w}
                  height={Math.max(b.h, 1)}
                  rx="1"
                  fill={isPos ? "url(#pnlBarP)" : "url(#pnlBarN)"}
                  style={{
                    animationDelay: `${i * 0.015}s`,
                    transformOrigin: `center ${midY}px`,
                  }}
                />
                {/* 柱顶/底发光线 */}
                <rect
                  x={b.x}
                  y={isPos ? b.y : b.y + Math.max(b.h, 1) - 1}
                  width={b.w}
                  height="1"
                  rx="0.5"
                  fill={color}
                  opacity={isHovered ? 1 : 0.5}
                  filter="url(#pnlGlowS)"
                />
                {/* 今日标记 */}
                {isToday && (
                  <circle
                    cx={b.x + b.w / 2}
                    cy={isPos ? b.y - 3 : b.y + b.h + 3}
                    r="1.5"
                    fill={color}
                    className="pnl-today-pulse"
                  />
                )}
                {/* hover 描边 */}
                {isHovered && (
                  <rect
                    x={b.x - 1}
                    y={b.y - 1}
                    width={b.w + 2}
                    height={Math.max(b.h, 1) + 2}
                    rx="2"
                    fill="none"
                    stroke={color}
                    strokeWidth="1"
                    opacity="0.5"
                  />
                )}
              </g>
            );
          })}
        </svg>
      </div>

      {/* 右侧数据 */}
      <div style={{ display: "flex", flexDirection: "column", gap: "3px", width: "80px", flexShrink: 0 }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: "3px" }}>
          <span style={{
            fontSize: "9px",
            color: "#475569",
            textTransform: "uppercase",
            letterSpacing: "0.8px",
            fontWeight: 600,
          }}>
            {hovered !== null ? chartData[hovered].date.replace("-", "/") : "30天"}
          </span>
          <span style={{
            fontSize: "15px",
            fontWeight: 800,
            fontVariantNumeric: "tabular-nums",
            color: hovered !== null
              ? (chartData[hovered].pnl >= 0 ? "#2dd4bf" : "#fb7185")
              : (isPositive ? "#2dd4bf" : "#fb7185"),
            textShadow: hovered !== null
              ? (chartData[hovered].pnl >= 0
                ? "0 0 12px rgba(45,212,191,0.5), 0 0 4px rgba(45,212,191,0.3)"
                : "0 0 12px rgba(251,113,133,0.5), 0 0 4px rgba(251,113,133,0.3)")
              : (isPositive
                ? "0 0 12px rgba(45,212,191,0.5), 0 0 4px rgba(45,212,191,0.3)"
                : "0 0 12px rgba(251,113,133,0.5), 0 0 4px rgba(251,113,133,0.3)"),
          }}>
            {hovered !== null
              ? `${chartData[hovered].pnl >= 0 ? "+" : ""}${chartData[hovered].pnl.toFixed(2)}`
              : `${isPositive ? "+" : ""}${totalPnl.toFixed(2)}`}
          </span>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: "4px" }}>
          <button
            onClick={() => setRateMode(r => r === "annual" ? "monthly" : "annual")}
            style={{
              background: "none",
              border: "none",
              cursor: "pointer",
              padding: 0,
              fontSize: "9px",
              color: "#475569",
              textTransform: "uppercase",
              letterSpacing: "0.8px",
              fontWeight: 600,
              transition: "color 0.2s ease",
            }}
            onMouseEnter={(e) => { e.currentTarget.style.color = "#818cf8"; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = "#475569"; }}
            title="Toggle APR / MPR"
          >
            {rateLabel}
          </button>
          <span style={{
            fontSize: "11px",
            fontWeight: 700,
            fontVariantNumeric: "tabular-nums",
            color: currentRate >= 0 ? "#2dd4bf" : "#fb7185",
          }}>
            {currentRate >= 0 ? "+" : ""}{currentRate.toFixed(2)}%
          </span>
        </div>
      </div>
    </div>
  );
}
