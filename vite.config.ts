import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf-8"));
// 构建时间（北京时间，MMDD-HHmm），用于区分同一版本号的不同构建
const buildTime = new Intl.DateTimeFormat("zh-CN", {
  timeZone: "Asia/Shanghai", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
}).formatToParts(new Date()).reduce<Record<string, string>>((acc, p) => ({ ...acc, [p.type]: p.value }), {});

// @tauri-apps/cli 自动注入 tauri 相关配置
export default defineConfig({
  plugins: [react()],
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
    __BUILD_TIME__: JSON.stringify(`${buildTime.month}${buildTime.day}-${buildTime.hour}${buildTime.minute}`),
  },
  clearScreen: false,
  server: {
    port: 1685,
    strictPort: true,
    watch: {
      // 不监听 Rust 源码变化
      ignored: ["**/src-tauri/**"],
    },
  },
});
