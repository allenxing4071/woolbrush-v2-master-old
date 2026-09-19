#!/usr/bin/env python3
"""
Polymarket 盘口深度采集器（公开数据，不需要账户/私钥）

为什么需要它：历史盘口快照是唯一无法事后补抓的数据。价格历史（prices-history）
只有成交价序列，没有挂单量，所以任何「深度够不够成交」的回测结论都只能是上限。
这个脚本按固定节奏把全量盘口存成 JSONL，攒够之后 simulate.py 才能加上真实深度约束。

采集范围（默认不做任何价格区间过滤）：
  - 51 个城市 × {最高温, 最低温} 两类市场 × 全部 11 个档位
  - 每个档位的 NO 与 YES 两侧订单簿，各最多 60 档（价+量）
  不过滤是刻意的：0.92–0.99 只是当前策略的区间，全量数据才能事后回答
  「换到别的价格区间是否收益更高」。

输出：data/depth/depth-YYYY-MM-DD.jsonl（按 UTC 日切换，跨日后自动 gzip 旧文件）
每行一条记录，schema：
  ts     int    采样时刻 Unix 秒（UTC）
  kind   str    "highest" | "lowest"
  city   str    城市 slug
  mdate  str    该市场对应的城市本地日期 YYYY-MM-DD
  lh     float  采样时该城市的本地小时（含小数，便于按时段切片）
  label  str    档位标签，如 "78-79°F"
  tick   float  最小价格变动
  minsz  float  交易所最小下单量
  no_tok/yes_tok  str    token id
  no_a/yes_a      list   asks，[[价, 量], ...]，价格升序（最优价在前）
  no_b/yes_b      list   bids，[[价, 量], ...]，价格降序（最优价在前）
  no_lt/yes_lt    float  最近成交价（可能为 null）

重要：这是账户无关的公开行情，只需在一台机器上跑一个实例。不要在 100 个账户
的云电脑上各跑一份——那只会得到 100 份完全相同的数据。

用法：
    # 先单跑一轮验证
    python3 backtest/collect_depth.py --once

    # 长期后台采集（5 分钟一轮）
    nohup python3 backtest/collect_depth.py --interval 300 \
        >> backtest/depth_collector.log 2>&1 &
"""
from __future__ import annotations   # 兼容 macOS 自带 Python 3.9（PEP 604 的 X | None 语法需 3.10+）

import argparse
import concurrent.futures as cf
import datetime as dt
import gzip
import json
import os
import shutil
import signal
import sqlite3
import sys
import time
import urllib.error
import urllib.request
from zoneinfo import ZoneInfo

HERE = os.path.dirname(os.path.abspath(__file__))
PROJ = os.path.dirname(HERE)
CACHE = os.path.join(HERE, "cache")
GAMMA = "https://gamma-api.polymarket.com"
CLOB = "https://clob.polymarket.com"
UA = "woolbrush-depth/1.0"

MONTHS = ["january", "february", "march", "april", "may", "june", "july",
          "august", "september", "october", "november", "december"]

MAX_LEVELS = 60      # 每侧最多保留档数，防止极端盘口把文件撑爆
BOOK_CHUNK = 100     # /books 单次批量请求的 token 数
EVENT_RETRY_SEC = 1800   # 事件未找到时的重试间隔（新市场可能稍后才挂出）

_stop = False


def _on_signal(signum, _frame):
    global _stop
    _stop = True
    log(f"收到信号 {signum}，本轮结束后退出")


def log(msg: str) -> None:
    print(f"[{dt.datetime.now(dt.timezone.utc):%Y-%m-%d %H:%M:%S}Z] {msg}",
          file=sys.stderr, flush=True)


def http_json(url: str, data: bytes | None = None, retries: int = 4, timeout: int = 40):
    """GET/POST JSON。必须带 User-Agent，默认 UA 会被 Gamma 以 403 拒绝。"""
    headers = {"User-Agent": UA}
    if data is not None:
        headers["Content-Type"] = "application/json"
    last = None
    for i in range(retries):
        try:
            req = urllib.request.Request(url, data=data, headers=headers,
                                         method="POST" if data else "GET")
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.loads(r.read().decode("utf-8", "replace"))
        except Exception as e:  # noqa: BLE001
            last = e
            # 429/5xx 退避重试；4xx（除 429）直接放弃，重试无意义
            code = getattr(e, "code", None)
            if code and code != 429 and 400 <= code < 500:
                break
            if _stop:
                break
            time.sleep(min(2.0 * (i + 1), 10.0))
    raise RuntimeError(f"请求失败 {url}: {last}")


# ── 城市列表 ──

def load_cities(db: str | None):
    """优先用 fetch_data.py 留下的 cache/index.json，避免依赖数据库文件存在。"""
    idx = os.path.join(CACHE, "index.json")
    if not db and os.path.exists(idx):
        cities = json.load(open(idx)).get("cities", [])
        if cities:
            return [c for c in cities if c.get("slug") and c.get("tz")]
    if not db:
        db = os.path.join(PROJ, "src-tauri", "woolbrush.db")
    con = sqlite3.connect(db)
    rows = con.execute("select slug, iana_tz from cities where slug is not null").fetchall()
    con.close()
    return [{"slug": r[0], "tz": r[1]} for r in rows if r[0] and r[1]]


