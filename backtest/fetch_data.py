#!/usr/bin/env python3
"""
回测数据抓取（全部为公开数据，不需要账户）

数据源：
- Gamma API      : 每个城市每天的「最高温」事件，含各档位 token 与结算结果（outcomePrices）
- CLOB API       : 各档位 NO token 的历史价格（prices-history，fidelity=10 分钟）
- IEM ASOS       : 气象站逐小时 METAR 观测（与程序 ST 源 weather.gov / Wunderground 同源）
- Open-Meteo     : 历史预报（historical-forecast-api，对应程序里的 MET 预报）

输出：backtest/cache/ 下的 JSON 缓存，可重复运行（已存在的文件跳过）。

用法：
    python3 backtest/fetch_data.py --db path/to/woolbrush.db --start 2026-08-10 --end 2026-09-16
"""
import argparse
import concurrent.futures as cf
import datetime as dt
import json
import os
import sqlite3
import sys
import time
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
CACHE = os.path.join(HERE, "cache")
GAMMA = "https://gamma-api.polymarket.com"
CLOB = "https://clob.polymarket.com"
IEM = "https://mesonet.agron.iastate.edu/cgi-bin/request/asos.py"
OM_HIST_FCST = "https://historical-forecast-api.open-meteo.com/v1/forecast"

MONTHS = ["january", "february", "march", "april", "may", "june", "july",
          "august", "september", "october", "november", "december"]


def http_get(url: str, retries: int = 4, timeout: int = 40) -> str:
    last = None
    for i in range(retries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "woolbrush-backtest/1.0"})
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return r.read().decode("utf-8", "replace")
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(1.5 * (i + 1))
    raise RuntimeError(f"GET failed {url}: {last}")


def cache_path(*parts: str) -> str:
    p = os.path.join(CACHE, *parts)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    return p


def load_cities(db: str):
    con = sqlite3.connect(db)
    rows = con.execute(
        "select slug, iana_tz, unit, station_code, lat, lon from cities "
        "where station_code is not null and station_code != ''"
    ).fetchall()
    return [dict(slug=r[0], tz=r[1], unit=r[2], station=r[3], lat=r[4], lon=r[5]) for r in rows]


def event_slug(city: str, d: dt.date) -> str:
    return f"highest-temperature-in-{city}-on-{MONTHS[d.month - 1]}-{d.day}-{d.year}"


# ── Gamma events ──

def fetch_event(city: str, d: dt.date):
    p = cache_path("events", city, f"{d.isoformat()}.json")
    if os.path.exists(p):
        return json.load(open(p))
    url = f"{GAMMA}/events?slug={event_slug(city, d)}"
    data = json.loads(http_get(url))
    out = None
    if data:
        ev = data[0]
        markets = []
        for m in ev.get("markets", []):
            try:
                tokens = json.loads(m.get("clobTokenIds") or "[]")
                prices = json.loads(m.get("outcomePrices") or "[]")
            except json.JSONDecodeError:
                tokens, prices = [], []
            if len(tokens) != 2:
                continue
            markets.append({
                "label": m.get("groupItemTitle") or m.get("question"),
                "yes_token": tokens[0],
                "no_token": tokens[1],
                "closed": bool(m.get("closed")),
                # outcomePrices ["1","0"] => YES 赢（该档位为最终最高温）
                "yes_won": (len(prices) == 2 and prices[0] in ("1", "1.0")),
                "resolved": (len(prices) == 2 and set(prices) <= {"0", "1", "0.0", "1.0"}),
            })
        out = {"slug": ev["slug"], "closed": ev.get("closed"), "description": ev.get("description", ""),
               "markets": markets}
    json.dump(out, open(p, "w"))
    return out


# ── CLOB price history ──

def fetch_prices(token: str, fidelity: int = 10):
    p = cache_path("prices", f"{token}.json")
    if os.path.exists(p):
        return json.load(open(p))
    url = f"{CLOB}/prices-history?market={token}&interval=max&fidelity={fidelity}"
    data = json.loads(http_get(url))
    hist = [(pt["t"], pt["p"]) for pt in data.get("history", [])]
    json.dump(hist, open(p, "w"))
    return hist


# ── IEM station observations ──

