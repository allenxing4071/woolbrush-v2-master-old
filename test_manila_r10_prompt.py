#!/usr/bin/env python
"""
Test: Manila R10 snapshot with UPDATED prompt (YES peak = mid min, not yes_price).
Send to Ollama gemma4:31b.
"""

import json, requests, re, sys

# ── 1. Load R10 Manila snapshot from JSONL ──
jsonl_path = 'C:/Users/liguo/.qianfan/workspace/e20961114c64449db9bf1a4a67dbf4ca/.dumate/inbox/2026-09-03(1).jsonl'
snap = None
with open(jsonl_path, 'r', encoding='utf-8') as f:
    for line in f:
        rec = json.loads(line)
        if rec.get('city') == 'manila' and rec.get('round') == 10 and rec.get('event') == 'city_snapshot':
            snap = rec['data']
            break

if snap is None:
    print("ERROR: Manila R10 snapshot not found")
    sys.exit(1)

unit = '\u00b0C'
bid_ask_min = float(snap['toolbar']['askMin'])
bid_ask_max = float(snap['toolbar']['askMax'])
st = snap['st']
awc = snap['awc']
met = snap['met_forecast']
local_time = snap.get('local_time', '15:00')
city_tz = snap.get('city_tz', 'Asia/Manila')
thresholds = snap['thresholds']
positions = snap.get('positions', [])

held_labels = []
for pos in positions:
    if pos.get('strategy') == 'LLM' or pos.get('pinned'):
        held_labels.append(pos.get('threshold', ''))

# ── 2. Build prompt with UPDATED text (YES peak = mid min) ──

DEFAULT_PROMPT = r"""你是一个 Polymarket 气温市场的交易分析师。

## 核心原则：保本第一
保本是最高优先级。只允许开仓最有把握的档位——即 NO概率极高、几乎不可能成为最高温的档位。
如果对某个档位没有很大的把握，就不要开仓。宁可错过机会，也绝不冒亏损风险。
没有合适机会时，should_open 返回 false。

## 任务
分析以下城市气温市场数据，判断是否应该开仓，以及开仓的档位和方向。

## 市场规则
- 每个城市每天有一个「最高温」市场，包含多个温度档位（如 20°C, 21°C, ... 30°C or higher）。
- 最终结算时，只有一个档位的 YES=1（实际最高温命中该档位），其余所有档位 NO=1。
- 温度档位表中的 YES价格 和 NO价格 就是各自命中的概率（YES价格 + NO价格 = 1）。
- YES价格越高 = 该档位越可能是最高温 = NO价格越低 = NO大概率不会结算为1。
- YES价格越低 = 该档位越不可能是最高温 = NO价格越高 = NO大概率结算为1。

## ST 温度——市场结算的判定标准
ST 温度是判定市场温度档位胜负的核心标准和最终依据，必须高度重视。
ST 温度格式为：最高温度/实时温度 天气状况（如 "35/32°C scattered"）。
ST 温度的最高温度反映当天已观测到的最高气温，实时温度反映当前实际气温，天气状况反映当前气象条件。
在判断哪个温度档位会成为最终最高温时，必须以 ST 温度的最高温度为主要依据，而非 AWC 或 MET 数据。
AWC 实况和 MET 预报仅作为辅助参考，当 ST 温度与 AWC/MET 存在差异时，以 ST 温度为准。

## 开仓条件
1. 只考虑 NO 侧开仓（买入 NO token）。
2. 必须选择 NO价格（NO概率）高的档位——这意味着该档位大概率不是最高温，NO结算为1的概率大。
3. NO价格（NO概率）低的档位绝对不能买入——这说明市场认为该档位很可能是最高温，买入NO大概率亏损。
4. ST 温度的最高温度是判断最终最高温的首要依据；AWC 实况温度可作为交叉验证；MET 预报值不够准确，不要直接用其数值判断最高温，但可以参考其预示的最高温出现时间段。
5. 如果所有档位的 NO价格都很低（市场已将最高温锁定在很窄范围），说明没有安全的NO开仓机会，不要开仓。
6. 如果市场尚未充分定价（大部分档位无价格数据），不要开仓。
7. 必须遵守操作栏参数约束：
   - 档位的 bid 和 ask 必须都在 BID/ASK 区间内，否则排除该档位。
   - OFFSET 约束（与当地时间联动，三条独立判断，满足任一即 PASS）：YES峰值档位 = 温度档位表中 mid 值最小的档位（mid 最小 = 市场认为该档位最可能是最终最高温）。注意：必须用 mid 列判定，不要用 YES概率(价格) 列，该列可能滞后或不准确。偏移量 = (当前档位温度 - YES峰值档位温度) / 档位步长。
     - 条件1：当地时间 <= 13 时，偏移量 >= 3 则 PASS
     - 条件2：当地时间 > 13 时，偏移量 >= 2 则 PASS
     - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度时，偏移量 >= 1 则 PASS
     以上三条为独立判断，满足任一即 PASS，全部不满足才 FAIL。
8. 必须结合时间因素判断最高温是否已经定型：
   - 日最高温通常出现在 14:00-16:00。如果当前当地时间已过 17:00，当日最高温大概率已确定，不再会升得更高，此时可以更有信心地排除该最高温档位附近的开仓。
   - 如果当前当地时间在 10:00-16:00 之间，温度仍可能继续上升，已观测到的最高温未必是最终最高温。此时需要参考 MET 预报中剩余时段是否有更高温度，判断最终最高温可能落在哪个档位。
   - 如果当前当地时间在 10:00 之前，当日最高温远未确定，预报不确定性最大，应更加谨慎，优先选择离预报峰值很远的档位开仓 NO。
   - 关注最高温已观测到的时间点：如果最高温出现在 15:00 且当前已过 17:00，说明午后高峰已过，最高温大概率定型；如果最高温出现在上午且当前仍在午前，后续可能还有更高温度。
   - 【关键信号】当当地时间 > 14:00 且 ST 最高温度 > 当前实时温度时，说明最高温峰值已过、后续不太可能再出现更高的温度。此时最高温大概率已经定型，可以显著增加开仓把握度，更放心地排除最高温档位附近的 NO 开仓。
9. MET 预报值不准确，不要直接用预报数值判断最终最高温；但可以参考预报中高温分布的趋势，辅助判断最高温可能出现的时间段。ST 最高温度才是判断最终最高温的可靠依据。
"""

