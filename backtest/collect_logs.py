#!/usr/bin/env python3
"""多账户复盘日志汇总 + 盘口深度分析

用途
----
每个账户的 exe 会把复盘日志写到自己的 data/review/YYYY-MM-DD_<acct>.jsonl。
把各账户的 review 目录拷到同一个父目录下（子目录名随意），然后：

    python3 collect_logs.py --root ~/收集/20260919 --out out/depth

脚本做三件事：
1. 汇总去重：按 (acct, ts, city, event) 去重，防止重复拷贝导致重复计数。
2. 深度体检：统计各买入价带在决策时点真实能吃进多少钱，回答
   "闸门放行的档位到底有没有深度" 这个回测一直无法验证的问题。
3. 成交归因：按 strategy_label（LLM / Gate）分别统计胜率与收益，
   用于验证"LLM 降级"这个改动在实盘上是否成立。

设计说明
--------
只读输入，所有产出写到 --out 目录，不修改原始日志。
"""
import argparse
import collections
import csv
import hashlib
import json
import os

# 与线上闸门一致的买入价分带；最后一档是明确亏损带，单独列出便于确认没有误开
BANDS = [(0.90, 0.92), (0.92, 0.95), (0.95, 0.98), (0.98, 0.99), (0.99, 0.995), (0.995, 1.0)]


def iter_entries(root):
    """遍历 root 下所有 *.jsonl，产出 (path, entry)。坏行跳过并计数。"""
    bad = 0
    for dirpath, _, files in os.walk(root):
        for fn in sorted(files):
            if not fn.endswith(".jsonl"):
                continue
            path = os.path.join(dirpath, fn)
            with open(path, encoding="utf-8", errors="replace") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        yield path, json.loads(line)
                    except json.JSONDecodeError:
                        bad += 1
    if bad:
        print(f"  （跳过 {bad} 行无法解析的内容）")


def acct_of(entry, path):
    """账户标识：优先取行内 acct 字段，否则从文件名 YYYY-MM-DD_<acct>.jsonl 推断。"""
    a = entry.get("acct")
    if a:
        return str(a)
    base = os.path.basename(path)
    stem = base[:-6] if base.endswith(".jsonl") else base
    return stem.split("_", 1)[1] if "_" in stem else "unknown"


def load(root):
    """汇总所有日志并去重。

    去重键用「账户 + 整行内容的哈希」而不是 (账户, 时间戳, 城市, 事件)：
    同一毫秒内完全可能出现两条针对不同档位的 open_execute，用元数据做键
    会把其中一条静默丢掉。按内容哈希只会剔除真正逐字重复的行，正好对应
    "同一份日志被拷贝了两次" 这个实际场景。
    """
    seen = set()
    rows = []
    dupes = 0
    for path, e in iter_entries(root):
        acct = acct_of(e, path)
        key = (acct, hashlib.sha1(
            json.dumps(e, sort_keys=True, ensure_ascii=False).encode("utf-8")
        ).hexdigest())
        if key in seen:
            dupes += 1
            continue
        seen.add(key)
        e["_acct"] = acct
        rows.append(e)
    if dupes:
        print(f"  （剔除 {dupes} 条重复行）")
    return rows


