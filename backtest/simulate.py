#!/usr/bin/env python3
"""
气温市场 NO 策略回测

按小时复现程序的决策流程：
  当地 h:00 → 观测（ST 高/实时，滞后 1 小时，与线上一致）→ MET 剩余时段预报峰值
  → 各档位 NO 盘口（价格历史 mid ± 0.005）→ 开仓闸门 → 持仓跟踪（TP 0.999 / SL / 结算）

闸门变体：
  old_llm     : 复现旧 prompt 的 OFFSET 规则（相对市场 YES 峰值档的偏移量，<=13 点需 >=3 档，>13 点需 >=2 档，
                峰值已过且降温需 >=1 档）
  gate_N      : 新安全闸，峰值未到时要求高出 max(观测高, MET 剩余峰值, 市场峰值档上限) >= N 档；
                峰值已过要求高出观测高 1 档且高于 MET 剩余峰值
  gate_N_post : 同上但峰值未到一律不开

同时评估反向思路：温度回落后买"当前最高温所在档位"的 YES。

用法：
    python3 backtest/simulate.py [--sl 0.95] [--ask-min 0.95] [--ask-max 0.995] [--notional 100]
"""
import argparse
import bisect
import collections
import csv
import datetime as dt
import json
import os
import re
import sys
from zoneinfo import ZoneInfo

HERE = os.path.dirname(os.path.abspath(__file__))
CACHE = os.path.join(HERE, "cache")
OUT = os.path.join(HERE, "out")

HALF_SPREAD = 0.005
TP_PRICE = 0.999
DECISION_HOURS = list(range(11, 18))  # 与 TIME>=11、每小时一轮一致
OBS_LAG_HOURS = 1                     # ST 源相对于决策时刻的滞后（线上观测到约 1 小时）


# ── 基础工具 ──

def lower_of(label: str):
    if re.search(r"or below", label, re.I):
        return None
    m = re.search(r"-?\d+(?:\.\d+)?", label)
    return float(m.group(0)) if m else None


def step_of(unit: str) -> int:
    return 2 if "F" in unit else 1


def f_to_unit(tf: float, unit: str) -> float:
    return tf if "F" in unit else (tf - 32.0) * 5.0 / 9.0


class PriceSeries:
    def __init__(self, hist):
        self.t = [h[0] for h in hist]
        self.p = [h[1] for h in hist]

    def at(self, ts: int):
        i = bisect.bisect_right(self.t, ts) - 1
        return self.p[i] if i >= 0 else None

    def after(self, ts: int):
        i = bisect.bisect_right(self.t, ts)
        return zip(self.t[i:], self.p[i:])


def load_json(*parts):
    p = os.path.join(CACHE, *parts)
    if not os.path.exists(p):
        return None
    with open(p) as f:
        return json.load(f)


# ── 闸门 ──

def gate_new(lower, step, local_hour, st_high, st_cur, met_peak, mk_lower, pre_steps, post_only, yhh=None,
             post_hour=15, post_steps=1, post_met_margin=0.0, use_yhh=False):
    """移植自 App.tsx checkOpenSafetyGate；返回 (pass, phase)。"""
    if lower is None or st_high is None:
        return False, "na"
    obs = max(v for v in (st_high, st_cur) if v is not None)
    ref = max([obs] + [v for v in (met_peak, (mk_lower + step) if mk_lower is not None else None) if v is not None])
    peak_hour = yhh if yhh is not None else 16
    afternoon_flat = local_hour >= post_hour and st_cur is not None and st_cur <= st_high
    well_past = use_yhh and local_hour > peak_hour + 1
    post = afternoon_flat or well_past
    if post:
        if isinstance(post_steps, dict):
            post_steps = post_steps.get(local_hour, post_steps.get("default", 1))
        if lower < obs + post_steps * step:
            return False, "post"
        if met_peak is not None and lower < met_peak + post_met_margin * step + 1e-9:
            return False, "post"
        if met_peak is not None and post_met_margin == 0 and lower <= met_peak:
            return False, "post"
        return True, "post"
    if post_only:
        return False, "pre"
    return lower >= ref + pre_steps * step, "pre"


