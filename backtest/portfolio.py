#!/usr/bin/env python3
"""
资金层回测：给定 simulate.py 产出的交易流（每笔 $100 名义），按"每笔 = 总资产 × f"复利下单，
受现金约束（没钱就跳过），输出终值、最大回撤、跳单数、最大并发仓位。

用法：
    python3 backtest/portfolio.py --trades backtest/out/trades_llm_AND_h16_p1_m0.5.csv --capital 4500
"""
import argparse
import csv
import random


def run(trades, capital, frac, min_order=5.0, flip_extra_loss_rate=0.0, seed=0):
    rnd = random.Random(seed)
    rows = sorted(trades, key=lambda r: r["entry_ts"])
    cash = capital
    open_pos = []  # (exit_ts, cost, pnl)
    equity_curve = []
    skipped = 0
    max_conc = 0
    peak = capital
    max_dd = 0.0
    n_open = 0
    n_loss = 0

    def release(now):
        nonlocal cash
        keep = []
        for ex, cost, pnl in open_pos:
            if ex <= now:
                cash += cost + pnl
            else:
                keep.append((ex, cost, pnl))
        open_pos[:] = keep

    for r in rows:
        release(r["entry_ts"])
        equity = cash + sum(c for _, c, _ in open_pos)
        peak = max(peak, equity)
        max_dd = max(max_dd, (peak - equity) / peak)
        equity_curve.append(equity)
        stake = equity * frac
        if stake > cash:
            if cash >= min_order:
                stake = cash
            else:
                skipped += 1
                continue
        if stake < min_order:
            skipped += 1
            continue
        pnl100 = r["pnl"]
        if flip_extra_loss_rate > 0 and pnl100 > 0 and rnd.random() < flip_extra_loss_rate:
            pnl100 = -100.0
        pnl = pnl100 * stake / 100.0
        if pnl < 0:
            n_loss += 1
        cash -= stake
        open_pos.append((r["exit_ts"], stake, pnl))
        n_open += 1
        max_conc = max(max_conc, len(open_pos))
    release(10 ** 12)
    final = cash
    peak = max(peak, final)
    return dict(final=final, ret=(final / capital - 1) * 100, max_dd=max_dd * 100, opened=n_open,
                skipped=skipped, max_conc=max_conc, losses=n_loss)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trades", required=True)
    ap.add_argument("--capital", type=float, default=4500)
    ap.add_argument("--fracs", default="0.02,0.03,0.04,0.05,0.07,0.10,0.15,0.20")
    ap.add_argument("--stress", type=float, default=0.01, help="压力测试：额外把该比例的盈利单翻成 -100%%")
    ap.add_argument("--seeds", type=int, default=200)
    args = ap.parse_args()
    trades = []
    with open(args.trades) as f:
        for r in csv.DictReader(f):
            trades.append(dict(entry_ts=int(r["entry_ts"]), exit_ts=int(float(r["exit_ts"])), pnl=float(r["pnl"])))
    days = (max(t["exit_ts"] for t in trades) - min(t["entry_ts"] for t in trades)) / 86400
    print(f"trades={len(trades)} span={days:.0f}d capital={args.capital:.0f}  file={args.trades}")
    print(f"{'f':>5s} | {'final':>8s} {'ret%':>7s} {'maxDD%':>7s} {'opened':>6s} {'skip':>5s} {'maxConc':>7s} | "
          f"stress(+{args.stress*100:.1f}% loss rate, {args.seeds} runs)")
    for fs in args.fracs.split(","):
        f = float(fs)
        base = run(trades, args.capital, f)
        st = [run(trades, args.capital, f, flip_extra_loss_rate=args.stress, seed=s) for s in range(args.seeds)]
        rets = sorted(x["ret"] for x in st)
        st_med = rets[len(rets) // 2]
        st_p10 = rets[len(rets) // 10]
        st_dd = sum(x["max_dd"] for x in st) / len(st)
        print(f"{f*100:4.0f}% | {base['final']:8.0f} {base['ret']:7.1f} {base['max_dd']:7.1f} {base['opened']:6d} "
              f"{base['skipped']:5d} {base['max_conc']:7d} | median {st_med:7.1f}  p10 {st_p10:7.1f}  maxDD {st_dd:5.1f}")


if __name__ == "__main__":
    main()
