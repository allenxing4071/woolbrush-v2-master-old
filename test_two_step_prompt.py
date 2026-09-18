import sqlite3, json, requests, re, sys

db_path = 'D:/DuMate/Polymarket/WoolBrush-羊毛刷-V2/data/woolbrush.db'
conn = sqlite3.connect(db_path)
cur = conn.cursor()
cur.execute('SELECT ollama_api_key, ollama_url, ollama_model FROM user_settings WHERE id=1')
row = cur.fetchone()
api_key, api_url, model = row[0], row[1], row[2]
conn.close()

with open('D:/DuMate/Polymarket/WoolBrush-羊毛刷-V2/.dumate/inbox/2026-09-02.jsonl', 'r', encoding='utf-8') as f:
    for line in f:
        rec = json.loads(line)
        if rec.get('city') == 'karachi' and rec.get('round') == 89 and rec.get('event') == 'city_snapshot':
            snap = rec['data']
            break

unit = '\u00b0C'
bid_ask_min = float(snap['toolbar']['askMin'])
bid_ask_max = float(snap['toolbar']['askMax'])
st = snap['st']
awc = snap['awc']
met = snap['met_forecast']

# Build prompt as list of strings to avoid escaping issues
parts = []
parts.append("你是一个 Polymarket 气温市场的交易分析师。\n")
parts.append("## 核心原则：保本第一\n")
parts.append("保本是最高优先级。只允许开仓最有把握的档位——即 NO概率极高、几乎不可能成为最高温的档位。\n")
parts.append("如果对某个档位没有很大的把握，就不要开仓。宁可错过机会，也绝不冒亏损风险。\n\n")
parts.append("## 任务\n")
parts.append("分析以下城市气温市场数据，判断是否应该开仓，以及开仓的档位和方向。\n\n")
parts.append("## 市场规则\n")
parts.append("- 每个城市每天有一个「最高温」市场，包含多个温度档位。\n")
parts.append("- 最终结算时，只有一个档位的 YES=1（实际最高温命中该档位），其余所有档位 NO=1。\n")
parts.append("- YES价格 + NO价格 = 1。YES价格越高 = 该档位越可能是最高温 = NO越不安全。\n\n")
parts.append("## ST 温度——市场结算的判定标准\n")
parts.append("ST 温度是判定市场温度档位胜负的核心标准和最终依据，必须高度重视。\n")
parts.append("在判断哪个温度档位会成为最终最高温时，必须以 ST 温度的最高温度为主要依据。\n\n")
parts.append("## 开仓硬门控约束（必须全部通过才能开仓）\n")
parts.append("1. 只考虑 NO 侧开仓（买入 NO token）。\n")
parts.append("2. GATE-PRICE：档位的 bid 和 ask 必须都在 BID/ASK 区间内，否则排除该档位。\n")
parts.append("3. GATE-OFFSET（与当地时间联动，三条独立判断，满足任一即 PASS）：偏移量 = (当前档位温度 - YES概率最高的档位温度) / 档位步长。\n")
parts.append("   - 条件1：当地时间 <= 13 时，偏移量 >= 3 则 PASS\n")
parts.append("   - 条件2：当地时间 > 13 时，偏移量 >= 2 则 PASS\n")
parts.append("   - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度时，偏移量 >= 1 则 PASS\n")
parts.append("   以上三条为独立判断，满足任一即 PASS，全部不满足才 FAIL。\n")
parts.append("5. 时间因素：如果当前当地时间在 10:00-16:00 之间，温度仍可能继续上升，已观测到的最高温未必是最终最高温。\n")
parts.append("6. 如果市场尚未充分定价（大部分档位无 ask 价格），不要开仓。\n\n")
parts.append("## 输出要求（两步推理结构，严格遵守）\n\n")
parts.append("### 第一步：逐条约束检查\n")
parts.append("对每个有 ask 价格的档位，逐条列出以下检查结果：\n")
parts.append("- GATE-PRICE：bid 和 ask 是否都在 [BID_ASK_MIN, BID_ASK_MAX] 区间内？结果 PASS 或 FAIL\n")
parts.append("- GATE-OFFSET：偏移量 = (档位温度 - YES概率最高档位温度) / 步长 = 具体数值。独立判断以下三条，满足任一即 PASS：\n")
parts.append("  - 条件1：当地时间 <= 13 且偏移量 >= 3\n")
parts.append("  - 条件2：当地时间 > 13 且偏移量 >= 2\n")
parts.append("  - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度且偏移量 >= 1\n")
parts.append("  全部不满足才 FAIL\n\n")
parts.append("**关键规则：任何一项为 FAIL 的档位，绝对不能出现在最终 JSON 的 actions 数组中。**\n\n")
parts.append("### 第二步：输出最终 JSON\n")
parts.append("在完成所有档位的逐条检查后，仅输出一个 JSON 格式的结果，不要输出多个 JSON：\n")
parts.append("```json\n")
parts.append("{\n  \"actions\": [\n    {\n      \"threshold_label\": \"档位标签\",\n      \"side\": \"NO\",\n      \"reason\": \"简要说明\"\n    }\n  ],\n  \"summary\": \"总体分析说明\"\n}\n")
parts.append("```\n")
parts.append("所有 GATE 检查全部 PASS 的档位才能放入 actions。如果没有档位通过所有 GATE 检查，actions 返回空数组 []，并在 summary 中说明原因。\n\n")