prompt = []
prompt.append(DEFAULT_PROMPT)
prompt.append('\n')

# Held thresholds
if held_labels:
    prompt.append('## 已持仓档位（无需再分析，直接跳过）\n')
    for label in held_labels:
        prompt.append(f'- {label}（已开仓）\n')
    prompt.append('以上档位已经开仓，不要重复推荐。请分析其余未持仓的档位是否也值得开仓。\n\n')

# City data
prompt.append('## 当前城市数据\n')
prompt.append(f'- 城市: manila\n')
prompt.append(f'- 时区: {city_tz}\n')
prompt.append(f'- 当地时间: {local_time}\n')
prompt.append(f'- 温度单位: {unit}\n\n')

# Operation bar params (UPDATED with YES peak = mid min)
prompt.append('### 操作栏参数（用户设定的交易约束）\n')
prompt.append(f'- BID/ASK: 只允许买入 bid 和 ask 都在 [{bid_ask_min:.3f}, {bid_ask_max:.3f}] 区间内的档位。bid/ask 不在此区间内的档位必须排除。\n')
prompt.append('- OFFSET（与当地时间联动，三条独立判断，满足任一即 PASS）：YES峰值档位 = 温度档位表中 mid 值最小的档位（mid 最小 = 市场认为该档位最可能是最终最高温）。注意：必须用 mid 列判定，不要用 YES概率(价格) 列，该列可能滞后或不准确。偏移量 = (当前档位温度 - YES峰值档位温度) / 档位步长。\n')
prompt.append('  - 条件1：当地时间 <= 13 时，偏移量 >= 3 则 PASS\n')
prompt.append('  - 条件2：当地时间 > 13 时，偏移量 >= 2 则 PASS\n')
prompt.append('  - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度时，偏移量 >= 1 则 PASS\n')
prompt.append('  以上三条为独立判断，满足任一即 PASS，全部不满足才 FAIL。\n\n')

