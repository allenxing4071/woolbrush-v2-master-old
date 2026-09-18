import sqlite3, json, requests, re

# Read Ollama config from DB
db_path = 'D:/DuMate/Polymarket/WoolBrush-羊毛刷-V2/data/woolbrush.db'
conn = sqlite3.connect(db_path)
cur = conn.cursor()
cur.execute('SELECT ollama_api_key, ollama_url, ollama_model FROM user_settings WHERE id=1')
row = cur.fetchone()
api_key, api_url, model = row[0], row[1], row[2]
conn.close()

# Read Seattle round 17 snapshot (the round where 68-69°F was actually opened)
with open('D:/DuMate/Polymarket/WoolBrush-羊毛刷-V2/.dumate/inbox/2026-09-03(1).jsonl', 'r', encoding='utf-8') as f:
    for line in f:
        rec = json.loads(line)
        if rec.get('city') == 'seattle' and rec.get('round') == 17 and rec.get('event') == 'city_snapshot':
            snap = rec['data']
            break

unit = '\u00b0F'
bid_ask_min = float(snap['toolbar']['askMin'])
bid_ask_max = float(snap['toolbar']['askMax'])
st = snap['st']
awc = snap['awc']
met = snap['met_forecast']
local_time = snap['local_time']
city_tz = snap['city_tz']

# Yesterday data from METAR for 2026-09-02 (the day before this snapshot's date 2026-09-03)
yesterday_high = 69.1  # 20.6C = 69.1F, max at 11:53 local
yesterday_condition = "broken"  # BKN cover at max temp time

# Build prompt matching latest analyze_cmd.rs (with GATE-YESTERDAY hard constraint)
parts = []

