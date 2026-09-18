#!/usr/bin/env python3
"""
Scan all temperature markets and find openable positions.
Uses concurrent requests for speed.
"""

import json, time, sys, re, os
from datetime import datetime, timezone, timedelta
from concurrent.futures import ThreadPoolExecutor, as_completed
import urllib.request
import urllib.error
import ssl

# --- Config ---
GAMMA_BASE = "https://gamma-api.polymarket.com"
CLOB_BASE  = "https://clob.polymarket.com"
PROXY_URL  = "http://localhost:15236"

# Strategy params (matching App.tsx defaults)
ASK_MIN   = 0.950
ASK_MAX   = 0.995
OFFSET_MIN = 2
TIME_HOUR  = 17

ssl_ctx = ssl.create_default_context()
ssl_ctx.check_hostname = False
ssl_ctx.verify_mode = ssl.CERT_NONE

proxy_handler = urllib.request.ProxyHandler({
    'http':  PROXY_URL,
    'https': PROXY_URL,
})
opener = urllib.request.build_opener(proxy_handler, urllib.request.HTTPSHandler(context=ssl_ctx))

def fetch_json(url, timeout=15):
    req = urllib.request.Request(url, headers={
        'User-Agent': 'Mozilla/5.0',
        'Accept': 'application/json',
    })
    resp = opener.open(req, timeout=timeout)
    data = resp.read().decode('utf-8')
    return json.loads(data)

# City data: (slug, iana_tz, utc_offset_hours, unit)
CITIES = [
    ("amsterdam", "Europe/Amsterdam", 1, "C"), ("ankara", "Europe/Istanbul", 3, "C"),
    ("athens", "Europe/Athens", 2, "C"), ("atlanta", "America/New_York", -5, "F"),
    ("austin", "America/Chicago", -6, "F"), ("bangkok", "Asia/Bangkok", 7, "C"),
    ("barcelona", "Europe/Madrid", 1, "C"), ("beijing", "Asia/Shanghai", 8, "C"),
    ("berlin", "Europe/Berlin", 1, "C"), ("bogota", "America/Bogota", -5, "C"),
    ("boston", "America/New_York", -5, "F"), ("brussels", "Europe/Brussels", 1, "C"),
    ("buenos-aires", "America/Argentina/Buenos_Aires", -3, "C"), ("busan", "Asia/Seoul", 9, "C"),
    ("cairo", "Africa/Cairo", 2, "C"), ("cape-town", "Africa/Johannesburg", 2, "C"),
    ("chengdu", "Asia/Shanghai", 8, "C"), ("chicago", "America/Chicago", -6, "F"),
    ("chongqing", "Asia/Shanghai", 8, "C"), ("dallas", "America/Chicago", -6, "F"),
    ("delhi", "Asia/Kolkata", 5.5, "C"), ("denver", "America/Denver", -7, "F"),
    ("detroit", "America/Detroit", -5, "F"), ("dhaka", "Asia/Dhaka", 6, "C"),
    ("dubai", "Asia/Dubai", 4, "C"), ("dublin", "Europe/Dublin", 0, "C"),
    ("fuzhou", "Asia/Shanghai", 8, "C"), ("guangzhou", "Asia/Shanghai", 8, "C"),
    ("hangzhou", "Asia/Shanghai", 8, "C"), ("helsinki", "Europe/Helsinki", 2, "C"),
    ("ho-chi-minh-city", "Asia/Ho_Chi_Minh", 7, "C"), ("hong-kong", "Asia/Hong_Kong", 8, "C"),
    ("houston", "America/Chicago", -6, "F"), ("istanbul", "Europe/Istanbul", 3, "C"),
    ("jakarta", "Asia/Jakarta", 7, "C"), ("jeddah", "Asia/Riyadh", 3, "C"),
    ("jinan", "Asia/Shanghai", 8, "C"), ("johannesburg", "Africa/Johannesburg", 2, "C"),
    ("karachi", "Asia/Karachi", 5, "C"), ("kuala-lumpur", "Asia/Kuala_Lumpur", 8, "C"),
    ("lagos", "Africa/Lagos", 1, "C"), ("las-vegas", "America/Los_Angeles", -8, "F"),
    ("lima", "America/Lima", -5, "C"), ("lisbon", "Europe/Lisbon", 0, "C"),
    ("london", "Europe/London", 1, "C"), ("los-angeles", "America/Los_Angeles", -8, "F"),
    ("lucknow", "Asia/Kolkata", 5.5, "C"), ("madrid", "Europe/Madrid", 1, "C"),
    ("manila", "Asia/Manila", 8, "C"), ("mexico-city", "America/Mexico_City", -6, "C"),
    ("miami", "America/New_York", -5, "F"), ("milan", "Europe/Rome", 1, "C"),
    ("minneapolis", "America/Chicago", -6, "F"), ("moscow", "Europe/Moscow", 3, "C"),
    ("mumbai", "Asia/Kolkata", 5.5, "C"), ("munich", "Europe/Berlin", 1, "C"),
    ("nairobi", "Africa/Nairobi", 3, "C"), ("nanjing", "Asia/Shanghai", 8, "C"),
    ("nashville", "America/Chicago", -6, "F"), ("new-york", "America/New_York", -5, "F"),
    ("oslo", "Europe/Oslo", 1, "C"), ("panama-city", "America/Panama", -5, "C"),
    ("paris", "Europe/Paris", 1, "C"), ("philadelphia", "America/New_York", -5, "F"),
    ("phoenix", "America/Phoenix", -7, "F"), ("portland", "America/Los_Angeles", -8, "F"),
    ("qingdao", "Asia/Shanghai", 8, "C"), ("rio-de-janeiro", "America/Sao_Paulo", -3, "C"),
    ("riyadh", "Asia/Riyadh", 3, "C"), ("rome", "Europe/Rome", 1, "C"),
    ("san-francisco", "America/Los_Angeles", -8, "F"), ("santiago", "America/Santiago", -4, "C"),
    ("sao-paulo", "America/Sao_Paulo", -3, "C"), ("seattle", "America/Los_Angeles", -8, "F"),
    ("seoul", "Asia/Seoul", 9, "C"), ("shanghai", "Asia/Shanghai", 8, "C"),
    ("shenzhen", "Asia/Shanghai", 8, "C"), ("singapore", "Asia/Singapore", 8, "C"),
    ("stockholm", "Europe/Stockholm", 1, "C"), ("sydney", "Australia/Sydney", 10, "C"),
    ("taipei", "Asia/Taipei", 8, "C"), ("tel-aviv", "Asia/Jerusalem", 2, "C"),
    ("tokyo", "Asia/Tokyo", 9, "C"), ("toronto", "America/Toronto", -5, "C"),
    ("vancouver", "America/Vancouver", -8, "F"), ("vienna", "Europe/Vienna", 1, "C"),
    ("warsaw", "Europe/Warsaw", 1, "C"), ("washington-dc", "America/New_York", -5, "F"),
    ("wellington", "Pacific/Auckland", 12, "C"), ("wuhan", "Asia/Shanghai", 8, "C"),
    ("xiamen", "Asia/Shanghai", 8, "C"), ("zhengzhou", "Asia/Shanghai", 8, "C"),
    ("zurich", "Europe/Zurich", 1, "C"),
]