# ST temperature
prompt.append('### ST 温度（市场结算判定标准，最高优先级）\n')
prompt.append('ST 温度是判定市场温度档位胜负的核心依据，格式为：最高温度/实时温度 天气状况。\n')
high_str = f"{st['high']:.1f}{unit}" if st.get('high') is not None else "--"
current_str = f"{st['current']:.1f}{unit}" if st.get('current') is not None else "--"
cond_str = st.get('condition', '--')
prompt.append(f'- ST: {high_str}/{current_str} {cond_str}\n')
if st.get('high') is not None:
    prompt.append(f'- 当天最高温度（ST）: {st["high"]:.1f}{unit} —— 这是判断最终最高温的首要依据\n')
if st.get('current') is not None:
    prompt.append(f'- 当前实时温度（ST）: {st["current"]:.1f}{unit}\n')
if st.get('condition'):
    prompt.append(f'- 天气状况: {st["condition"]}\n')
prompt.append('\n')

# AWC data
prompt.append('### AWC 实况温度\n')
if awc.get('max') is not None:
    prompt.append(f'- 当天已检测到的最高温度: {awc["max"]:.1f}{unit}（时间向后推移也许会出现更高的值）\n')
    hourly = awc.get('hourly_temps', {})
    if hourly:
        max_val = awc['max']
        max_hours = [int(h) for h, v in hourly.items() if abs(v - max_val) < 0.05]
        if max_hours:
            prompt.append(f'- 最高温度出现在当地时间 {min(max_hours)}:00\n')
if awc.get('current') is not None:
    prompt.append(f'- 当前实时温度: {awc["current"]:.1f}{unit}\n')
hourly = awc.get('hourly_temps', {})
if hourly:
    prompt.append('- 按小时观测温度:\n')
    for h in sorted(hourly.keys(), key=lambda x: int(x)):
        prompt.append(f'  - {h}:00 -> {hourly[h]:.1f}{unit}\n')
prompt.append('\n')

# MET forecast
prompt.append('### MET 预报温度 (10:00-17:00)\n')
for i, f_item in enumerate(met):
    hour = 10 + i
    prompt.append(f'- {hour}:00 -> {f_item["temp"]:.1f}{unit}\n')
prompt.append('\n')

# Threshold table
prompt.append('### 温度档位及价格（概率）\n')
prompt.append('| 档位 | YES概率(价格) | NO概率(价格) | bid | ask | mid |\n')
prompt.append('|------|---------------|-------------|-----|-----|-----|\n')
for t in thresholds:
    bid_str = f"{t['bid']:.3f}" if t.get('bid') is not None else "-"
    ask_str = f"{t['ask']:.3f}" if t.get('ask') is not None else "-"
    mid_str = f"{t['mid']:.3f}" if t.get('mid') is not None else "-"
    prompt.append(f"| {t['label']} | {t['yes_price']:.3f} | {t['no_price']:.3f} | {bid_str} | {ask_str} | {mid_str} |\n")
prompt.append('\n')

