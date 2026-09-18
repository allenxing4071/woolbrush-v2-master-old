#!/usr/bin/env bash
# Tauri dev 启动脚本 — 完全脱离 shell 的后台运行
# 用法: bash start-dev.sh

set -e

WORK_DIR="D:/DuMate/Polymarket/WoolBrush-羊毛刷-V2"
LOG_FILE="/tmp/woolbrush-v2-dev.log"
PID_FILE="/tmp/woolbrush-v2-dev.pid"

# 停止旧进程
if [ -f "$PID_FILE" ]; then
    OLD_PID=$(cat "$PID_FILE" 2>/dev/null)
    if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
        echo "Stopping old process (PID=$OLD_PID)..."
        kill "$OLD_PID" 2>/dev/null || true
        sleep 2
    fi
    rm -f "$PID_FILE"
fi

# 只杀 V2 进程，不影响 V3
pkill -f "WoolBrush-羊毛刷-V2.*tauri dev" 2>/dev/null || true
pkill -f "WoolBrush-羊毛刷-V2.*cargo run" 2>/dev/null || true
sleep 1

echo "Starting tauri dev..."
cd "$WORK_DIR"

# 使用 dev-x64.bat 初始化 MSVC x64 环境后运行 tauri dev
# disown 确保 shell 退出时不发送 SIGHUP
nohup cmd //c "dev-x64.bat" > "$LOG_FILE" 2>&1 &
NEW_PID=$!
echo "$NEW_PID" > "$PID_FILE"
disown $NEW_PID 2>/dev/null || true

echo "Started (PID=$NEW_PID), log: $LOG_FILE"
echo "Monitor with: tail -f $LOG_FILE"