MONTH_NAMES = [
    "january", "february", "march", "april", "may", "june",
    "july", "august", "september", "october", "november", "december"
]

def local_dt(offset_hours):
    now_utc = datetime.now(timezone.utc)
    return now_utc + timedelta(hours=offset_hours)

def build_slug(city_slug, local):
    month_name = MONTH_NAMES[local.month - 1]
    return f"highest-temperature-in-{city_slug}-on-{month_name}-{local.day}-{local.year}"

def temp_value(label):
    m = re.search(r'(\d+)', label)
    if m:
        return int(m.group(1))
    return None

def fetch_event(city, tz, offset, unit):
    local = local_dt(offset)
    slug = build_slug(city, local)
    url = f"{GAMMA_BASE}/events?slug={slug}"
    try:
        data = fetch_json(url, timeout=10)
        if isinstance(data, list) and len(data) > 0:
            return (city, tz, offset, unit, local, data[0])
    except:
        pass
    return None

def fetch_orderbook(token_id):
    url = f"{CLOB_BASE}/book?token_id={token_id}"
    try:
        data = fetch_json(url, timeout=10)
        return token_id, data
    except:
        return token_id, None

def get_best_bid_ask(book):
    bids = book.get('bids', [])
    asks = book.get('asks', [])
    best_bid = float(bids[0]['price']) if bids else 0.0
    best_ask = float(asks[0]['price']) if asks else 1.0
    mid = (best_bid + best_ask) / 2.0 if best_bid > 0 and best_ask < 1 else (best_bid or best_ask)
    return best_bid, best_ask, mid