# default_llm_prompt() core (latest version with GATE-YESTERDAY as item 10)
parts.append("你是一个 Polymarket 气温市场的交易分析师。\n\n")
parts.append("## 核心原则：保本第一\n")
parts.append("保本是最高优先级。只允许开仓最有把握的档位——即 NO概率极高、几乎不可能成为最高温的档位。\n")
parts.append("如果对某个档位没有很大的把握，就不要开仓。宁可错过机会，也绝不冒亏损风险。\n")
parts.append("没有合适机会时，should_open 返回 false。\n\n")
parts.append("## 任务\n")
parts.append("分析以下城市气温市场数据，判断是否应该开仓，以及开仓的档位和方向。\n\n")
parts.append("## 市场规则\n")
parts.append("- 每个城市每天有一个「最高温」市场，包含多个温度档位（如 20°C, 21°C, ... 30°C or higher）。\n")
parts.append("- 最终结算时，只有一个档位的 YES=1（实际最高温命中该档位），其余所有档位 NO=1。\n")
parts.append("- 温度档位表中的 YES价格 和 NO价格 就是各自命中的概率（YES价格 + NO价格 = 1）。\n")
parts.append("- YES价格越高 = 该档位越可能是最高温 = NO价格越低 = NO大概率不会结算为1。\n")
parts.append("- YES价格越低 = 该档位越不可能是最高温 = NO价格越高 = NO大概率结算为1。\n\n")
parts.append("## ST 温度——市场结算的判定标准\n")
parts.append("ST 温度是判定市场温度档位胜负的核心标准和最终依据，必须高度重视。\n")
parts.append("ST 温度格式为：最高温度/实时温度 天气状况（如 \"35/32°C scattered\"）。\n")
parts.append("ST 温度的最高温度反映当天已观测到的最高气温，实时温度反映当前实际气温，天气状况反映当前气象条件。\n")
parts.append("在判断哪个温度档位会成为最终最高温时，必须以 ST 温度的最高温度为主要依据，而非 AWC 或 MET 数据。\n")
parts.append("AWC 实况和 MET 预报仅作为辅助参考，当 ST 温度与 AWC/MET 存在差异时，以 ST 温度为准。\n\n")
parts.append("## 开仓条件\n")
parts.append("1. 只考虑 NO 侧开仓（买入 NO token）。\n")
parts.append("2. 必须选择 NO价格（NO概率）高的档位——这意味着该档位大概率不是最高温，NO结算为1的概率大。\n")
parts.append("3. NO价格（NO概率）低的档位绝对不能买入——这说明市场认为该档位很可能是最高温，买入NO大概率亏损。\n")
parts.append("4. ST 温度的最高温度是判断最终最高温的首要依据；AWC 实况温度可作为交叉验证；MET 预报值不够准确，不要直接用其数值判断最高温，但可以参考其预示的最高温出现时间段。\n")
parts.append("5. 如果所有档位的 NO价格都很低（市场已将最高温锁定在很窄范围），说明没有安全的NO开仓机会，不要开仓。\n")
parts.append("6. 如果市场尚未充分定价（大部分档位无价格数据），不要开仓。\n")
parts.append("7. 必须遵守操作栏参数约束：\n")
parts.append("   - 档位的 bid 和 ask 必须都在 BID/ASK 区间内，否则排除该档位。\n")
parts.append("   - OFFSET 约束（与当地时间联动，三条独立判断，满足任一即 PASS）：YES峰值档位 = 温度档位表中 mid 值最小的档位（mid 最小 = 市场认为该档位最可能是最终最高温）。注意：必须用 mid 列判定，不要用 YES概率(价格) 列，该列可能滞后或不准确。偏移量 = (当前档位下限 - YES峰值档位下限) / 档位步长。档位步长：°F城市=2（如\"68-69°F\"下限68），°C城市=1（如\"25°C\"下限25）。示例：峰值\"66-67°F\"、当前\"68-69°F\" → 偏移量=(68-66)/2=1。\n")
parts.append("     - 条件1：当地时间 <= 13 时，偏移量 >= 3 则 PASS\n")
parts.append("     - 条件2：当地时间 > 13 时，偏移量 >= 2 则 PASS\n")
parts.append("     - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度时，偏移量 >= 1 则 PASS\n")
parts.append("     以上三条为独立判断，满足任一即 PASS，全部不满足才 FAIL。\n")
parts.append("8. 必须结合时间因素判断最高温是否已经定型：\n")
parts.append("   - 日最高温通常出现在 14:00-16:00。如果当前当地时间已过 17:00，当日最高温大概率已确定，不再会升得更高，此时可以更有信心地排除该最高温档位附近的开仓。\n")
parts.append("   - 如果当前当地时间在 10:00-16:00 之间，温度仍可能继续上升，已观测到的最高温未必是最终最高温。此时需要参考 MET 预报中剩余时段是否有更高温度，判断最终最高温可能落在哪个档位。\n")
parts.append("   - 如果当前当地时间在 10:00 之前，当日最高温远未确定，预报不确定性最大，应更加谨慎，优先选择离预报峰值很远的档位开仓 NO。\n")
parts.append("   - 关注最高温已观测到的时间点：如果最高温出现在 15:00 且当前已过 17:00，说明午后高峰已过，最高温大概率定型；如果最高温出现在上午且当前仍在午前，后续可能还有更高温度。\n")
parts.append("   - 【关键信号】当当地时间 > 14:00 且 ST 最高温度 > 当前实时温度时，说明最高温峰值已过、后续不太可能再出现更高的温度。此时最高温大概率已经定型，可以显著增加开仓把握度，更放心地排除最高温档位附近的 NO 开仓。\n")
parts.append("9. MET 预报值不准确，不要直接用预报数值判断最终最高温；但可以参考预报中高温分布的趋势，辅助判断最高温可能出现的时间段。ST 最高温度才是判断最终最高温的可靠依据。\n")
parts.append("10. GATE-YESTERDAY 硬约束：当今天天气状况与昨天天气状况相同时，昨天最高温所在档位及其相邻正负1个步长的档位判定为 FAIL，禁止开仓 NO。理由：相似天气条件下今天最高温很可能落在与昨天相近的区间，开仓 NO 风险过高。仅当昨天最高温不在该档位范围及相邻范围内时才 PASS。当今天与昨天天气状况不同时，此约束不生效（直接 PASS）。档位步长：华氏度城市=2（如\"68-69°F\"步长2），摄氏度城市=1（如\"25°C\"步长1）。示例：昨天最高温69.1°F落在68-69°F范围内，则66-67°F、68-69°F、70-71°F三个档位均 FAIL。\n\n")

# build_analysis_prompt() - city data
parts.append("## 当前城市数据\n")
parts.append(f"- 城市: Seattle\n")
parts.append(f"- 时区: {city_tz}\n")
parts.append(f"- 当地时间: {local_time}\n")
parts.append(f"- 温度单位: {unit}\n\n")