# Two-step reasoning output format (UPDATED with YES peak = mid min)
prompt.append('## 输出要求（两步推理结构，严格遵守）\n\n')
prompt.append('### 第一步：逐条约束检查\n')
prompt.append('对每个有 ask 价格的档位，逐条列出以下检查结果：\n')
prompt.append('- GATE-PRICE：bid 和 ask 是否都在 [BID_ASK_MIN, BID_ASK_MAX] 区间内？PASS 或 FAIL\n')
prompt.append('- GATE-OFFSET：YES峰值档位 = mid 最小的档位（非 YES概率列）。偏移量 = (档位温度 - YES峰值档位温度) / 步长 = 具体数值。独立判断以下三条，满足任一即 PASS：\n')
prompt.append('  - 条件1：当地时间 <= 13 且偏移量 >= 3\n')
prompt.append('  - 条件2：当地时间 > 13 且偏移量 >= 2\n')
prompt.append('  - 条件3：当地时间 > 16 且 ST 实时温度 < ST 最高温度且偏移量 >= 1\n')
prompt.append('  全部不满足才 FAIL\n\n')
prompt.append('**关键规则：任何一项为 FAIL 的档位，绝对不能出现在最终 JSON 的 actions 数组中。**\n\n')
prompt.append('### 第二步：输出最终 JSON\n')
prompt.append('在完成所有档位的逐条检查后，仅输出一个 JSON 格式的结果，不要输出多个 JSON：\n')
prompt.append('```json\n')
prompt.append('{\n')
prompt.append('  "actions": [\n')
prompt.append('    {\n')
prompt.append('      "threshold_label": "档位标签如 25°C",\n')
prompt.append('      "side": "NO",\n')
prompt.append('      "reason": "简要说明分析理由"\n')
prompt.append('    }\n')
prompt.append('  ],\n')
prompt.append('  "summary": "总体分析说明"\n')
prompt.append('}\n')
prompt.append('```\n')
prompt.append('所有 GATE 检查全部 PASS 的档位才能放入 actions。如果没有档位通过所有 GATE 检查，actions 返回空数组 []，并在 summary 中说明原因。\n')
prompt.append('一次可以推荐多个档位，只要每个档位都满足所有开仓条件即可。\n')

full_prompt = ''.join(prompt)

# ── 3. Print debug info ──
print(f'Prompt length: {len(full_prompt)} chars')
print(f'Model: gemma4:31b')
print(f'API URL: https://ollama.com/v1/chat/completions')
print(f'Held thresholds: {held_labels if held_labels else "none"}')
print(f'BID/ASK range: [{bid_ask_min:.3f}, {bid_ask_max:.3f}]')
print(f'ST: high={st.get("high")} current={st.get("current")} cond={st.get("condition")}')
print(f'MET peak: {max(f["temp"] for f in met):.1f}C')

# Verify YES peak by mid
mid_vals = [(t['label'], t.get('mid'), t.get('yes_price')) for t in thresholds if t.get('mid') is not None]
mid_min = min(mid_vals, key=lambda x: x[1])
yes_max = max(mid_vals, key=lambda x: x[2])
print(f'YES peak by mid (CORRECT): {mid_min[0]} (mid={mid_min[1]:.3f})')
print(f'YES peak by yes_price (WRONG): {yes_max[0]} (yes_price={yes_max[2]:.3f})')
print()

# ── 4. Call Ollama API ──
chat_req = {
    "model": "gemma4:31b",
    "messages": [{"role": "user", "content": full_prompt}],
    "temperature": 0.1,
    "top_p": 0.8,
}

api_key = '871c65e6260647b4801ddf3380e5c9f4.JVTpxA8Ajp8H-Vk97qxweFFE'
api_url = 'https://ollama.com/v1/chat/completions'

print("Calling Ollama API...")
try:
    resp = requests.post(
        api_url,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        json=chat_req,
        timeout=120,
    )
    print(f'HTTP Status: {resp.status_code}')
    print()
    if resp.status_code == 200:
        data = resp.json()
        content = data["choices"][0]["message"]["content"]
        print("=" * 60)
        print("=== LLM RESPONSE (gemma4:31b, UPDATED prompt) ===")
        print("=" * 60)
        print(content)
        print("=" * 60)
        print()

        # Extract JSON
        json_blocks = re.findall(r'\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}', content)
        print(f"Found {len(json_blocks)} JSON block(s) in response")

        # Try parse the main JSON
        json_match = re.search(r'```json\s*(\{.*?\})\s*```', content, re.DOTALL)
        if json_match:
            try:
                result = json.loads(json_match.group(1))
                actions = result.get('actions', [])
                print(f"Actions: {len(actions)} item(s)")
                for a in actions:
                    print(f"  - {a.get('threshold_label')} {a.get('side')}: {a.get('reason','')[:80]}")
                print(f"Summary: {result.get('summary', '')[:200]}")
            except json.JSONDecodeError as e:
                print(f"JSON parse error: {e}")
        else:
            print("No ```json``` block found, trying raw extraction...")
    else:
        print(f"Error response: {resp.text[:500]}")
except Exception as e:
    print(f"Request failed: {e}")
