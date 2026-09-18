import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import type { CityRow } from "./CityCard";

interface Props {
  open: boolean;
  onClose: () => void;
  cities: CityRow[];
  /** null = 全选；空集合 = 全不选 */
  selectedSlugs: Set<string> | null;
  onToggle: (slug: string) => void;
  onSelectAll: () => void;
  onSelectNone: () => void;
  /** station_code 修改成功后回调，通知父组件刷新 */
  onStationCodeUpdated?: (slug: string, newCode: string | null) => void;
  /** 从 Polymarket 同步城市列表（原 Sync Cities 功能），由父组件传入 */
  onSyncFromPolymarket?: () => Promise<void>;
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

export default function CitySelectorModal({
  open,
  onClose,
  cities,
  selectedSlugs,
  onToggle,
  onSelectAll,
  onSelectNone,
  onStationCodeUpdated,
  onSyncFromPolymarket,
}: Props) {
  const [search, setSearch] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const [allChecked, setAllChecked] = useState(false);
  const [editingSlug, setEditingSlug] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [savingSlug, setSavingSlug] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);

  const filtered = useMemo(() => {
    if (!search.trim()) return cities;
    const q = search.toLowerCase();
    return cities.filter(
      (c) =>
        c.slug.toLowerCase().includes(q) ||
        (c.city_name ?? "").toLowerCase().includes(q)
    );
  }, [cities, search]);

  // 统计全选状态
  useEffect(() => {
    if (cities.length === 0) {
      setAllChecked(false);
      return;
    }
    // null = 全选
    if (selectedSlugs === null) {
      setAllChecked(true);
      return;
    }
    const selectedCount = selectedSlugs === null
      ? cities.length
      : cities.filter((c) => selectedSlugs.has(c.slug)).length;
    setAllChecked(selectedCount === cities.length);
  }, [cities, selectedSlugs]);

  const handleAllToggle = useCallback(() => {
    if (allChecked) {
      onSelectNone();
    } else {
      onSelectAll();
    }
  }, [allChecked, onSelectAll, onSelectNone]);

  // 保存气象站编号
  const handleSaveStationCode = useCallback(
    async (slug: string) => {
      const trimmed = editValue.trim();
      const code = trimmed.length > 0 ? trimmed : null;
      setSavingSlug(slug);
      try {
        await invoke("update_station_code", {
          slug,
          stationCode: code,
        });
        onStationCodeUpdated?.(slug, code);
        setEditingSlug(null);
      } catch (e) {
        console.error("[station_code] update failed:", e);
      } finally {
        setSavingSlug(null);
      }
    },
    [editValue, onStationCodeUpdated]
  );

  // 从 Polymarket 同步城市列表
  const handleSync = useCallback(async () => {
    if (!onSyncFromPolymarket || syncing) return;
    setSyncing(true);
    try {
      await onSyncFromPolymarket();
    } catch (e) {
      console.error("[city sync] failed:", e);
    } finally {
      setSyncing(false);
    }
  }, [onSyncFromPolymarket, syncing]);

  // 打开时聚焦搜索框
  useEffect(() => {
    if (open) {
      setSearch("");
      setTimeout(() => searchRef.current?.focus(), 50);
    }
  }, [open]);