def main():
    now_utc = datetime.now(timezone.utc)
    print(f"=== WoolBrush V2 Opportunity Scanner ===")
    print(f"UTC: {now_utc.isoformat()}")
    print(f"Strategy: NO side | bid/ask in [{ASK_MIN}, {ASK_MAX}] | offset >= {OFFSET_MIN} | local hour >= {TIME_HOUR}")
    print(f"Proxy: {PROXY_URL}")
    print()

    # Phase 1: Fetch all city events concurrently
    print("--- Phase 1: Fetching Gamma events (concurrent) ---")
    all_events = []
    with ThreadPoolExecutor(max_workers=20) as executor:
        futures = {executor.submit(fetch_event, c[0], c[1], c[2], c[3]): c for c in CITIES}
        for f in as_completed(futures):
            result = f.result()
            if result:
                all_events.append(result)
                city = result[0]
                local = result[4]
                ev = result[5]
                n_markets = len(ev.get('markets', []))
                print(f"  [OK] {city:20s} | local {local.strftime('%m-%d %H:%M')} | {n_markets} markets")
    
    print(f"\nPhase 1: {len(all_events)}/{len(CITIES)} cities with events")
    print()

    # Phase 2: Parse thresholds
    print("--- Phase 2: Parsing thresholds ---")
    all_thresholds = []
    all_token_ids = []
    
    for city, tz, offset, unit, local, ev in all_events:
        markets = ev.get('markets', [])
        event_slug = ev.get('slug', '')
        for m in markets:
            question = m.get('question', '')
            group_title = m.get('groupItemTitle', '')
            label = group_title if group_title else question
            token_ids_raw = m.get('clobTokenIds', '[]')
            try:
                token_ids = json.loads(token_ids_raw) if isinstance(token_ids_raw, str) else token_ids_raw
            except:
                token_ids = []
            if len(token_ids) < 2:
                continue
            prices_raw = m.get('outcomePrices', '[]')
            try:
                prices = json.loads(prices_raw) if isinstance(prices_raw, str) else prices_raw
                prices = [float(p) for p in prices]
            except:
                prices = []
            yes_price = prices[0] if len(prices) > 0 else 0.0
            no_price = prices[1] if len(prices) > 1 else 0.0
            temp_val = temp_value(label)
            if temp_val is None:
                continue
            all_thresholds.append({
                'city': city, 'tz': tz, 'offset': offset, 'unit': unit,
                'local': local, 'event_slug': event_slug,
                'market_id': m.get('id', ''), 'label': label,
                'yes_token_id': token_ids[0], 'no_token_id': token_ids[1],
                'yes_price': yes_price, 'no_price': no_price, 'temp_val': temp_val,
            })
            all_token_ids.append(token_ids[1])  # NO token

    print(f"Total thresholds: {len(all_thresholds)}")
    print(f"Total NO tokens to fetch: {len(all_token_ids)}")
    print()

    # Phase 3: Fetch order books concurrently
    print("--- Phase 3: Fetching CLOB order books (concurrent) ---")
    price_map = {}
    with ThreadPoolExecutor(max_workers=20) as executor:
        futures = {executor.submit(fetch_orderbook, tid): tid for tid in all_token_ids}
        done = 0
        for f in as_completed(futures):
            tid, book = f.result()
            if book:
                bid, ask, mid = get_best_bid_ask(book)
                price_map[tid] = (bid, ask, mid)
            done += 1
            if done % 100 == 0:
                print(f"  Fetched {done}/{len(all_token_ids)}...")
    
    print(f"Phase 3: {len(price_map)} prices fetched")
    print()

    # Phase 4: Apply strategy
    print("--- Phase 4: Applying strategy ---")
    
    # Group by event_slug
    event_groups = {}
    for t in all_thresholds:
        key = t['event_slug']
        if key not in event_groups:
            event_groups[key] = []
        event_groups[key].append(t)
    
    candidates = []
    near_misses = []
    
    for event_slug, group in event_groups.items():
        if not group:
            continue
        all_have_live = all(t['no_token_id'] in price_map for t in group)
        if not all_have_live:
            continue
        
        # Find YES peak
        peak_temp = 0
        peak_yes_prob = -1
        for t in group:
            _, _, mid = price_map[t['no_token_id']]
            yes_prob = 1 - mid
            if yes_prob > peak_yes_prob:
                peak_yes_prob = yes_prob
                peak_temp = t['temp_val']
        
        is_fahrenheit = any(t['unit'] == 'F' for t in group)
        step = 2 if is_fahrenheit else 1
        
        for t in group:
            offset = (t['temp_val'] - peak_temp) / step
            bid, ask, mid = price_map[t['no_token_id']]
            local_hour = t['local'].hour
            
            reasons = []
            if bid < ASK_MIN or bid > ASK_MAX:
                reasons.append(f"bid={bid:.3f} out_of_range")
            if ask < ASK_MIN or ask > ASK_MAX:
                reasons.append(f"ask={ask:.3f} out_of_range")
            if abs(offset) < OFFSET_MIN:
                reasons.append(f"offset={abs(offset):.0f}<{OFFSET_MIN}")
            if local_hour < TIME_HOUR:
                reasons.append(f"local_hr={local_hour}<{TIME_HOUR}")
            
            entry = {
                'city': t['city'], 'label': t['label'], 'unit': t['unit'],
                'bid': bid, 'ask': ask, 'mid': mid,
                'offset': offset, 'peak_temp': peak_temp,
                'peak_yes_prob': peak_yes_prob, 'local_hour': local_hour,
                'local_str': t['local'].strftime('%m-%d %H:%M'),
                'no_token_id': t['no_token_id'],
                'yes_price': t['yes_price'],
            }
            
            if not reasons:
                candidates.append(entry)
            elif bid >= 0.5 and bid <= 1.0:
                entry['reasons'] = reasons
                near_misses.append(entry)
    
    candidates.sort(key=lambda x: abs(x['offset']), reverse=True)
    near_misses.sort(key=lambda x: x['bid'], reverse=True)
    
    # Output results
    print()
    print("=" * 120)
    print(f"OPENABLE POSITIONS ({len(candidates)} found)")
    print("=" * 120)
    if candidates:
        for c in candidates:
            print(f"  {c['city']:20s} | {c['label']:15s} | NO bid={c['bid']:.3f} ask={c['ask']:.3f} mid={c['mid']:.3f} | offset={c['offset']:+.0f} (peak={c['peak_temp']}) | local {c['local_str']} (hr={c['local_hour']}) | YES_peak={c['peak_yes_prob']:.3f}")
    else:
        print("  None found.")
    
    print()
    print("=" * 120)
    print(f"NEAR MISSES (top 30, at least bid >= 0.5)")
    print("=" * 120)
    for nm in near_misses[:30]:
        print(f"  {nm['city']:20s} | {nm['label']:15s} | NO bid={nm['bid']:.3f} ask={nm['ask']:.3f} | offset={nm['offset']:+.0f} peak={nm['peak_temp']} | local_hr={nm['local_hour']} | {'; '.join(nm['reasons'])}")
    
    print()
    print("=" * 120)
    print("SUMMARY")
    print("=" * 120)
    print(f"  Cities scanned:    {len(CITIES)}")
    print(f"  Events found:      {len(all_events)}")
    print(f"  Thresholds:        {len(all_thresholds)}")
    print(f"  Prices fetched:    {len(price_map)}")
    print(f"  Openable:          {len(candidates)}")
    print(f"  Near misses:       {len(near_misses)}")
    
    # Top events by peak YES prob
    print()
    print("--- Top 20 events by peak YES probability ---")
    event_peaks = []
    for event_slug, group in event_groups.items():
        if not group:
            continue
        all_have_live = all(t['no_token_id'] in price_map for t in group)
        if not all_have_live:
            continue
        peak_t = 0
        peak_yp = -1
        for t in group:
            _, _, mid = price_map[t['no_token_id']]
            yp = 1 - mid
            if yp > peak_yp:
                peak_yp = yp
                peak_t = t['temp_val']
        event_peaks.append({
            'city': group[0]['city'], 'local': group[0]['local'],
            'peak_temp': peak_t, 'peak_yes_prob': peak_yp,
            'unit': group[0]['unit'], 'count': len(group),
        })
    event_peaks.sort(key=lambda x: x['peak_yes_prob'], reverse=True)
    for ep in event_peaks[:20]:
        u = 'F' if ep['unit'] == 'F' else 'C'
        print(f"  {ep['city']:20s} | local {ep['local'].strftime('%m-%d %H:%M')} | peak={ep['peak_temp']}{u} YES={ep['peak_yes_prob']:.3f} | {ep['count']} thresholds")

    # Also output full data as JSON for analysis
    output = {
        'scan_time': now_utc.isoformat(),
        'strategy': {'askMin': ASK_MIN, 'askMax': ASK_MAX, 'offsetMin': OFFSET_MIN, 'timeHour': TIME_HOUR},
        'candidates': candidates,
        'near_misses': near_misses[:50],
        'top_events': event_peaks[:30],
    }
    out_path = os.path.join(os.path.dirname(__file__), 'scan_result.json')
    with open(out_path, 'w', encoding='utf-8') as f:
        json.dump(output, f, ensure_ascii=False, indent=2, default=str)
    print(f"\nFull results saved to: {out_path}")

if __name__ == '__main__':
    main()