# City data
parts.append("## 当前城市数据\n")
parts.append(f"- 城市: karachi\n- 时区: Asia/Karachi\n- 当地时间: 11:00\n- 温度单位: {unit}\n\n")
parts.append("### 操作栏参数\n")
parts.append(f"- BID/ASK 区间: [{bid_ask_min:.3f}, {bid_ask_max:.3f}]\n\n")
parts.append("### ST 温度（市场结算判定标准，最高优先级）\n")
parts.append(f"- ST: {st['high']:.1f}{unit}/{st['current']:.1f}{unit} {st['condition']}\n")
parts.append(f"- 当天最高温度（ST）: {st['high']:.1f}{unit}\n")
parts.append(f"- 当前实时温度（ST）: {st['current']:.1f}{unit}\n")
parts.append(f"- 天气状况: {st['condition']}\n\n")
parts.append("### AWC 实况温度\n")
parts.append(f"- 当天已检测到的最高温度: {awc['max']:.1f}{unit}\n")
hourly = awc.get('hourly_temps', {})
if hourly:
    max_val = awc['max']
    max_hours = [h for h, v in hourly.items() if abs(v - max_val) < 0.05]
    if max_hours:
        parts.append(f"- 最高温度出现在当地时间 {min(max_hours, key=lambda x: int(x))}:00\n")
parts.append(f"- 当前实时温度: {awc['current']:.1f}{unit}\n")
if hourly:
    parts.append("- 按小时观测温度:\n")
    for h in sorted(hourly.keys(), key=lambda x: int(x)):
        parts.append(f"  - {h}:00 -> {hourly[h]:.1f}{unit}\n")
parts.append("\n")
parts.append("### MET 预报温度 (10:00-17:00)\n")
for i, f_item in enumerate(met):
    hour = 10 + i
    parts.append(f"- {hour}:00 -> {f_item['temp']:.1f}{unit}\n")

parts.append("\n### 温度档位及价格\n")
parts.append("| 档位 | YES概率 | NO概率 | bid | ask | mid |\n")
parts.append("|------|---------|--------|-----|-----|-----|\n")
for t in snap['thresholds']:
    bid_str = f"{t['bid']:.3f}" if t.get('bid') is not None else "-"
    ask_str = f"{t['ask']:.3f}" if t.get('ask') is not None else "-"
    mid_str = f"{t['mid']:.3f}" if t.get('mid') is not None else "-"
    parts.append(f"| {t['label']} | {t['yes_price']:.3f} | {t['no_price']:.3f} | {bid_str} | {ask_str} | {mid_str} |\n")

prompt = "".join(parts)
print(f"Prompt length: {len(prompt)} chars")
print(f"Model: {model}")
print()

chat_req = {"model": model, "messages": [{"role": "user", "content": prompt}], "temperature": 0.1, "top_p": 0.8}

print("Calling Ollama API (direct, no proxy)...")
try:
    resp = requests.post(
        api_url,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        json=chat_req,
        timeout=120,
    )
    print(f"HTTP Status: {resp.status_code}")
    print()
    if resp.status_code == 200:
        data = resp.json()
        content = data["choices"][0]["message"]["content"]
        print("=== LLM RESPONSE (gemma4:31b, two-step prompt) ===")
        print(content)
        print()
        # Try to extract JSON
        json_match = re.search(r'\{[\s\S]*\}', content)
        if json_match:
            raw = json_match.group()
            try:
                parsed = json.loads(raw)
                print("=== PARSED JSON ===")
                print(json.dumps(parsed, ensure_ascii=False, indent=2))
                actions = parsed.get("actions", [])
                print(f"\n=== RESULT: actions has {len(actions)} item(s) ===")
                if actions:
                    for a in actions:
                        print(f"  - {a.get('threshold_label')} {a.get('side')}")
                else:
                    print("  EMPTY (no open) - CORRECT!")
            except json.JSONDecodeError as e:
                print(f"JSON parse error: {e}")
                print(f"Raw: {raw[:300]}")
        else:
            print("No JSON found in response")
    else:
        print(f"Error: {resp.text[:1000]}")
except Exception as e:
    print(f"Request failed: {e}")