  // ESC 关闭
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const selectedCount = selectedSlugs === null
    ? cities.length
    : cities.filter((c) => selectedSlugs.has(c.slug)).length;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 1000,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: "rgba(0,0,0,0.7)",
        backdropFilter: "blur(4px)",
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        style={{
          width: 520,
          maxHeight: "80vh",
          display: "flex",
          flexDirection: "column",
          background: "#0f172a",
          border: "1px solid #334155",
          borderRadius: "12px",
          padding: "24px",
          boxShadow: "0 25px 50px rgba(0,0,0,0.5)",
        }}
      >
        {/* 标题 */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            marginBottom: "16px",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
            <h2
              style={{
                margin: 0,
                fontSize: "18px",
                fontWeight: 700,
                color: "#e2e8f0",
              }}
            >
              City Filter
            </h2>
            <span style={{ fontSize: "12px", color: "#64748b" }}>
              {selectedCount}/{cities.length} selected
            </span>
          </div>
          <button
            onClick={onClose}
            style={{
              background: "none",
              border: "none",
              color: "#94a3b8",
              fontSize: "20px",
              cursor: "pointer",
              padding: "4px 8px",
              borderRadius: "4px",
            }}
            onMouseEnter={(e) => (e.currentTarget.style.color = "#e2e8f0")}
            onMouseLeave={(e) => (e.currentTarget.style.color = "#94a3b8")}
          >
            &times;
          </button>
        </div>

        {/* 搜索框 + 全选 */}
        <div style={{ display: "flex", gap: "8px", marginBottom: "12px" }}>
          <input
            ref={searchRef}
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search city..."
            style={{
              flex: 1,
              padding: "8px 12px",
              fontSize: "13px",
              borderRadius: "6px",
              border: "1px solid #334155",
              background: "#1e293b",
              color: "#e2e8f0",
              outline: "none",
            }}
          />
          <button
            onClick={handleAllToggle}
            style={{
              padding: "8px 14px",
              fontSize: "12px",
              fontWeight: 600,
              borderRadius: "6px",
              border: "1px solid #334155",
              background: allChecked ? "#1e293b" : "#0f172a",
              color: allChecked ? "#34d399" : "#94a3b8",
              cursor: "pointer",
              whiteSpace: "nowrap",
            }}
          >
            {allChecked ? "All Selected" : "Select All"}
          </button>
        </div>

        {/* 城市网格列表 */}
        <div
          style={{
            flex: 1,
            overflowY: "auto",
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            gap: "4px 8px",
            alignContent: "start",
            paddingRight: "4px",
          }}
        >
          {filtered.map((c) => {
            const checked = selectedSlugs === null || selectedSlugs.has(c.slug);
            const name = c.city_name || cityDisplayName(c.slug);
            const initial = name.charAt(0).toUpperCase();
            const bgColor = avatarColor(c.slug);
            const avatarUrl = c.avatar;
            const isEditing = editingSlug === c.slug;
            const isSaving = savingSlug === c.slug;

            return (
              <div
                key={c.slug}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: "8px",
                  padding: "6px 8px",
                  borderRadius: "6px",
                  cursor: "pointer",
                  background: checked ? "rgba(16,185,129,0.08)" : "transparent",
                  border: checked ? "1px solid rgba(16,185,129,0.2)" : "1px solid transparent",
                  transition: "background 0.15s, border 0.15s",
                  flexWrap: "wrap",
                }}
                onMouseEnter={(e) => {
                  if (!checked)
                    e.currentTarget.style.background = "rgba(255,255,255,0.04)";
                }}
                onMouseLeave={(e) => {
                  if (!checked)
                    e.currentTarget.style.background = "transparent";
                }}
                onClick={() => {
                  if (!isEditing) onToggle(c.slug);
                }}
              >
                {/* 复选框 */}
                <div
                  style={{
                    width: "16px",
                    height: "16px",
                    borderRadius: "3px",
                    border: checked
                      ? "1px solid #10b981"
                      : "1px solid #475569",
                    background: checked ? "#10b981" : "transparent",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    flexShrink: 0,
                    transition: "all 0.15s",
                  }}
                >
                  {checked && (
                    <svg
                      width="10"
                      height="10"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="#0f172a"
                      strokeWidth="3"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    >
                      <polyline points="20 6 9 17 4 12" />
                    </svg>
                  )}
                </div>
                {/* 头像 */}
                {avatarUrl ? (
                  <img
                    src={avatarUrl}
                    alt={name}
                    loading="lazy"
                    onError={(e) => {
                      const t = e.currentTarget;
                      t.style.display = "none";
                      const fb = t.nextElementSibling as HTMLElement | null;
                      if (fb) fb.style.display = "flex";
                    }}
                    style={{
                      width: "22px",
                      height: "22px",
                      borderRadius: "50%",
                      objectFit: "cover",
                      flexShrink: 0,
                    }}
                  />
                ) : null}
                <div
                  style={{
                    width: "22px",
                    height: "22px",
                    borderRadius: "50%",
                    background: bgColor,
                    display: avatarUrl ? "none" : "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontSize: "10px",
                    fontWeight: 700,
                    color: "#fff",
                    flexShrink: 0,
                  }}
                >
                  {initial}
                </div>
                {/* 城市名 */}
                <span
                  style={{
                    fontSize: "12px",
                    color: checked ? "#e2e8f0" : "#94a3b8",
                    fontWeight: checked ? 600 : 400,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {name}
                </span>
                {/* 气象站编号 */}
                {isEditing ? (
                  <div
                    style={{ display: "flex", alignItems: "center", gap: "4px", marginLeft: "auto" }}
                    onClick={(e) => e.stopPropagation()}
                  >
                    <input
                      type="text"
                      value={editValue}
                      onChange={(e) => setEditValue(e.target.value.toUpperCase())}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          handleSaveStationCode(c.slug);
                        } else if (e.key === "Escape") {
                          setEditingSlug(null);
                        }
                      }}
                      autoFocus
                      disabled={isSaving}
                      placeholder="ICAO"
                      style={{
                        width: "70px",
                        padding: "2px 6px",
                        fontSize: "11px",
                        borderRadius: "4px",
                        border: "1px solid #3b82f6",
                        background: "#1e293b",
                        color: "#e2e8f0",
                        outline: "none",
                        fontFamily: "monospace",
                      }}
                    />
                    <button
                      onClick={() => handleSaveStationCode(c.slug)}
                      disabled={isSaving}
                      style={{
                        padding: "2px 6px",
                        fontSize: "11px",
                        borderRadius: "4px",
                        border: "none",
                        background: isSaving ? "#1e293b" : "#3b82f6",
                        color: "#fff",
                        cursor: isSaving ? "wait" : "pointer",
                        fontWeight: 600,
                      }}
                    >
                      {isSaving ? "..." : "OK"}
                    </button>
                    <button
                      onClick={() => setEditingSlug(null)}
                      disabled={isSaving}
                      style={{
                        padding: "2px 6px",
                        fontSize: "11px",
                        borderRadius: "4px",
                        border: "1px solid #334155",
                        background: "transparent",
                        color: "#94a3b8",
                        cursor: "pointer",
                      }}
                    >
                      &times;
                    </button>
                  </div>
                ) : (
                  <span
                    onClick={(e) => {
                      e.stopPropagation();
                      setEditingSlug(c.slug);
                      setEditValue(c.station_code ?? "");
                    }}
                    title="点击编辑观测站编号"
                    style={{
                      marginLeft: "auto",
                      fontSize: "10px",
                      fontFamily: "monospace",
                      padding: "1px 6px",
                      borderRadius: "3px",
                      background: c.station_code ? "rgba(59,130,246,0.15)" : "transparent",
                      color: c.station_code ? "#7dd3fc" : "#475569",
                      border: c.station_code
                        ? "1px solid rgba(59,130,246,0.2)"
                        : "1px dashed #334155",
                      cursor: "text",
                      whiteSpace: "nowrap",
                      transition: "all 0.15s",
                    }}
                  >
                    {c.station_code || "--"}
                  </span>
                )}
                {/* 数据采集站点链接 */}
                {c.station_url ? (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      openUrl(c.station_url!);
                    }}
                    title={`数据采集站点: ${c.station_url}`}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      width: "20px",
                      height: "20px",
                      padding: 0,
                      marginLeft: "4px",
                      flexShrink: 0,
                      background: "transparent",
                      border: "1px solid #334155",
                      borderRadius: "4px",
                      color: "#64748b",
                      cursor: "pointer",
                      transition: "all 0.15s",
                    }}
                  >
                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
                      <polyline points="15 3 21 3 21 9" />
                      <line x1="10" y1="14" x2="21" y2="3" />
                    </svg>
                  </button>
                ) : null}
              </div>
            );
          })}
          {filtered.length === 0 && (
            <div
              style={{
                gridColumn: "1 / -1",
                padding: "40px",
                textAlign: "center",
                color: "#64748b",
                fontSize: "13px",
              }}
            >
              No cities found.
            </div>
          )}
        </div>

        {/* 底栏 */}
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            marginTop: "16px",
            paddingTop: "12px",
            borderTop: "1px solid #1e293b",
          }}
        >
          {onSyncFromPolymarket ? (
            <button
              onClick={handleSync}
              disabled={syncing}
              title="从 Polymarket 更新城市列表与气象站信息"
              style={{
                padding: "6px 12px",
                fontSize: "12px",
                borderRadius: "6px",
                border: "1px solid #334155",
                background: syncing ? "#1e293b" : "transparent",
                color: syncing ? "#64748b" : "#94a3b8",
                cursor: syncing ? "wait" : "pointer",
                display: "flex",
                alignItems: "center",
                gap: "6px",
              }}
            >
              {syncing && (
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" style={{ animation: "spin 1s linear infinite" }}>
                  <path d="M21 12a9 9 0 1 1-6.219-8.56" />
                </svg>
              )}
              {syncing ? "Syncing..." : "Sync from Polymarket"}
            </button>
          ) : (
            <span style={{ fontSize: "11px", color: "#475569" }}>
              Selections are saved automatically
            </span>
          )}
          <button
            onClick={onClose}
            style={{
              padding: "8px 20px",
              fontSize: "13px",
              borderRadius: "6px",
              border: "none",
              background: "linear-gradient(90deg,#7c3aed,#ec4899)",
              color: "#fff",
              cursor: "pointer",
              fontWeight: 600,
            }}
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}