import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface SettingsForm {
  private_key: string;
  wallet_address: string;
  funder_address: string;
  proxy_url: string;
  feishu_webhook: string;
  feishu_chat_id: string;
  qianfan_api_key: string;
  qianfan_secret_key: string;
  llm_provider: string;
  bailian_api_key: string;
  bailian_model: string;
  ollama_api_key: string;
  ollama_url: string;
  ollama_model: string;
  llm_prompt: string;
}

interface SettingsDto {
  wallet_address: string;
  funder_address: string | null;
  proxy_url: string | null;
  feishu_webhook: string | null;
  feishu_chat_id: string | null;
  has_private_key: boolean;
  qianfan_api_key: string | null;
  qianfan_secret_key: string | null;
  llm_provider: string | null;
  bailian_api_key: string | null;
  bailian_model: string | null;
  ollama_api_key: string | null;
  ollama_url: string | null;
  ollama_model: string | null;
  llm_prompt: string | null;
}

interface TestStep {
  name: string;
  success: boolean;
  message: string;
  duration_ms: number;
}

interface TestResult {
  success: boolean;
  message: string;
  steps: TestStep[];
  pusd_balance: number | null;
}

interface KeyTestResult {
  key_index: number;
  key_prefix: string;
  success: boolean;
  message: string;
  duration_ms: number;
}

interface TestLlmResult {
  success: boolean;
  provider: string;
  model: string;
  message: string;
  duration_ms: number;
  key_results: KeyTestResult[];
}

interface Props {
  open: boolean;
  onClose: () => void;
}

const INITIAL_FORM: SettingsForm = {
  private_key: "",
  wallet_address: "",
  funder_address: "",
  proxy_url: "",
  feishu_webhook: "",
  feishu_chat_id: "",
  qianfan_api_key: "",
  qianfan_secret_key: "",
  llm_provider: "qianfan",
  bailian_api_key: "",
  bailian_model: "",
  ollama_api_key: "",
  ollama_url: "",
  ollama_model: "",
  llm_prompt: "",
};

const inputStyle: React.CSSProperties = {
  width: "100%",
  padding: "8px 12px",
  fontSize: "13px",
  borderRadius: "6px",
  border: "1px solid #334155",
  background: "#1e293b",
  color: "#e2e8f0",
  outline: "none",
};

const labelStyle: React.CSSProperties = {
  display: "block",
  fontSize: "12px",
  color: "#94a3b8",
  marginBottom: "6px",
  fontWeight: 600,
};