# ── 事件（档位 → token）解析，带磁盘缓存 ──

def event_slug(kind: str, city: str, d: dt.date) -> str:
    return f"{kind}-temperature-in-{city}-on-{MONTHS[d.month - 1]}-{d.day}-{d.year}"


_miss: dict[tuple, float] = {}   # (kind, city, date) -> 上次未找到的时刻


def resolve_event(kind: str, city: str, d: dt.date):
    """返回 [{label, no_token, yes_token}]；市场不存在返回 None。"""
    key = (kind, city, d.isoformat())
    p = os.path.join(CACHE, "live_events", kind, city, f"{d.isoformat()}.json")
    if os.path.exists(p):
        try:
            return json.load(open(p))
        except json.JSONDecodeError:
            pass   # 缓存损坏，重新拉
    last = _miss.get(key)
    if last is not None and time.time() - last < EVENT_RETRY_SEC:
        return None

    data = http_json(f"{GAMMA}/events?slug={event_slug(kind, city, d)}")
    if not data:
        _miss[key] = time.time()
        return None
    markets = []
    for m in data[0].get("markets", []):
        try:
            toks = json.loads(m.get("clobTokenIds") or "[]")
        except json.JSONDecodeError:
            continue
        if len(toks) != 2:
            continue
        markets.append({
            "label": m.get("groupItemTitle") or m.get("question") or "?",
            "yes_token": toks[0],
            "no_token": toks[1],
        })
    if not markets:
        _miss[key] = time.time()
        return None
    os.makedirs(os.path.dirname(p), exist_ok=True)
    json.dump(markets, open(p, "w"))
    _miss.pop(key, None)
    return markets


# ── 盘口抓取 ──

def norm_levels(levels, ascending: bool):
    """归一化为 [[价, 量]]：asks 升序、bids 降序，最优价恒在首位。

    交易所返回的档位顺序不保证，这里显式排序，下游不必再猜。
    """
    out = []
    for lv in levels or []:
        try:
            p = float(lv["price"])
            s = float(lv["size"])
        except (KeyError, TypeError, ValueError):
            continue
        if s <= 0:
            continue
        out.append([round(p, 4), round(s, 2)])
    out.sort(key=lambda x: x[0], reverse=not ascending)
    return out[:MAX_LEVELS]


def fetch_books(tokens: list[str]) -> dict:
    """批量拉订单簿，返回 {token_id: book}。单个分片失败不影响其余分片。"""
    books = {}
    chunks = [tokens[i:i + BOOK_CHUNK] for i in range(0, len(tokens), BOOK_CHUNK)]

    def one(chunk):
        body = json.dumps([{"token_id": t} for t in chunk]).encode()
        return http_json(f"{CLOB}/books", data=body)

    with cf.ThreadPoolExecutor(min(6, max(1, len(chunks)))) as ex:
        futs = {ex.submit(one, c): c for c in chunks}
        for f in cf.as_completed(futs):
            try:
                for b in f.result() or []:
                    aid = b.get("asset_id")
                    if aid:
                        books[str(aid)] = b
            except Exception as e:  # noqa: BLE001
                log(f"  盘口分片失败（{len(futs[f])} tokens）: {e}")
    return books


# ── 输出文件（按 UTC 日轮转，旧文件 gzip） ──

class Writer:
    def __init__(self, out_dir: str):
        self.dir = out_dir
        os.makedirs(out_dir, exist_ok=True)
        self.day = None
        self.fh = None

    def _rotate(self, day: str):
        if self.fh:
            self.fh.close()
            self.fh = None
            self._compress(self.day)
        self.day = day
        self.fh = open(os.path.join(self.dir, f"depth-{day}.jsonl"), "a", encoding="utf-8")

    def _compress(self, day: str):
        src = os.path.join(self.dir, f"depth-{day}.jsonl")
        if not os.path.exists(src):
            return
        try:
            with open(src, "rb") as i, gzip.open(src + ".gz", "wb", compresslevel=6) as o:
                shutil.copyfileobj(i, o)
            os.remove(src)
            log(f"已压缩 {os.path.basename(src)}.gz")
        except Exception as e:  # noqa: BLE001
            log(f"压缩 {src} 失败（原文件保留）: {e}")

    def write(self, rows: list[dict]):
        day = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d")
        if day != self.day:
            self._rotate(day)
        for r in rows:
            self.fh.write(json.dumps(r, ensure_ascii=False, separators=(",", ":")) + "\n")
        self.fh.flush()
        os.fsync(self.fh.fileno())

    def close(self):
        if self.fh:
            self.fh.close()
            self.fh = None


# ── 单轮采集 ──

