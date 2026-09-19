import React, { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import * as XLSX from "xlsx";

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

/// 城市时区信息
interface CityTz {
  slug: string;
  iana_tz: string;
}

/// 同步结果
interface SyncResult {
  total_open: number;
  synced: number;
  still_open: number;
  errors: number;
  details: SyncDetail[];
}

interface SyncDetail {
  token_id: string;
  city: string;
  action: string;
  message: string;
}

function cityDisplayName(slug: string): string {
  return slug.split("-").map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(" ");
}

function formatDateTime(iso: string | null): string {
  if (!iso) return "--";
  try {
    const d = new Date(iso);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  } catch {
    return iso;
  }
}

/// 将 UTC 时间戳按城市时区转换为当地时间字符串，格式 yyyy-MM-dd HH:mm
function formatLocalTime(iso: string | null, ianaTz: string | null): string {
  if (!iso) return "--";
  try {
    const tz = ianaTz || "UTC";
    const d = new Date(iso);
    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone: tz,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }).formatToParts(d);
    const get = (t: string) => parts.find((p) => p.type === t)?.value ?? "";
    return `${get("year")}-${get("month")}-${get("day")} ${get("hour")}:${get("minute")}`;
  } catch {
    return iso;
  }
}

/// 截取 question 中的盘口信息（取关键部分）
function formatQuestion(q: string): string {
  if (!q) return "--";
  // 去掉固定前缀，保留后面全部内容
  return q.replace(/^Will the highest temperature in\s+/i, "");
}

interface Props {
  open: boolean;
  onClose: () => void;
  cities: CityTz[];
}