export default function SettingsModal({ open, onClose }: Props) {
  const [form, setForm] = useState<SettingsForm>({ ...INITIAL_FORM });
  const [loading, setLoading] = useState(false);
  const [testLoading, setTestLoading] = useState(false);
  const [error, setError] = useState("");
  const [savedToast, setSavedToast] = useState<{ opacity: number } | null>(null);
  const [hasSavedKey, setHasSavedKey] = useState(false);
  const [testToast, setTestToast] = useState<{ success: boolean; message: string; opacity: number } | null>(null);
  const [llmLoading, setLlmLoading] = useState(false);
  const [llmToast, setLlmToast] = useState<{
    success: boolean;
    message: string;
    opacity: number;
    keyResults?: KeyTestResult[];
  } | null>(null);
  const [activeTab, setActiveTab] = useState<"account" | "system" | "prompt" | "message">("account");
  const [promptSuffix, setPromptSuffix] = useState("");

  // 加载已保存的设置
  const loadSettings = useCallback(async () => {
    try {
      const dto = await invoke<SettingsDto>("get_settings");
      setForm((prev) => ({
        ...prev,
        // private_key 不从后端加载（安全设计），保留空值
        // 用户如需修改可重新输入，不输入则保留原值
        private_key: "",
        wallet_address: dto.wallet_address || "",
        funder_address: dto.funder_address || "",
        proxy_url: dto.proxy_url || "",
        feishu_webhook: dto.feishu_webhook || "",
        feishu_chat_id: dto.feishu_chat_id || "",
        qianfan_api_key: dto.qianfan_api_key || "",
        qianfan_secret_key: dto.qianfan_secret_key || "",
        llm_provider: dto.llm_provider || "qianfan",
        bailian_api_key: dto.bailian_api_key || "",
        bailian_model: dto.bailian_model || "",
        ollama_api_key: dto.ollama_api_key || "",
        ollama_url: dto.ollama_url || "",
        ollama_model: dto.ollama_model || "",
        llm_prompt: dto.llm_prompt || "",
      }));
      // 提示词为空时加载默认值供用户查看和编辑
      if (!dto.llm_prompt) {
        try {
          const defaultPrompt = await invoke<string>("get_default_llm_prompt");
          setForm((prev) => ({ ...prev, llm_prompt: defaultPrompt }));
        } catch {
          // 获取默认值失败时忽略，用户仍可手动输入
        }
      }
      // 标记是否已有保存的私钥
      setHasSavedKey(dto.has_private_key);
      // 加载不可编辑的附加数据模板
      try {
        const suffix = await invoke<string>("get_llm_prompt_suffix");
        setPromptSuffix(suffix);
      } catch {
        // 忽略
      }
    } catch {
      // 首次打开可能没有记录，忽略
    }
  }, []);

  useEffect(() => {
    if (open) {
      loadSettings();
      setTestToast(null);
      setError("");
      setSavedToast(null);
    }
  }, [open, loadSettings]);

  const updateField = (field: keyof SettingsForm, value: string) => {
    setForm((prev) => ({ ...prev, [field]: value }));
    setSavedToast(null);
    setError("");
  };

  const handleSave = async () => {
    setLoading(true);
    setError("");
    try {
      await invoke("save_settings", {
        form: {
          private_key: form.private_key || null,
          wallet_address: form.wallet_address,
          funder_address: form.funder_address || null,
          proxy_url: form.proxy_url || null,
          feishu_webhook: form.feishu_webhook || null,
          feishu_chat_id: form.feishu_chat_id || null,
          qianfan_api_key: form.qianfan_api_key || null,
          qianfan_secret_key: form.qianfan_secret_key || null,
          llm_provider: form.llm_provider || null,
          bailian_api_key: form.bailian_api_key || null,
          bailian_model: form.bailian_model || null,
          ollama_api_key: form.ollama_api_key || null,
          ollama_url: form.ollama_url || null,
          ollama_model: form.ollama_model || null,
          llm_prompt: form.llm_prompt || null,
        },
      });
      setSavedToast({ opacity: 1 });
      setTimeout(() => setSavedToast((t) => (t ? { ...t, opacity: 0 } : null)), 3000);
      setTimeout(() => setSavedToast(null), 5000);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleTest = async () => {
    setTestLoading(true);
    setTestToast(null);
    try {
      const result = await invoke<TestResult>("test_connection_with_settings", {
        form: {
          private_key: form.private_key || null,
          wallet_address: form.wallet_address,
          funder_address: form.funder_address || null,
          proxy_url: form.proxy_url || null,
          feishu_webhook: form.feishu_webhook || null,
          feishu_chat_id: form.feishu_chat_id || null,
          qianfan_api_key: form.qianfan_api_key || null,
          qianfan_secret_key: form.qianfan_secret_key || null,
          llm_provider: form.llm_provider || null,
          bailian_api_key: form.bailian_api_key || null,
          bailian_model: form.bailian_model || null,
          ollama_api_key: form.ollama_api_key || null,
          ollama_url: form.ollama_url || null,
          ollama_model: form.ollama_model || null,
          llm_prompt: form.llm_prompt || null,
        },
      });
      const msg = result.success
        ? `连接成功${result.pusd_balance != null ? ` | pUSD: $${result.pusd_balance.toFixed(2)}` : ""}`
        : result.message || "连接失败";
      showToast(result.success, msg);
    } catch (e) {
      showToast(false, String(e));
    } finally {
      setTestLoading(false);
    }
  };

  const showToast = (success: boolean, message: string) => {
    setTestToast({ success, message, opacity: 1 });
    // 3秒后开始渐隐，5秒后完全消失
    setTimeout(() => setTestToast((t) => (t ? { ...t, opacity: 0 } : null)), 3000);
    setTimeout(() => setTestToast(null), 5000);
  };

  const handleTestLlm = async () => {
    setLlmLoading(true);
    setLlmToast(null);
    try {
      const result = await invoke<TestLlmResult>("test_llm_connection", {
        form: {
          private_key: form.private_key || null,
          wallet_address: form.wallet_address,
          funder_address: form.funder_address || null,
          proxy_url: form.proxy_url || null,
          feishu_webhook: form.feishu_webhook || null,
          feishu_chat_id: form.feishu_chat_id || null,
          qianfan_api_key: form.qianfan_api_key || null,
          qianfan_secret_key: form.qianfan_secret_key || null,
          llm_provider: form.llm_provider || null,
          bailian_api_key: form.bailian_api_key || null,
          bailian_model: form.bailian_model || null,
          ollama_api_key: form.ollama_api_key || null,
          ollama_url: form.ollama_url || null,
          ollama_model: form.ollama_model || null,
          llm_prompt: form.llm_prompt || null,
        },
      });
      const msg = result.success
        ? `${result.provider} / ${result.model} - ${result.message} (${result.duration_ms}ms)`
        : `${result.provider} / ${result.model} - ${result.message}`;
      setLlmToast({
        success: result.success,
        message: msg,
        opacity: 1,
        keyResults: result.key_results,
      });
    } catch (e) {
      setLlmToast({ success: false, message: String(e), opacity: 1 });
    } finally {
      setLlmLoading(false);
    }
    // 5秒后开始渐隐，7秒后完全消失
    setTimeout(() => setLlmToast((t) => (t ? { ...t, opacity: 0 } : null)), 5000);
    setTimeout(() => setLlmToast(null), 7000);
  };

  if (!open) return null;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 1000,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: "rgba(0,0,0,0.7)",
        backdropFilter: "blur(4px)",
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        style={{
          width: 600,
          height: 620,
          display: "flex",
          flexDirection: "column",
          background: "#0f172a",
          border: "1px solid #334155",
          borderRadius: "12px",
          padding: "24px",
          boxShadow: "0 25px 50px rgba(0,0,0,0.5)",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            marginBottom: "20px",
          }}
        >
          <h2
            style={{
              margin: 0,
              fontSize: "18px",
              fontWeight: 700,
              color: "#e2e8f0",
            }}
          >
            设置
          </h2>
          <button
            onClick={onClose}
            style={{
              background: "none",
              border: "none",
              color: "#94a3b8",
              fontSize: "20px",
              cursor: "pointer",
              padding: "4px 8px",
              borderRadius: "4px",
            }}
            onMouseEnter={(e) =>
                (e.currentTarget.style.color = "#e2e8f0")
              }
            onMouseLeave={(e) =>
                (e.currentTarget.style.color = "#94a3b8")
              }
          >
            &times;
          </button>
        </div>

        {/* 标签页导航 */}
        <div style={{ display: "flex", gap: "4px", marginBottom: "16px", borderBottom: "1px solid #334155" }}>
          {(["account", "system", "prompt", "message"] as const).map((tab) => (
            <button
              key={tab}
              onClick={() => setActiveTab(tab)}
              style={{
                padding: "8px 16px",
                fontSize: "13px",
                fontWeight: 600,
                border: "none",
                borderBottom: activeTab === tab ? "2px solid #7c3aed" : "2px solid transparent",
                background: "transparent",
                color: activeTab === tab ? "#e2e8f0" : "#64748b",
                cursor: "pointer",
                marginBottom: "-1px",
              }}
            >
              {tab === "account" ? "账户" : tab === "system" ? "系统" : tab === "message" ? "消息" : "Prompt"}
            </button>
          ))}
        </div>

        {/* 标签页内容 */}
        <div style={{ flex: 1, overflowY: "auto", display: "flex", flexDirection: "column", gap: "14px" }}>
          {activeTab === "account" && (
            <>
              {/* Signer Address */}
              <div>
                <label style={labelStyle}>
                  签名地址
                </label>
                <input
                  type="text"
                  value={form.wallet_address}
                  onChange={(e) => updateField("wallet_address", e.target.value)}
                  placeholder="0x..."
                  style={inputStyle}
                />
              </div>

              {/* Signer Address Private Key */}
              <div>
                <label style={labelStyle}>
                  签名私钥
                </label>
                <input
                  type="password"
                  autoComplete="off"
                  value={form.private_key}
                  onChange={(e) => updateField("private_key", e.target.value)}
                  placeholder={hasSavedKey ? "\u2022".repeat(45) : ""}
                  style={inputStyle}
                />
              </div>

              {/* Funder 地址 */}
              <div>
                <label style={labelStyle}>资金地址</label>
                <input
                  type="text"
                  value={form.funder_address}
                  onChange={(e) => updateField("funder_address", e.target.value)}
                  placeholder="0x..."
                  style={inputStyle}
                />
              </div>
            </>
          )}

          {activeTab === "system" && (
            <>
              {/* 代理 URL */}
              <div>
                <label style={labelStyle}>代理地址</label>
                <input
                  type="text"
                  value={form.proxy_url}
                  onChange={(e) => updateField("proxy_url", e.target.value)}
                  placeholder="http://127.0.0.1:15236"
                  style={inputStyle}
                />
              </div>

              {/* LLM Provider 选择器 */}
              <div>
                <label style={labelStyle}>LLM 服务商</label>
                <div style={{ display: "flex", gap: "8px" }}>
                  {(["qianfan", "bailian", "ollama"] as const).map((p) => (
                    <button
                      key={p}
                      onClick={() => updateField("llm_provider", p)}
                      style={{
                        padding: "6px 14px",
                        fontSize: "13px",
                        fontWeight: 600,
                        borderRadius: "6px",
                        border: `1px solid ${form.llm_provider === p ? "#7c3aed" : "#334155"}`,
                        background: form.llm_provider === p ? "#2e1065" : "#1e293b",
                        color: form.llm_provider === p ? "#c4b5fd" : "#64748b",
                        cursor: "pointer",
                      }}
                    >
                      {p === "qianfan" ? "千帆" : p === "bailian" ? "百炼" : "Ollama"}
                    </button>
                  ))}
                </div>
              </div>

              {/* 千帆 API Key（provider=qianfan 时显示） */}
              {form.llm_provider === "qianfan" && (
                <div>
                  <label style={labelStyle}>千帆 API Key</label>
                  <textarea
                    value={form.qianfan_api_key}
                    onChange={(e) => updateField("qianfan_api_key", e.target.value)}
                    placeholder={"sk-...\nsk-...\n（每行一个 Key，轮询调用）"}
                    style={{
                      ...inputStyle,
                      minHeight: "60px",
                      maxHeight: "120px",
                      resize: "vertical",
                      fontFamily: "monospace",
                      lineHeight: "1.5",
                    }}
                  />
                  <div style={{ fontSize: "11px", color: "#64748b", marginTop: "4px" }}>
                    支持多个 Key，每行一个，按轮询调用
                  </div>
                </div>
              )}

              {/* 百炼 API Key + 模型（provider=bailian 时显示） */}
              {form.llm_provider === "bailian" && (
                <>
                  <div>
                    <label style={labelStyle}>百炼 API Key</label>
                    <textarea
                      value={form.bailian_api_key}
                      onChange={(e) => updateField("bailian_api_key", e.target.value)}
                      placeholder={"sk-...\nsk-...\n（每行一个 Key，轮询调用）"}
                      style={{
                        ...inputStyle,
                        minHeight: "60px",
                        maxHeight: "120px",
                        resize: "vertical",
                        fontFamily: "monospace",
                        lineHeight: "1.5",
                      }}
                    />
                    <div style={{ fontSize: "11px", color: "#64748b", marginTop: "4px" }}>
                      支持多个 Key，每行一个，按轮询调用
                    </div>
                  </div>
                  <div>
                    <label style={labelStyle}>Bailian 模型名</label>
                    <input
                      type="text"
                      value={form.bailian_model}
                      onChange={(e) => updateField("bailian_model", e.target.value)}
                      placeholder="qwen-plus"
                      style={inputStyle}
                    />
                  </div>
                </>
              )}

              {/* Ollama API Key + URL + 模型（provider=ollama 时显示） */}
              {form.llm_provider === "ollama" && (
                <>
                  <div>
                    <label style={labelStyle}>Ollama API 密钥</label>
                    <textarea
                      value={form.ollama_api_key}
                      onChange={(e) => updateField("ollama_api_key", e.target.value)}
                      placeholder={"在 ollama.com -> Settings -> API Keys 获取\n每行一个 Key，轮询调用"}
                      style={{
                        ...inputStyle,
                        minHeight: "60px",
                        maxHeight: "120px",
                        resize: "vertical",
                        fontFamily: "monospace",
                        lineHeight: "1.5",
                      }}
                    />
                    <div style={{ fontSize: "11px", color: "#64748b", marginTop: "4px" }}>
                      支持多个 Key，每行一个，按轮询调用
                    </div>
                  </div>
                  <div>
                    <label style={labelStyle}>Ollama 接口地址</label>
                    <input
                      type="text"
                      value={form.ollama_url}
                      onChange={(e) => updateField("ollama_url", e.target.value)}
                      placeholder="https://ollama.com/v1/chat/completions"
                      style={inputStyle}
                    />
                  </div>
                  <div>
                    <label style={labelStyle}>Ollama 模型名</label>
                    <input
                      type="text"
                      value={form.ollama_model}
                      onChange={(e) => updateField("ollama_model", e.target.value)}
                      placeholder="qwen2.5:14b"
                      style={inputStyle}
                    />
                  </div>
                </>
              )}

              {/* 大模型连通性测试按钮 */}
              <div style={{ display: "flex", alignItems: "center", gap: "10px" }}>
                <button
                  onClick={handleTestLlm}
                  disabled={llmLoading}
                  style={{
                    padding: "6px 14px",
                    fontSize: "13px",
                    fontWeight: 600,
                    borderRadius: "6px",
                    border: "1px solid #334155",
                    background: "#1e293b",
                    color: "#e2e8f0",
                    cursor: llmLoading ? "not-allowed" : "pointer",
                    opacity: llmLoading ? 0.6 : 1,
                  }}
                >
                  {llmLoading ? "测试中..." : "测试 LLM 连接"}
                </button>
              </div>

              {/* LLM 测试结果 toast */}
              {llmToast && (
                <div
                  style={{
                    padding: "10px 12px",
                    borderRadius: "6px",
                    fontSize: "12px",
                    fontWeight: 600,
                    border: `1px solid ${llmToast.success ? "#166534" : "#7f1d1d"}`,
                    background: llmToast.success ? "#052e16" : "#450a0a",
                    color: llmToast.success ? "#86efac" : "#fca5a5",
                    opacity: llmToast.opacity,
                    transition: "opacity 2s ease-in-out",
                  }}
                >
                  {llmToast.success ? "\u2713 " : "\u2717 "}
                  {llmToast.message}
                  {llmToast.keyResults && llmToast.keyResults.length > 1 && (
                    <div style={{ marginTop: "8px", fontWeight: 400 }}>
                      {llmToast.keyResults.map((kr) => (
                        <div
                          key={kr.key_index}
                          style={{
                            display: "flex",
                            alignItems: "flex-start",
                            gap: "6px",
                            padding: "2px 0",
                          }}
                        >
                          <span style={{ flexShrink: 0 }}>
                            {kr.success ? "\u2713" : "\u2717"}
                          </span>
                          <span style={{ color: kr.success ? "#86efac" : "#fca5a5" }}>
                            密钥 {kr.key_index} ({kr.key_prefix}...): {kr.message}
                          </span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </>
          )}

          {activeTab === "prompt" && (
            <>
              <div>
                <label style={labelStyle}>LLM Prompt</label>
                <textarea
                  value={form.llm_prompt}
                  onChange={(e) => updateField("llm_prompt", e.target.value)}
                  placeholder="Leave empty to use default prompt"
                  style={{
                    ...inputStyle,
                    minHeight: "300px",
                    resize: "vertical",
                    fontFamily: "monospace",
                    lineHeight: "1.5",
                    whiteSpace: "pre-wrap",
                  }}
                />
              </div>
              {promptSuffix && (
                <div style={{ marginTop: "16px" }}>
                  <label style={{ ...labelStyle, color: "#64748b" }}>
                    Auto-appended Data (read-only)
                  </label>
                  <pre
                    style={{
                      ...inputStyle,
                      minHeight: "200px",
                      maxHeight: "300px",
                      overflow: "auto",
                      resize: "vertical",
                      fontFamily: "monospace",
                      fontSize: "12px",
                      lineHeight: "1.5",
                      whiteSpace: "pre-wrap",
                      color: "#94a3b8",
                      background: "#0f172a",
                      borderColor: "#1e293b",
                      cursor: "default",
                    }}
                  >
                    {promptSuffix}
                  </pre>
                </div>
              )}
            </>
          )}

          {activeTab === "message" && (
            <>
              {/* 飞书 Webhook */}
              <div>
                <label style={labelStyle}>飞书 Webhook</label>
                <input
                  type="text"
                  value={form.feishu_webhook}
                  onChange={(e) => updateField("feishu_webhook", e.target.value)}
                  placeholder="https://open.feishu.cn/open-apis/bot/v2/hook/..."
                  style={inputStyle}
                />
              </div>

              {/* 飞书 Chat ID */}
              <div>
                <label style={labelStyle}>飞书群 ID</label>
                <input
                  type="text"
                  value={form.feishu_chat_id}
                  onChange={(e) => updateField("feishu_chat_id", e.target.value)}
                  placeholder="oc_xxxxxxxx"
                  style={inputStyle}
                />
              </div>
            </>
          )}

          {/* 测试结果 toast */}
          {testToast && (
            <div
              style={{
                padding: "10px 12px",
                borderRadius: "6px",
                fontSize: "13px",
                fontWeight: 600,
                border: `1px solid ${testToast.success ? "#166534" : "#7f1d1d"}`,
                background: testToast.success ? "#052e16" : "#450a0a",
                color: testToast.success ? "#86efac" : "#fca5a5",
                opacity: testToast.opacity,
                transition: "opacity 2s ease-in-out",
              }}
            >
              {testToast.success ? "\u2713 " : "\u2717 "}
              {testToast.message}
            </div>
          )}

          {/* 错误提示 */}
          {error && (
            <div
              style={{
                padding: "10px 12px",
                borderRadius: "6px",
                fontSize: "13px",
                border: "1px solid #7f1d1d",
                background: "#450a0a",
                color: "#fca5a5",
              }}
            >
              {error}
            </div>
          )}

          {/* 保存成功 toast */}
          {savedToast && (
            <div
              style={{
                padding: "10px 12px",
                borderRadius: "6px",
                fontSize: "13px",
                fontWeight: 600,
                border: "1px solid #166534",
                background: "#052e16",
                color: "#86efac",
                opacity: savedToast.opacity,
                transition: "opacity 2s ease-in-out",
              }}
            >
              {"\u2713 "}设置保存成功
            </div>
          )}
        </div>

        {/* 按钮区域 */}
        <div
          style={{
              display: "flex",
              gap: "10px",
              marginTop: "10px",
              justifyContent: "flex-end",
              flexShrink: 0,
            }}
          >
            {activeTab === "account" && (
            <button
              onClick={handleTest}
              disabled={testLoading}
              style={{
                padding: "8px 16px",
                fontSize: "13px",
                borderRadius: "6px",
                border: "1px solid #334155",
                background: "#1e293b",
                color: "#e2e8f0",
                cursor: testLoading ? "not-allowed" : "pointer",
                opacity: testLoading ? 0.6 : 1,
                fontWeight: 600,
              }}
            >
              {testLoading ? "测试连接中..." : "测试连接"}
            </button>
            )}
            <button
              onClick={onClose}
              style={{
                padding: "8px 16px",
                fontSize: "13px",
                borderRadius: "6px",
                border: "1px solid #334155",
                background: "transparent",
                color: "#94a3b8",
                cursor: "pointer",
                fontWeight: 600,
              }}
            >
              取消
            </button>
            <button
              onClick={handleSave}
              disabled={loading}
              style={{
                padding: "8px 16px",
                fontSize: "13px",
                borderRadius: "6px",
                border: "none",
                background: "linear-gradient(90deg,#7c3aed,#ec4899)",
                color: "#fff",
                cursor: loading ? "not-allowed" : "pointer",
                opacity: loading ? 0.6 : 1,
                fontWeight: 600,
              }}
            >
              {loading ? "保存中..." : "保存"}
            </button>
          </div>
      </div>
    </div>
  );
}
