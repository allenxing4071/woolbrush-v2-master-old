import { invoke } from "@tauri-apps/api/core";

// ── 复盘日志类型 ──

export type ReviewEventType =
  | "round_start"
  | "city_snapshot"
  | "close_attempt"
  | "skip"
  | "open_preflight"
  | "open_llm_result"
  | "open_candidates"
  | "safety_gate_pass"
  | "open_execute"
  | "round_end"
  | "error";

export interface ReviewEntry {
  ts: string;
  round: number;
  city: string | null;
  event: ReviewEventType;
  data: Record<string, unknown>;
}

let roundCounter = 0;

export function nextRound(): number {
  return ++roundCounter;
}

export function getRound(): number {
  return roundCounter;
}

function ts(): string {
  return new Date().toISOString();
}

/**
 * 写入一条复盘日志（JSONL 格式，按日期分文件）
 *
 * 文件: ../data/review/YYYY-MM-DD.jsonl（src-tauri 外部，避免触发 dev watcher 重启）
 *
 * 写入失败不会抛异常，只在控制台输出警告，不影响主流程
 */
export async function rlog(
  city: string | null,
  event: ReviewEventType,
  data: Record<string, unknown>,
): Promise<void> {
  const entry: ReviewEntry = {
    ts: ts(),
    round: roundCounter,
    city,
    event,
    data,
  };
  try {
    await invoke("append_review_log", { line: JSON.stringify(entry) });
  } catch (e) {
    console.warn("[rlog] write failed:", e);
  }
}