# Operation bar params (with GATE-YESTERDAY)
parts.append("### 操作栏参数（用户设定的交易约束）\n")
parts.append(f"- BID/ASK: 只允许买入 bid 和 ask 都在 [{bid_ask_min:.3f}, {bid_ask_max:.3f}] 区间内的档位。bid/ask 不在此区间内的档位必须排除。\n")
parts.append("- OFFSET（与当地时间联动，三条独立判断，满足任一即 PASS）：YES峰值档位 = 温度档位表中 mid 值最小的档位（mid 最小 = 市场认为该档位最可能是最终最高温）。注意：必须用 mid 列判定，不要用 YES概率(价格) 列，该列可能滞后或不准确。偏移量 = (当前档位下限 - YES峰值档位下限) / 档位步长。档位步长：°F城市=2（如\"68-69°F\"下限68），°C城市=1（如\"25°C\"下限25）。示例：峰值\"66-67°F\"、当前\"68-69°F\" → 偏移量=(68-66)/2=1。\n")
parts.append("  - 条件1：当地时间 <= 13 时，偏移量 >= 3 则 PASS\n")
parts.append("  - 条件2：当地时间 > 13 时，偏移量 >= 2 则 PASS\n")
parts.append("  - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度时，偏移量 >= 1 则 PASS\n")
parts.append("  以上三条为独立判断，满足任一即 PASS，全部不满足才 FAIL。\n")
parts.append("- GATE-YESTERDAY（昨天温度硬约束）：当今天天气状况与昨天天气状况相同时，检查昨天最高温是否落在当前档位范围内或相邻正负1个步长范围内。落在范围内 -> FAIL，不在范围内 -> PASS。当今天与昨天天气状况不同时，直接 PASS。档位步长：°F城市=2，°C城市=1。示例：昨天最高温69.1°F落在68-69°F范围内，则66-67°F、68-69°F、70-71°F均 FAIL，64-65°F、72-73°F PASS。\n\n")

# ST temperature (with yesterday GATE-YESTERDAY hard constraint)
parts.append("### ST 温度（市场结算判定标准，最高优先级）\n")
parts.append("ST 温度是判定市场温度档位胜负的核心依据，格式为：最高温度/实时温度 天气状况。\n")
st_high = st.get('high')
st_current = st.get('current')
st_cond = st.get('condition', '')
parts.append(f"- ST: {st_high:.1f}{unit}/{st_current:.1f}{unit} {st_cond}\n")
parts.append(f"- 当天最高温度（ST）: {st_high:.1f}{unit} —— 这是判断最终最高温的首要依据\n")
parts.append(f"- 当前实时温度（ST）: {st_current:.1f}{unit}\n")
if st_cond:
    parts.append(f"- 天气状况: {st_cond}\n")

# Yesterday data with GATE-YESTERDAY hard constraint
parts.append(f"- 昨天最高温度: {yesterday_high:.1f}{unit}\n")
parts.append(f"- 昨天天气状况: {yesterday_condition}\n")
parts.append("- GATE-YESTERDAY 硬约束：昨天最高温落在该档位或相邻正负1个步长的档位时，该档位判定 FAIL（相似天气条件下今天最高温很可能接近昨天最高温，开仓 NO 风险过高）。昨天最高温不在该范围时判定 PASS。仅当今天与昨天天气状况相同时触发此约束。\n")
parts.append("\n")

# AWC
parts.append("### AWC 实况温度\n")
awc_max = awc.get('max')
awc_current = awc.get('current')
parts.append(f"- 当天已检测到的最高温度: {awc_max:.1f}{unit}（时间向后推移也许会出现更高的值）\n")
hourly = awc.get('hourly_temps', {})
if hourly:
    max_val = awc_max
    max_hours = [h for h, v in hourly.items() if abs(v - max_val) < 0.05]
    if max_hours:
        parts.append(f"- 最高温度出现在当地时间 {min(max_hours, key=lambda x: int(x))}:00\n")
parts.append(f"- 当前实时温度: {awc_current:.1f}{unit}\n")
if hourly:
    parts.append("- 按小时观测温度:\n")
    for h in sorted(hourly.keys(), key=lambda x: int(x)):
        parts.append(f"  - {h}:00 -> {hourly[h]:.1f}{unit}\n")
parts.append("\n")

# MET forecast
parts.append("### MET 预报温度 (10:00-17:00)\n")
for i, f_item in enumerate(met):
    hour = 10 + i
    parts.append(f"- {hour}:00 -> {f_item['temp']:.1f}{unit}\n")
parts.append("\n")

# Thresholds
parts.append("### 温度档位及价格（概率）\n")
parts.append("| 档位 | YES概率(价格) | NO概率(价格) | bid | ask | mid |\n")
parts.append("|------|---------------|-------------|-----|-----|-----|\n")
for t in snap['thresholds']:
    bid_str = f"{t['bid']:.3f}" if t.get('bid') is not None else "-"
    ask_str = f"{t['ask']:.3f}" if t.get('ask') is not None else "-"
    mid_str = f"{t['mid']:.3f}" if t.get('mid') is not None else "-"
    parts.append(f"| {t['label']} | {t['yes_price']:.3f} | {t['no_price']:.3f} | {bid_str} | {ask_str} | {mid_str} |\n")