def fetch_obs(station: str, start: dt.date, end: dt.date, tz: str):
    """逐小时 METAR 观测，返回 [(local_iso, temp_f)]。IEM 站号：美国站去掉前缀 K，其余用 ICAO 四字码。"""
    key = f"{station}_{start}_{end}"
    p = cache_path("obs", f"{key}.json")
    if os.path.exists(p):
        return json.load(open(p))
    sid = station[1:] if (len(station) == 4 and station.startswith("K")) else station
    e2 = end + dt.timedelta(days=1)  # IEM 的 end 为开区间
    q = {
        "station": sid, "data": "tmpf",
        "year1": start.year, "month1": start.month, "day1": start.day,
        "year2": e2.year, "month2": e2.month, "day2": e2.day,
        "tz": tz, "format": "onlycomma", "latlon": "no", "missing": "M", "trace": "T", "direct": "no",
    }
    url = IEM + "?" + urllib.parse.urlencode(q) + "&report_type=3&report_type=4"
    text = http_get(url, timeout=120)
    rows = []
    for line in text.splitlines()[1:]:
        parts = line.split(",")
        if len(parts) < 3 or parts[2] in ("M", ""):
            continue
        try:
            rows.append((parts[1], float(parts[2])))
        except ValueError:
            continue
    json.dump(rows, open(p, "w"))
    return rows


# ── Open-Meteo historical forecast ──

def fetch_forecast(lat: float, lon: float, tz: str, start: dt.date, end: dt.date, unit: str):
    key = f"{lat:.3f}_{lon:.3f}_{start}_{end}"
    p = cache_path("fcst", f"{key}.json")
    if os.path.exists(p):
        return json.load(open(p))
    q = {
        "latitude": f"{lat:.4f}", "longitude": f"{lon:.4f}",
        "start_date": start.isoformat(), "end_date": end.isoformat(),
        "hourly": "temperature_2m", "timezone": tz,
        "temperature_unit": "fahrenheit" if "F" in unit else "celsius",
    }
    data = json.loads(http_get(OM_HIST_FCST + "?" + urllib.parse.urlencode(q), timeout=90))
    h = data.get("hourly", {})
    rows = list(zip(h.get("time", []), h.get("temperature_2m", [])))
    json.dump(rows, open(p, "w"))
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--start", required=True)
    ap.add_argument("--end", required=True)
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--cities", default="", help="逗号分隔的城市 slug，留空=全部有站点代码的城市")
    args = ap.parse_args()

    start = dt.date.fromisoformat(args.start)
    end = dt.date.fromisoformat(args.end)
    cities = load_cities(args.db)
    if args.cities:
        want = set(args.cities.split(","))
        cities = [c for c in cities if c["slug"] in want]
    days = [start + dt.timedelta(days=i) for i in range((end - start).days + 1)]
    print(f"cities={len(cities)} days={len(days)}", file=sys.stderr)

    # 1) 事件
    jobs = [(c["slug"], d) for c in cities for d in days]
    events = {}
    with cf.ThreadPoolExecutor(args.workers) as ex:
        futs = {ex.submit(fetch_event, c, d): (c, d) for c, d in jobs}
        done = 0
        for f in cf.as_completed(futs):
            c, d = futs[f]
            try:
                events[(c, d.isoformat())] = f.result()
            except Exception as e:  # noqa: BLE001
                print(f"event {c} {d}: {e}", file=sys.stderr)
            done += 1
            if done % 200 == 0:
                print(f"events {done}/{len(jobs)}", file=sys.stderr)
    found = sum(1 for v in events.values() if v)
    print(f"events found {found}/{len(jobs)}", file=sys.stderr)

    # 2) 价格历史（NO token + YES token 均拉；YES 用于反向策略评估）
    tokens = []
    for ev in events.values():
        if not ev:
            continue
        for m in ev["markets"]:
            tokens.append(m["no_token"])
    tokens = sorted(set(tokens))
    print(f"price tokens {len(tokens)}", file=sys.stderr)
    with cf.ThreadPoolExecutor(args.workers) as ex:
        futs = {ex.submit(fetch_prices, t): t for t in tokens}
        done = 0
        for f in cf.as_completed(futs):
            try:
                f.result()
            except Exception as e:  # noqa: BLE001
                print(f"prices {futs[f][:12]}: {e}", file=sys.stderr)
            done += 1
            if done % 500 == 0:
                print(f"prices {done}/{len(tokens)}", file=sys.stderr)

    # 3) 观测 + 预报（每城市一次调用覆盖整个区间）
    with cf.ThreadPoolExecutor(4) as ex:
        futs = []
        for c in cities:
            futs.append(ex.submit(fetch_obs, c["station"], start, end, c["tz"]))
            futs.append(ex.submit(fetch_forecast, c["lat"], c["lon"], c["tz"], start, end, c["unit"]))
        for f in cf.as_completed(futs):
            try:
                f.result()
            except Exception as e:  # noqa: BLE001
                print(f"weather: {e}", file=sys.stderr)

    # 索引文件
    json.dump({"start": start.isoformat(), "end": end.isoformat(), "cities": cities},
              open(cache_path("index.json"), "w"), indent=1)
    print("done", file=sys.stderr)


if __name__ == "__main__":
    main()
