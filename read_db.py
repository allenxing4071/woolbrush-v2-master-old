import sqlite3, json, sys

db_path = r'D:\DuMate\Polymarket\WoolBrush-羊毛刷-V2\src-tauri\data\woolbrush.db'
db = sqlite3.connect(db_path)
db.row_factory = sqlite3.Row

# cities
rows = db.execute('SELECT slug, city_name, utc_offset, iana_tz, unit FROM cities ORDER BY slug').fetchall()
print(f'=== Cities: {len(rows)} ===')
for r in rows:
    print(f"{r['slug']:20s} | {r['city_name']:20s} | {r['utc_offset']:8s} | {r['iana_tz']:30s} | {r['unit']}")

# settings
s = db.execute('SELECT * FROM user_settings WHERE id=1').fetchone()
print(f"\n=== Settings ===")
if s:
    for k in s.keys():
        v = s[k]
        if k in ('private_key',):
            v = '***'
        print(f"{k}: {v}")
else:
    print("No settings found")

# trades
t = db.execute('SELECT id, city, side, threshold, entry_price, size, cost, status, timestamp FROM trades ORDER BY timestamp DESC LIMIT 20').fetchall()
print(f"\n=== Trades: {len(t)} ===")
for r in t:
    print(f"{r['timestamp']:20s} | {r['city']:15s} | {r['side']:3s} | {r['threshold']:10s} | entry={r['entry_price']:.4f} | size={r['size']} | cost={r['cost']:.2f} | {r['status']}")

db.close()