def one_cycle(cities, kinds, writer: Writer):
    now = dt.datetime.now(dt.timezone.utc)
    ts = int(now.timestamp())

    # 1) 解析各城市当天（城市本地日）的事件
    targets = []          # (kind, city, mdate, lh, markets)
    tokens = []
    ev_fail = 0
    plan = []
    for c in cities:
        try:
            lt = now.astimezone(ZoneInfo(c["tz"]))
        except Exception:  # noqa: BLE001 —— 时区名异常，跳过该城市
            continue
        lh = round(lt.hour + lt.minute / 60.0, 3)
        for k in kinds:
            plan.append((k, c["slug"], lt.date(), lh))

    with cf.ThreadPoolExecutor(8) as ex:
        futs = {ex.submit(resolve_event, k, s, d): (k, s, d, lh) for k, s, d, lh in plan}
        for f in cf.as_completed(futs):
            k, s, d, lh = futs[f]
            try:
                mk = f.result()
            except Exception as e:  # noqa: BLE001
                ev_fail += 1
                log(f"  事件解析失败 {k}/{s}/{d}: {e}")
                continue
            if not mk:
                continue
            targets.append((k, s, d.isoformat(), lh, mk))
            for m in mk:
                tokens.append(m["no_token"])
                tokens.append(m["yes_token"])

    if not tokens:
        log(f"本轮无可用市场（事件失败 {ev_fail}）")
        return 0, 0

    # 2) 批量拉盘口
    books = fetch_books(sorted(set(tokens)))

    # 3) 组装记录：YES/NO 合并为一行，减少重复字段
    rows = []
    empty = 0
    for kind, city, mdate, lh, markets in targets:
        for m in markets:
            nb = books.get(m["no_token"])
            yb = books.get(m["yes_token"])
            if nb is None and yb is None:
                continue
            no_a = norm_levels(nb.get("asks") if nb else None, True)
            no_b = norm_levels(nb.get("bids") if nb else None, False)
            ys_a = norm_levels(yb.get("asks") if yb else None, True)
            ys_b = norm_levels(yb.get("bids") if yb else None, False)
            # 两侧全空 = 市场未开盘或已结算，纯噪声，不落盘
            if not (no_a or no_b or ys_a or ys_b):
                empty += 1
                continue
            ref = nb or yb

            def fnum(v):
                try:
                    return float(v)
                except (TypeError, ValueError):
                    return None

            rows.append({
                "ts": ts, "kind": kind, "city": city, "mdate": mdate, "lh": lh,
                "label": m["label"],
                "tick": fnum(ref.get("tick_size")),
                "minsz": fnum(ref.get("min_order_size")),
                "no_tok": m["no_token"], "no_a": no_a, "no_b": no_b,
                "no_lt": fnum(nb.get("last_trade_price")) if nb else None,
                "yes_tok": m["yes_token"], "yes_a": ys_a, "yes_b": ys_b,
                "yes_lt": fnum(yb.get("last_trade_price")) if yb else None,
            })

    if rows:
        writer.write(rows)
    return len(rows), empty


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--interval", type=int, default=300, help="采样间隔秒，默认 300（5 分钟）")
    ap.add_argument("--once", action="store_true", help="只跑一轮后退出，用于验证")
    ap.add_argument("--out", default=os.path.join(PROJ, "data", "depth"))
    ap.add_argument("--kinds", default="highest,lowest")
    ap.add_argument("--cities", default="", help="逗号分隔城市 slug，留空=全部")
    ap.add_argument("--db", default="", help="城市列表来源，留空优先用 cache/index.json")
    args = ap.parse_args()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    cities = load_cities(args.db or None)
    if args.cities:
        want = {s.strip() for s in args.cities.split(",") if s.strip()}
        cities = [c for c in cities if c["slug"] in want]
    kinds = [k.strip() for k in args.kinds.split(",") if k.strip()]
    if not cities or not kinds:
        log("没有城市或市场类型可采集，退出")
        return 1

    writer = Writer(args.out)
    log(f"启动：{len(cities)} 城市 × {kinds} | 间隔 {args.interval}s | 输出 {args.out}")

    n = 0
    try:
        while not _stop:
            t0 = time.time()
            n += 1
            try:
                wrote, empty = one_cycle(cities, kinds, writer)
                log(f"第 {n} 轮：写入 {wrote} 条，跳过空盘口 {empty} 条，耗时 {time.time() - t0:.1f}s")
            except Exception as e:  # noqa: BLE001 —— 任何异常都不能终止长期采集
                log(f"第 {n} 轮异常（已忽略，继续下一轮）: {e}")
            if args.once or _stop:
                break
            # 对齐到间隔边界，使样本落在整齐的时刻上（便于跨城市对比）
            sleep = args.interval - (time.time() % args.interval)
            while sleep > 0 and not _stop:
                time.sleep(min(sleep, 1.0))
                sleep -= 1.0
    finally:
        writer.close()
    log("已退出")
    return 0


if __name__ == "__main__":
    sys.exit(main())