def analyze_depth(rows, out_dir):
    """各价格带的真实可成交金额分布。

    对每个 city_snapshot 的每个档位，按 NO 侧卖盘累加 price*size，
    落入对应买入价带。这样得到的是"决策时点该价位能买进多少美元"。
    """
    band_usd = collections.Counter()
    band_hits = collections.Counter()
    band_cities = collections.defaultdict(set)
    per_level = []
    snaps = 0
    with_depth = 0

    for e in rows:
        if e.get("event") != "city_snapshot":
            continue
        snaps += 1
        d = e.get("data") or {}
        city = e.get("city")
        for t in d.get("thresholds") or []:
            asks = t.get("depth_asks")
            if not asks:
                continue
            with_depth += 1
            for lvl in asks:
                p, sz = lvl.get("price"), lvl.get("size")
                if p is None or sz is None:
                    continue
                usd = p * sz
                for lo, hi in BANDS:
                    if lo <= p < hi:
                        band_usd[(lo, hi)] += usd
                        band_hits[(lo, hi)] += 1
                        band_cities[(lo, hi)].add(city)
                        break
            per_level.append(dict(
                acct=e["_acct"], ts=e.get("ts"), city=city, label=t.get("label"),
                ask=t.get("ask"), bid=t.get("bid"),
                # 能立刻吃进的金额，按线上区间 0.92–0.99 统计
                fillable_usd=round(sum(l["price"] * l["size"] for l in asks
                                       if l.get("price") is not None and l.get("size") is not None
                                       and 0.92 <= l["price"] <= 0.99), 2),
                best_ask=asks[0].get("price") if asks else None,
                best_ask_size=asks[0].get("size") if asks else None,
            ))

    print(f"\n=== 盘口深度 ===")
    print(f"城市快照 {snaps} 条，其中带深度数据的档位 {with_depth} 个")
    if not with_depth:
        print("  没有深度数据。请确认跑的是 v2.2.0 之后的版本。")
        return
    print(f"\n{'买入价带':>14s}{'累计可成交$':>14s}{'挂单档数':>10s}{'涉及城市':>10s}{'单档均值$':>12s}")
    for lo, hi in BANDS:
        n = band_hits[(lo, hi)]
        print(f"{lo:.3f}–{hi:.3f}{band_usd[(lo,hi)]:>14,.0f}{n:>10d}"
              f"{len(band_cities[(lo,hi)]):>10d}{(band_usd[(lo,hi)]/n if n else 0):>12,.1f}")
    live = sum(band_usd[b] for b in BANDS if 0.92 <= b[0] < 0.99)
    print(f"\n线上区间 0.92–0.99 累计可成交 ${live:,.0f}")

    path = os.path.join(out_dir, "depth_levels.csv")
    with open(path, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(per_level[0].keys()))
        w.writeheader()
        w.writerows(per_level)
    print(f"逐档明细已写出：{path}（{len(per_level)} 行）")


def analyze_gate(rows):
    """闸门与开仓结果统计，按账户和来源（LLM / Gate）分列。"""
    gate = collections.Counter()
    reasons = collections.Counter()
    exec_res = collections.Counter()
    by_origin = collections.Counter()
    for e in rows:
        ev, d = e.get("event"), e.get("data") or {}
        if ev == "safety_gate_pass":
            gate[(e["_acct"], "pass")] += 1
            by_origin[(d.get("origin") or "?", "gate_pass")] += 1
        elif ev == "open_execute":
            r = d.get("result") or "?"
            exec_res[(e["_acct"], r)] += 1
            if r == "safety_gate_fail":
                reasons[str(d.get("reason"))[:70]] += 1
            if r in ("success", "safety_gate_fail", "out_of_range"):
                by_origin[(d.get("origin") or "?", r)] += 1

    print(f"\n=== 闸门与开仓（按账户）===")
    accts = sorted({k[0] for k in list(gate) + list(exec_res)})
    for a in accts:
        succ = exec_res[(a, "success")]
        gfail = exec_res[(a, "safety_gate_fail")]
        oor = exec_res[(a, "out_of_range")]
        print(f"  账户 {a}: 闸门放行 {gate[(a,'pass')]} | 成交 {succ} | 闸门拦截 {gfail} | 价格区间外 {oor}")

    if by_origin:
        print(f"\n=== 按候选来源 ===")
        for o in sorted({k[0] for k in by_origin}):
            print(f"  {o:5s}: 闸门放行 {by_origin[(o,'gate_pass')]} | 成交 {by_origin[(o,'success')]} | "
                  f"闸门拦截 {by_origin[(o,'safety_gate_fail')]} | 区间外 {by_origin[(o,'out_of_range')]}")

    if reasons:
        print(f"\n=== 闸门拦截原因 Top 10 ===")
        for r, n in reasons.most_common(10):
            print(f"  {n:5d}  {r}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True, help="包含各账户 review 日志的父目录")
    ap.add_argument("--out", default="out/depth", help="分析产出目录")
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    print(f"扫描 {args.root} ...")
    rows = load(args.root)
    accts = collections.Counter(r["_acct"] for r in rows)
    events = collections.Counter(r.get("event") for r in rows)
    print(f"去重后 {len(rows)} 条日志，来自 {len(accts)} 个账户")
    for a, n in accts.most_common():
        print(f"  账户 {a}: {n} 条")
    print(f"事件分布: {dict(events.most_common())}")

    analyze_depth(rows, args.out)
    analyze_gate(rows)


if __name__ == "__main__":
    main()