parts.append("\n")

# Two-step output format (with GATE-YESTERDAY)
parts.append("## 输出要求（两步推理结构，严格遵守）\n\n")
parts.append("### 第一步：逐条约束检查\n")
parts.append("对每个有 ask 价格的档位，逐条列出以下检查结果：\n")
parts.append("- GATE-PRICE：bid 和 ask 是否都在 [BID_ASK_MIN, BID_ASK_MAX] 区间内？PASS 或 FAIL\n")
parts.append("- GATE-OFFSET：YES峰值档位 = mid 最小的档位（非 YES概率列）。偏移量 = (当前档位下限 - YES峰值档位下限) / 档位步长 = 具体数值。档位步长：°F城市=2（如\"68-69°F\"下限68），°C城市=1（如\"25°C\"下限25）。示例：峰值\"66-67°F\"、当前\"68-69°F\" → 偏移量=(68-66)/2=1。独立判断以下三条，满足任一即 PASS：\n")
parts.append("  - 条件1：当地时间 <= 13 且偏移量 >= 3\n")
parts.append("  - 条件2：当地时间 > 13 且偏移量 >= 2\n")
parts.append("  - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度且偏移量 >= 1\n")
parts.append("  全部不满足才 FAIL\n")
parts.append("- GATE-YESTERDAY：当今天天气状况与昨天天气状况相同时，检查昨天最高温是否落在当前档位范围内或相邻正负1个步长范围内。落在范围内 -> FAIL，不在范围内 -> PASS。档位步长：°F城市=2，°C城市=1。示例：昨天最高温69.1°F落在68-69°F范围内，则66-67°F、68-69°F、70-71°F均 FAIL，64-65°F、72-73°F PASS。当今天与昨天天气状况不同时，直接 PASS。\n\n")
parts.append("**关键规则：任何一项为 FAIL 的档位，绝对不能出现在最终 JSON 的 actions 数组中。**\n\n")
parts.append("**禁止在 JSON 字符串值中使用 LaTeX 格式（如 $\\le$、$\\ge$、$\\rightarrow$），使用纯文本（如 <=, >=, ->）。**\n\n")
parts.append("### 第二步：输出最终 JSON\n")
parts.append("在完成所有档位的逐条检查后，仅输出一个 JSON 格式的结果，不要输出多个 JSON：\n")
parts.append("```json\n")
parts.append("{\n")
parts.append('  "actions": [\n')
parts.append('    {\n')
parts.append('      "threshold_label": "档位标签如 25°C",\n')
parts.append('      "side": "NO",\n')
parts.append('      "reason": "简要说明分析理由"\n')
parts.append('    }\n')
parts.append('  ],\n')
parts.append('  "summary": "总体分析说明"\n')
parts.append("}\n")
parts.append("```\n")
parts.append("所有 GATE 检查全部 PASS 的档位才能放入 actions。如果没有档位通过所有 GATE 检查，actions 返回空数组 []，并在 summary 中说明原因。\n")
parts.append("一次可以推荐多个档位，只要每个档位都满足所有开仓条件即可。\n")

prompt = "".join(parts)

print(f"=== Round 17 Snapshot Data ===")
print(f"Local time: {local_time}")
print(f"ST: high={st_high}, current={st_current}, condition={st_cond}")
print(f"AWC: max={awc_max}, current={awc_current}")
print(f"Yesterday: high={yesterday_high}, condition={yesterday_condition}")
print(f"Today condition == Yesterday condition: {st_cond == yesterday_condition}")
print()
print(f"68-69°F threshold:")
for t in snap['thresholds']:
    if '68-69' in t['label']:
        print(f"  yes={t['yes_price']}, no={t['no_price']}, bid={t['bid']}, ask={t['ask']}, mid={t['mid']}")
print()
print(f"Prompt length: {len(prompt)} chars")
print(f"Model: {model}")
print(f"API URL: {api_url}")
print()

chat_req = {"model": model, "messages": [{"role": "user", "content": prompt}], "temperature": 0.1, "top_p": 0.8}

print("Calling Ollama API...")
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
        print("=== LLM RESPONSE ===")
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
                        print(f"  - {a.get('threshold_label')} {a.get('side')} | reason: {a.get('reason', '')[:120]}")
                else:
                    print("  EMPTY (no open)")
                print(f"\nSummary: {parsed.get('summary', '')}")
            except json.JSONDecodeError as e:
                print(f"JSON parse error: {e}")
                print(f"Raw: {raw[:500]}")
        else:
            print("No JSON found in response")
    else:
        print(f"Error: {resp.text[:1000]}")
except Exception as e:
    print(f"Request failed: {e}")