def gate_old_llm(lower, step, local_hour, st_high, st_cur, mk_lower):
    if lower is None or mk_lower is None:
        return False
    offset = (lower - mk_lower) / step
    if local_hour <= 13 and offset >= 3:
        return True
    if local_hour > 13 and offset >= 2:
        return True
    if local_hour > 16 and st_high is not None and st_cur is not None and st_cur < st_high and offset >= 1:
        return True
    return False


# ── 持仓结算 ──

def settle(series: PriceSeries, entry_ts: int, entry_ask: float, yes_won: bool, sl: float):
    """返回 (exit_price, reason)。价格历史为 mid，bid = mid - 半价差。"""
    last_t = entry_ts
    for t, p in series.after(entry_ts):
        last_t = t
        bid = p - HALF_SPREAD
        if sl > 0 and bid <= sl:
            return max(bid, 0.0), "SL", t
        if bid >= TP_PRICE:
            return TP_PRICE, "TP", t
    # 结算：按当天市场最后一个价格点后 12 小时释放资金（Polymarket 次日结算）
    return (0.0 if yes_won else 1.0), "RESOLVE", last_t + 12 * 3600


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sl", type=float, default=0.95)
    ap.add_argument("--ask-min", type=float, default=0.95)
    ap.add_argument("--ask-max", type=float, default=0.995)
    ap.add_argument("--notional", type=float, default=100.0)
    ap.add_argument("--detail", default="h16_p1_m1")
    ap.add_argument("--date-from", default="")
    ap.add_argument("--date-to", default="")
    args = ap.parse_args()
    os.makedirs(OUT, exist_ok=True)

    index = load_json("index.json")
    cities = {c["slug"]: c for c in index["cities"]}
    start = dt.date.fromisoformat(index["start"])
    end = dt.date.fromisoformat(index["end"])
    days = [start + dt.timedelta(days=i) for i in range((end - start).days + 1)]
    sim_days = [d for d in days if (not args.date_from or d >= dt.date.fromisoformat(args.date_from))
                and (not args.date_to or d <= dt.date.fromisoformat(args.date_to))]

    # 预加载观测与预报
    obs_by_city, fcst_by_city = {}, {}
    for slug, c in cities.items():
        o = load_json("obs", f"{c['station']}_{start}_{end}.json") or []
        obs_by_city[slug] = [(dt.datetime.fromisoformat(t), f_to_unit(v, c["unit"])) for t, v in o]
        f = load_json("fcst", f"{c['lat']:.3f}_{c['lon']:.3f}_{start}_{end}.json") or []
        fcst_by_city[slug] = {t: v for t, v in f if v is not None}

    # (pre_steps, post_only, post_hour, post_steps, post_met_margin_steps)
    variants = {
        "old_llm": None,
        "live_h15_p1_m0": (3, False, 15, 1, 0),   # 当前线上版本
        "h16_p1_m0": (3, False, 16, 1, 0),
        "h16_p1_m0.5": (3, False, 16, 1, 0.5),
        "h16_p1_m1": (3, False, 16, 1, 1),
        "h16_p2_m0": (3, False, 16, 2, 0),
        "h17_p1_m0": (3, False, 17, 1, 0),
        "h17_p1_m0.5": (3, False, 17, 1, 0.5),
        "h17_p1_m1": (3, False, 17, 1, 1),
        "h16_p1_m1_pre4": (4, False, 16, 1, 1),
        "postonly_h16_p1_m1": (3, True, 16, 1, 1),
        "hybrid_h16p2_h17p1_m0.5": (3, False, 16, {16: 2, "default": 1}, 0.5),
        "LIVE_h16_m0.5_YHH": (3, False, 16, 1, 0.5, True),   # 线上实际行为：wellPastPeak 用昨日峰值小时
        "FIX_h16_m0.5_noYHH": (3, False, 16, 1, 0.5, False), # 修复：去掉 wellPastPeak 旁路
        # 滑动闸门：越早开仓要求越大的安全边际，用档数换时间窗
        "SLIDE_h14": (3, False, 14, {14: 3, 15: 2, "default": 1}, 0.5),
        "SLIDE_h13": (3, False, 13, {13: 4, 14: 3, 15: 2, "default": 1}, 0.5),
        "SLIDE_h12": (3, False, 12, {12: 5, 13: 4, 14: 3, 15: 2, "default": 1}, 0.5),
        "SLIDE_h11": (3, False, 11, {11: 6, 12: 5, 13: 4, 14: 3, 15: 2, "default": 1}, 0.5),
        "llm_AND_h16_p1_m0.5": None,   # 实际线上 = LLM 推荐 ∩ 闸门
        "llm_AND_LIVE_YHH": None,
        "llm_AND_FIX_noYHH": None,
        "llm_AND_SLIDE_h14": None,
        "llm_AND_SLIDE_h13": None,
        "llm_AND_SLIDE_h12": None,
        "llm_AND_SLIDE_h11": None,
    }
    trades = {k: [] for k in variants}
    candidates = []  # 所有进入价格区间的候选，含特征，用于分析
    yes_obs = []     # 反向 YES 评估
    stats = collections.Counter()

    for slug, c in cities.items():
        tz = ZoneInfo(c["tz"])
        unit, step = c["unit"], step_of(c["unit"])
        obs = obs_by_city[slug]
        fc = fcst_by_city[slug]
        for d in sim_days:
            ev = load_json("events", slug, f"{d.isoformat()}.json")
            if not ev or not ev["markets"]:
                stats["no_event"] += 1
                continue
            mk = [m for m in ev["markets"] if m["resolved"]]
            if not mk or not any(m["yes_won"] for m in mk):
                stats["unresolved"] += 1
                continue
            stats["city_days"] += 1
            series = {}
            for m in mk:
                h = load_json("prices", f"{m['no_token']}.json")
                if h:
                    series[m["no_token"]] = PriceSeries(h)
                m["lower"] = lower_of(m["label"])
            day_obs = [(t, v) for t, v in obs if t.date() == d]
            yd = d - dt.timedelta(days=1)
            yobs = [(t, v) for t, v in obs if t.date() == yd]
            yday_high_hour = max(yobs, key=lambda x: x[1])[0].hour if yobs else None
            fc_day = {int(t[11:13]): v for t, v in fc.items() if t[:10] == d.isoformat()}
            winner = next(m for m in mk if m["yes_won"])
            final_high_obs = max((v for _, v in day_obs), default=None)

            held = {k: set() for k in variants}
            for h in DECISION_HOURS:
                local_dt = dt.datetime(d.year, d.month, d.day, h, 0, tzinfo=tz)
                ts = int(local_dt.timestamp())
                cutoff = local_dt.replace(tzinfo=None) - dt.timedelta(hours=OBS_LAG_HOURS)
                seen = [(t, v) for t, v in day_obs if t <= cutoff]
                if not seen:
                    continue
                st_high = max(v for _, v in seen)
                st_cur = seen[-1][1]
                met_rem = [v for hh, v in fc_day.items() if 10 <= hh <= 17 and hh >= h]
                met_peak = max(met_rem) if met_rem else (max(fc_day.values()) if fc_day else None)

                # 盘口
                quotes = {}
                for m in mk:
                    s = series.get(m["no_token"])
                    p = s.at(ts) if s else None
                    if p is None:
                        continue
                    quotes[m["no_token"]] = (p - HALF_SPREAD, min(p + HALF_SPREAD, 0.999), p)
                if not quotes:
                    continue
                mk_lower = None
                min_mid = 9
                for m in mk:
                    q = quotes.get(m["no_token"])
                    if q and m["lower"] is not None and q[2] < min_mid:
                        min_mid, mk_lower = q[2], m["lower"]

                # 反向 YES 评估：温度回落时，当前最高温所在档位
                if st_cur < st_high and h >= 12:
                    v = round(st_high)
                    b = None
                    ordered = sorted([m for m in mk if m["lower"] is not None], key=lambda x: x["lower"])
                    for i, m in enumerate(ordered):
                        hi = ordered[i + 1]["lower"] if i + 1 < len(ordered) else 1e9
                        if m["lower"] <= v < hi:
                            b = m
                            break
                    if b is None:
                        b = next((m for m in mk if m["lower"] is None), None)
                    if b and b["no_token"] in quotes:
                        no_bid = quotes[b["no_token"]][0]
                        yes_obs.append(dict(city=slug, date=d.isoformat(), hour=h, label=b["label"],
                                            yes_ask=round(1 - no_bid, 3), won=b["yes_won"],
                                            st_high=round(st_high, 1), st_cur=round(st_cur, 1)))

                # NO 候选
                for m in mk:
                    q = quotes.get(m["no_token"])
                    if not q or m["lower"] is None:
                        continue
                    bid, ask, mid = q
                    if not (args.ask_min <= ask <= args.ask_max):
                        continue
                    feat = dict(city=slug, date=d.isoformat(), hour=h, label=m["label"], unit=unit,
                                lower=m["lower"], ask=round(ask, 3), st_high=round(st_high, 1), st_cur=round(st_cur, 1),
                                met_peak=met_peak, mk_lower=mk_lower, yes_won=m["yes_won"],
                                final_high_obs=round(final_high_obs, 1) if final_high_obs is not None else None,
                                winner=winner["label"])
                    candidates.append(feat)
                    decisions = {
                        "old_llm": gate_old_llm(m["lower"], step, h, st_high, st_cur, mk_lower),
                    }
                    for k, v in ((k, v) for k, v in variants.items() if v):
                        n, post_only, ph, ps, pm = v[:5]
                        uy = v[5] if len(v) > 5 else False
                        decisions[k] = gate_new(m["lower"], step, h, st_high, st_cur, met_peak, mk_lower, n, post_only,
                                                yhh=yday_high_hour, post_hour=ph, post_steps=ps, post_met_margin=pm,
                                                use_yhh=uy)[0]
                    decisions["llm_AND_h16_p1_m0.5"] = decisions["old_llm"] and decisions["h16_p1_m0.5"]
                    decisions["llm_AND_LIVE_YHH"] = decisions["old_llm"] and decisions["LIVE_h16_m0.5_YHH"]
                    decisions["llm_AND_FIX_noYHH"] = decisions["old_llm"] and decisions["FIX_h16_m0.5_noYHH"]
                    for _h in ("h14", "h13", "h12", "h11"):
                        decisions[f"llm_AND_SLIDE_{_h}"] = decisions["old_llm"] and decisions[f"SLIDE_{_h}"]
                    for k, ok in decisions.items():
                        if not ok or m["no_token"] in held[k]:
                            continue
                        held[k].add(m["no_token"])
                        size = args.notional / ask
                        exit_p, reason, exit_ts = settle(series[m["no_token"]], ts, ask, m["yes_won"], args.sl)
                        pnl = (exit_p - ask) * size
                        trades[k].append({**feat, "variant": k, "exit": round(exit_p, 3), "reason": reason,
                                          "pnl": round(pnl, 2), "entry_ts": ts, "exit_ts": exit_ts})

    # ── 输出 ──
    print(f"city-days: {stats['city_days']}  (no_event {stats['no_event']}, unresolved {stats['unresolved']})")
    print(f"candidates in ask range: {len(candidates)}  | params sl={args.sl} ask=[{args.ask_min},{args.ask_max}] notional={args.notional}\n")
    print(f"{'variant':20s} {'trades':>6s} {'wins':>5s} {'losses':>6s} {'win%':>6s} {'pnl':>9s} {'pnl/trade':>9s} {'avgwin':>7s} {'SL exits':>8s}")
    for k, tl in trades.items():
        if not tl:
            print(f"{k:20s} {0:6d}")
            continue
        wins = sum(1 for t in tl if t["pnl"] > 0)
        losses = sum(1 for t in tl if t["pnl"] <= 0)
        pnl = sum(t["pnl"] for t in tl)
        worst = min(t["pnl"] for t in tl)
        sl_n = sum(1 for t in tl if t["reason"] == "SL")
        avgwin = sum(t["pnl"] for t in tl if t["pnl"] > 0) / max(1, wins)
        print(f"{k:20s} {len(tl):6d} {wins:5d} {losses:6d} {100*wins/len(tl):6.1f} {pnl:9.2f} {pnl/len(tl):9.3f} {avgwin:7.2f} {sl_n:8d}")
        with open(os.path.join(OUT, f"trades_{k}.csv"), "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=list(tl[0].keys()))
            w.writeheader()
            w.writerows(tl)

    # 亏损明细（新闸门 gate_3）
    print(f"\n== {args.detail} losing trades ==")
    for t in trades[args.detail]:
        if t["yes_won"]:
            print(f"  {t['date']} {t['city']:14s} {t['label']:14s} h={t['hour']} ask={t['ask']} st_high={t['st_high']} "
                  f"met={t['met_peak']} mk={t['mk_lower']} final_obs={t['final_high_obs']} exit={t['exit']} {t['reason']} pnl={t['pnl']}")

    # 按开仓小时分布（gate_3）
    print(f"\n== {args.detail} by hour ==")
    by_h = collections.defaultdict(list)
    for t in trades[args.detail]:
        by_h[t["hour"]].append(t["pnl"])
    for h in sorted(by_h):
        v = by_h[h]
        print(f"  {h:02d}:00  n={len(v):4d}  losses={sum(1 for x in v if x <= 0):3d}  pnl={sum(v):8.2f}")

    # 候选池里"实际会输"的档位有多少被各闸门拦下
    print("\n== losing candidates (bucket eventually won) and whether gates opened them ==")
    losers = [c for c in candidates if c["yes_won"]]
    print(f"  losing candidates: {len(losers)} of {len(candidates)}")
    for k in variants:
        opened = sum(1 for t in trades[k] if t["yes_won"])
        print(f"  {k:18s} opened {opened} of them")

    # 反向 YES
    print("\n== YES on current-high bucket when temp falling (by price band) ==")
    with open(os.path.join(OUT, "yes_obs.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(yes_obs[0].keys()) if yes_obs else ["none"])
        w.writeheader()
        w.writerows(yes_obs)
    bands = [(0.0, 0.3), (0.3, 0.5), (0.5, 0.7), (0.7, 0.85), (0.85, 0.95), (0.95, 1.01)]
    for lo, hi in bands:
        sel = [y for y in yes_obs if lo <= y["yes_ask"] < hi]
        if not sel:
            continue
        wins = sum(1 for y in sel if y["won"])
        ev_ = sum((1 - y["yes_ask"]) if y["won"] else -y["yes_ask"] for y in sel) / len(sel)
        print(f"  ask [{lo:.2f},{hi:.2f}) n={len(sel):5d} win%={100*wins/len(sel):5.1f} EV/$1={ev_:+.3f}")
    print("\n== YES by hour (ask<0.9 only) ==")
    by_h = collections.defaultdict(list)
    for y in yes_obs:
        if y["yes_ask"] < 0.9:
            by_h[y["hour"]].append(y)
    for h in sorted(by_h):
        sel = by_h[h]
        wins = sum(1 for y in sel if y["won"])
        ev_ = sum((1 - y["yes_ask"]) if y["won"] else -y["yes_ask"] for y in sel) / len(sel)
        print(f"  {h:02d}:00 n={len(sel):4d} win%={100*wins/len(sel):5.1f} EV/$1={ev_:+.3f}")


if __name__ == "__main__":
    main()