export default function TradeStatsModal({ open, onClose, cities }: Props) {
  // 默认最近一个月
  const defaultEndDate = new Date();
  const defaultStartDate = new Date();
  defaultStartDate.setMonth(defaultStartDate.getMonth() - 1);

  const fmtDate = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;

  const [startDate, setStartDate] = useState(fmtDate(defaultStartDate));
  const [endDate, setEndDate] = useState(fmtDate(defaultEndDate));
  const [timeField, setTimeField] = useState<"open" | "close">("open");
  const [records, setRecords] = useState<TradeRecord[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState<SyncResult | null>(null);

  const fetchRecords = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // end_date 加一天以包含当天
      const endDt = new Date(endDate);
      endDt.setDate(endDt.getDate() + 1);
      const endStr = fmtDate(endDt);
      const data = await invoke<TradeRecord[]>("get_trade_records", {
        startDate,
        endDate: endStr,
        timeField,
      });
      setRecords(data);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setRecords([]);
    } finally {
      setLoading(false);
    }
  }, [startDate, endDate, timeField]);

  const handleSync = useCallback(async () => {
    setSyncing(true);
    setSyncResult(null);
    try {
      const result = await invoke<SyncResult>("sync_open_positions");
      setSyncResult(result);
      // 同步完成后刷新记录
      const endDt = new Date(endDate);
      endDt.setDate(endDt.getDate() + 1);
      const endStr = fmtDate(endDt);
      const data = await invoke<TradeRecord[]>("get_trade_records", {
        startDate,
        endDate: endStr,
        timeField,
      });
      setRecords(data);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
    } finally {
      setSyncing(false);
    }
  }, [startDate, endDate, timeField]);

  useEffect(() => {
    if (open) {
      fetchRecords();
    }
  }, [open, timeField]);

  // 城市 slug -> iana_tz 映射
  const tzMap = useMemo(() => {
    const m = new Map<string, string>();
    for (const c of cities) {
      m.set(c.slug, c.iana_tz);
    }
    return m;
  }, [cities]);

  // 导出 Excel
  const handleExport = useCallback(async () => {
    if (records.length === 0) return;
    const rows = records.map((r) => {
      return {
        市场: formatQuestion(r.question),
        城市: cityDisplayName(r.city),
        温度档位: r.threshold.replace(" or higher", "+").replace(" or below", "-"),
        方向: r.side,
        "开仓时间(UTC)": formatDateTime(r.timestamp),
        本地开仓时间: formatLocalTime(r.timestamp, tzMap.get(r.city) || null),
        开仓价: r.entry_price,
        数量: r.size,
        成本: r.cost,
        "平仓时间(UTC)": r.exit_timestamp ? formatDateTime(r.exit_timestamp) : "",
        本地平仓时间: r.exit_timestamp ? formatLocalTime(r.exit_timestamp, tzMap.get(r.city) || null) : "",
        平仓价: r.exit_price ?? "",
        盈亏: r.realized_pnl ?? "",
        收益率: r.realized_pnl != null && r.cost > 0 ? ((r.realized_pnl / r.cost) * 100).toFixed(1) + "%" : "",
        状态: r.status,
      };
    });
    const ws = XLSX.utils.json_to_sheet(rows);
    const wb = XLSX.utils.book_new();
    XLSX.utils.book_append_sheet(wb, ws, "交易记录");
    const buf = XLSX.write(wb, { type: "array", bookType: "xlsx" }) as ArrayBuffer;
    const defaultName = `trades_${startDate}_${endDate}.xlsx`;
    try {
      await invoke("save_excel", { data: Array.from(new Uint8Array(buf)), defaultName });
    } catch (e) {
      setError(`导出失败：${e}`);
    }
  }, [records, startDate, endDate, tzMap]);

  // 汇总统计
  const stats = (() => {
    const count = records.length;
    const totalCost = records.reduce((sum, r) => sum + r.cost, 0);
    const totalPnl = records.reduce((sum, r) => sum + (r.realized_pnl ?? 0), 0);
    return { count, totalCost, totalPnl };
  })();

  if (!open) return null;

  return (
    <div
      style={{
        position: "fixed",
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        background: "rgba(0,0,0,0.6)",
        zIndex: 1000,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        backdropFilter: "blur(4px)",
      }}
      onClick={onClose}
    >
      <div
        style={{
          width: "92%",
          maxWidth: "1200px",
          maxHeight: "88vh",
          background: "#0f172a",
          border: "1px solid #334155",
          borderRadius: "10px",
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
          boxShadow: "0 8px 32px rgba(0,0,0,0.6)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* 头部：标题 + 日期筛选 */}
        <div
          style={{
            flexShrink: 0,
            padding: "14px 20px",
            borderBottom: "1px solid #334155",
            display: "flex",
            alignItems: "center",
            gap: "16px",
          }}
        >
          <h2 style={{ margin: 0, fontSize: "16px", fontWeight: 700, color: "#e2e8f0", whiteSpace: "nowrap" }}>
            交易统计
          </h2>
          <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
            <div style={{ display: "flex", border: "1px solid #334155", borderRadius: "4px", overflow: "hidden" }}>
              {(["open", "close"] as const).map((f) => (
                <button
                  key={f}
                  onClick={() => setTimeField(f)}
                  style={{
                    padding: "4px 10px",
                    fontSize: "12px",
                    fontWeight: 600,
                    background: timeField === f ? "#1e293b" : "transparent",
                    color: timeField === f ? "#38bdf8" : "#64748b",
                    border: "none",
                    cursor: "pointer",
                    transition: "all 0.15s",
                  }}
                >
                  {f === "open" ? "开仓时间" : "平仓时间"}
                </button>
              ))}
            </div>
            <input
              type="date"
              value={startDate}
              onChange={(e) => setStartDate(e.target.value)}
              style={{
                padding: "4px 8px",
                fontSize: "12px",
                color: "#e2e8f0",
                background: "#1e293b",
                border: "1px solid #334155",
                borderRadius: "4px",
                outline: "none",
                colorScheme: "dark",
              }}
            />
            <span style={{ color: "#64748b", fontSize: "12px" }}>~</span>
            <input
              type="date"
              value={endDate}
              onChange={(e) => setEndDate(e.target.value)}
              style={{
                padding: "4px 8px",
                fontSize: "12px",
                color: "#e2e8f0",
                background: "#1e293b",
                border: "1px solid #334155",
                borderRadius: "4px",
                outline: "none",
                colorScheme: "dark",
              }}
            />
            <button
              onClick={fetchRecords}
              disabled={loading}
              style={{
                padding: "4px 12px",
                fontSize: "12px",
                fontWeight: 600,
                background: loading ? "#334155" : "#1e293b",
                color: loading ? "#64748b" : "#38bdf8",
                border: "1px solid #334155",
                borderRadius: "4px",
                cursor: loading ? "not-allowed" : "pointer",
                transition: "all 0.15s",
              }}
            >
              {loading ? "加载中" : "查询"}
            </button>
            <button
              onClick={handleSync}
              disabled={syncing}
              style={{
                padding: "4px 12px",
                fontSize: "12px",
                fontWeight: 600,
                background: syncing ? "#334155" : "#1e293b",
                color: syncing ? "#64748b" : "#f59e0b",
                border: "1px solid #334155",
                borderRadius: "4px",
                cursor: syncing ? "not-allowed" : "pointer",
                transition: "all 0.15s",
                whiteSpace: "nowrap",
              }}
            >
              {syncing ? "同步中" : "同步持仓"}
            </button>
            <button
              onClick={handleExport}
              disabled={records.length === 0}
              style={{
                padding: "4px 12px",
                fontSize: "12px",
                fontWeight: 600,
                background: records.length === 0 ? "#334155" : "#1e293b",
                color: records.length === 0 ? "#64748b" : "#22c55e",
                border: "1px solid #334155",
                borderRadius: "4px",
                cursor: records.length === 0 ? "not-allowed" : "pointer",
                transition: "all 0.15s",
                whiteSpace: "nowrap",
              }}
            >
              导出 Excel
            </button>
          </div>
          <div style={{ flex: 1 }} />
          <button
            onClick={onClose}
            style={{
              width: "28px",
              height: "28px",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              background: "transparent",
              border: "none",
              color: "#64748b",
              cursor: "pointer",
              fontSize: "18px",
              borderRadius: "4px",
            }}
            onMouseEnter={(e) => { e.currentTarget.style.color = "#ef4444"; e.currentTarget.style.background = "rgba(239,68,68,0.1)"; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = "#64748b"; e.currentTarget.style.background = "transparent"; }}
          >
            ✕
          </button>
        </div>

        {/* 同步结果提示栏 */}
        {syncResult && (
          <div
            style={{
              flexShrink: 0,
              padding: "10px 20px",
              borderBottom: "1px solid #334155",
              background: "#1a2332",
              display: "flex",
              alignItems: "center",
              gap: "16px",
              fontSize: "12px",
            }}
          >
            <span style={{ color: "#f59e0b", fontWeight: 700 }}>同步完成：</span>
            <span style={{ color: "#94a3b8" }}>
              已检查：<span style={{ color: "#e2e8f0", fontWeight: 600 }}>{syncResult.total_open}</span>
            </span>
            {syncResult.synced > 0 && (
              <span style={{ color: "#22c55e" }}>
                已同步：<span style={{ fontWeight: 600 }}>{syncResult.synced}</span>
              </span>
            )}
            {syncResult.still_open > 0 && (
              <span style={{ color: "#94a3b8" }}>
                仍持仓：<span style={{ fontWeight: 600 }}>{syncResult.still_open}</span>
              </span>
            )}
            {syncResult.errors > 0 && (
              <span style={{ color: "#ef4444" }}>
                错误：<span style={{ fontWeight: 600 }}>{syncResult.errors}</span>
              </span>
            )}
            <div style={{ flex: 1 }} />
            <button
              onClick={() => setSyncResult(null)}
              style={{
                background: "transparent",
                border: "none",
                color: "#64748b",
                cursor: "pointer",
                fontSize: "14px",
                padding: "0 4px",
              }}
            >
              ✕
            </button>
          </div>
        )}

        {/* 表格内容区 */}
        <div style={{ flex: 1, overflow: "auto", minHeight: 0 }}>
          {error && (
            <div style={{ padding: "20px", textAlign: "center", color: "#ef4444", fontSize: "13px" }}>
              错误：{error}
            </div>
          )}
          {!error && records.length === 0 && !loading && (
            <div style={{ padding: "40px", textAlign: "center", color: "#64748b", fontSize: "13px" }}>
              该时段内没有交易记录。
            </div>
          )}
          {records.length > 0 && (
            <React.Fragment>
              <table
                style={{
                width: "100%",
                borderCollapse: "collapse",
                fontSize: "12px",
              }}
            >
              <colgroup>
                <col />
                <col />
                <col />
                <col />
                <col />
                <col />
                <col />
                <col />
                <col />
                <col />
              </colgroup>
              <thead>
                <tr style={{ position: "sticky", top: 0, zIndex: 10 }}>
                  {["市场", "城市", "温度档位", "开仓时间(UTC)", "本地时间", "开仓价", "成本", "平仓时间", "平仓价", "盈亏"].map((h, i) => (
                    <th
                      key={i}
                      style={{
                        padding: "8px 4px",
                        textAlign: i === 5 || i === 6 || i === 9 ? "right" : "left",
                        color: "#94a3b8",
                        fontWeight: 600,
                        fontSize: "11px",
                        textTransform: "uppercase",
                        letterSpacing: "0.5px",
                        background: "#1e293b",
                        borderBottom: "1px solid #334155",
                        whiteSpace: "nowrap",
                      }}
                    >
                      {h}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {records.map((r) => {
                  const pnl = r.realized_pnl;
                  const pnlColor = pnl == null ? "#64748b" : pnl >= 0 ? "#22c55e" : "#ef4444";
                  const isOpen = r.status === "open";
                  const tz = tzMap.get(r.city) || null;
                  return (
                    <tr
                      key={r.id}
                      style={{
                        borderBottom: "1px solid #1e293b",
                        transition: "background 0.1s",
                      }}
                      onMouseEnter={(e) => { e.currentTarget.style.background = "rgba(56,189,248,0.05)"; }}
                      onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                    >
                      <td style={{ padding: "6px 4px 6px 6px", color: "#cbd5e1", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={r.question}>
                        {formatQuestion(r.question)}
                      </td>
                      <td style={{ padding: "6px 4px", color: "#60a5fa", whiteSpace: "nowrap" }}>
                        {cityDisplayName(r.city)}
                      </td>
                      <td style={{ padding: "6px 4px", color: "#f59e0b", fontVariantNumeric: "tabular-nums", whiteSpace: "nowrap", fontWeight: 600 }}>
                        {(r.threshold || "--").replace(" or higher", "+").replace(" or below", "-")}
                      </td>
                      <td style={{ padding: "6px 4px", color: "#64748b", fontVariantNumeric: "tabular-nums", whiteSpace: "nowrap", fontSize: "11px" }}>
                        {formatDateTime(r.timestamp)}
                      </td>
                      <td style={{ padding: "6px 4px", color: "#38bdf8", fontVariantNumeric: "tabular-nums", whiteSpace: "nowrap" }}>
                        {formatLocalTime(r.timestamp, tz)}
                      </td>
                      <td style={{ padding: "6px 4px", color: "#cbd5e1", fontVariantNumeric: "tabular-nums", textAlign: "right", whiteSpace: "nowrap" }}>
                        {r.entry_price.toFixed(3)}
                      </td>
                      <td style={{ padding: "6px 4px", color: "#cbd5e1", fontVariantNumeric: "tabular-nums", textAlign: "right", whiteSpace: "nowrap" }}>
                        ${r.cost.toFixed(2)}
                      </td>
                      <td style={{ padding: "6px 4px", color: isOpen ? "#64748b" : "#cbd5e1", fontVariantNumeric: "tabular-nums", whiteSpace: "nowrap" }}>
                        {isOpen ? <span style={{ fontStyle: "italic", fontSize: "11px" }}>持仓中</span> : formatDateTime(r.exit_timestamp)}
                      </td>
                      <td style={{ padding: "6px 4px", color: r.exit_price != null ? "#cbd5e1" : "#64748b", fontVariantNumeric: "tabular-nums", whiteSpace: "nowrap" }}>
                        {r.exit_price != null ? r.exit_price.toFixed(3) : "--"}
                      </td>
                      <td style={{ padding: "6px 6px 6px 4px", color: pnlColor, fontVariantNumeric: "tabular-nums", textAlign: "right", whiteSpace: "nowrap", fontWeight: 600 }}>
                        {pnl != null ? `${pnl >= 0 ? "+" : ""}$${pnl.toFixed(2)}` : "--"}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {/* 独立统计条 — 避免和表格列宽耦合 */}
            <div
              style={{
                position: "sticky",
                bottom: 0,
                zIndex: 10,
                display: "flex",
                justifyContent: "flex-end",
                alignItems: "center",
                gap: "12px",
                padding: "10px 6px 10px 4px",
                background: "#1e293b",
                borderTop: "1px solid #334155",
                fontSize: "12px",
                whiteSpace: "nowrap",
              }}
            >
              <span style={{ color: "#94a3b8" }}>
                总交易数：<b style={{ color: "#e2e8f0" }}>{stats.count}</b>
              </span>
              <span style={{ color: "#64748b" }}>|</span>
              <span style={{ color: "#94a3b8" }}>
                总成本：<b style={{ color: "#e2e8f0" }}>${stats.totalCost.toFixed(2)}</b>
              </span>
              <span style={{ color: "#64748b" }}>|</span>
              <span style={{ color: "#94a3b8" }}>总盈亏：</span>
              <b style={{ color: stats.totalPnl >= 0 ? "#22c55e" : "#ef4444", fontVariantNumeric: "tabular-nums" }}>
                {stats.totalPnl >= 0 ? "+" : ""}${stats.totalPnl.toFixed(2)}
              </b>
            </div>
            </React.Fragment>
          )}
        </div>
      </div>
    </div>
  );
}
