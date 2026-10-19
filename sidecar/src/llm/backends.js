// 模型后端：OpenAI 兼容 + Ollama 两套协议，统一输出 ChatChunk 语义。
//
// 设计对齐 Rust 侧 `llm::ChatChunk`：
//   { type: 'text', text }
//   { type: 'tool_call', call }
//   { type: 'done', reason, usage }
//   { type: 'error', message }
//
// 每个后端暴露 `stream(model, payload) -> AsyncGenerator<Chunk>`，
// payload 为 OpenAI 兼容消息体，Ollama 后端在内部做字段转换。
// 协议差异被隔离在这里，上层（router / agent）只见统一接口。

export const DEFAULT_TIMEOUT_MS = 60_000;

/**
 * 统一用量。token 计量只用于观测，不该影响生成——拿不到就按 0。
 */
export class TokenUsage {
  constructor({ promptTokens = 0, completionTokens = 0, promptNanos = 0, evalNanos = 0 }) {
    this.promptTokens = promptTokens;
    this.completionTokens = completionTokens;
    this.promptNanos = promptNanos;
    this.evalNanos = evalNanos;
  }

  static zero() {
    return new TokenUsage({});
  }

  get isMeasured() {
    return this.promptTokens > 0 || this.completionTokens > 0;
  }

  merge(other) {
    this.promptTokens += other.promptTokens;
    this.completionTokens += other.completionTokens;
    this.promptNanos += other.promptNanos;
    this.evalNanos += other.evalNanos;
    return this;
  }

  logLine() {
    const promptSecs = this.promptNanos / 1e9;
    const evalSecs = this.evalNanos / 1e9;
    const rate = evalSecs > 0.01 ? (this.completionTokens / evalSecs).toFixed(1) : '—';
    return `提示词 ${this.promptTokens} token（${promptSecs.toFixed(1)}s）｜输出 ${this.completionTokens} token（${evalSecs.toFixed(1)}s，${rate} tok/s）`;
  }
}

/**
 * 从 OpenAI 兼容 usage 对象归一化。
 * 兼容 Ollama 的 prompt_eval_count/eval_count 与 OpenAI 的 prompt_tokens/completion_tokens。
 */
export function normalizeUsage(raw) {
  if (!raw || typeof raw !== 'object') return TokenUsage.zero();
  return new TokenUsage({
    promptTokens: Number(raw.prompt_tokens ?? raw.promptTokens ?? raw.prompt_eval_count ?? 0),
    completionTokens: Number(raw.completion_tokens ?? raw.completionTokens ?? raw.eval_count ?? 0),
    promptNanos: Number(raw.prompt_tokens_details?.duration_ns ?? raw.prompt_eval_duration ?? 0),
    evalNanos: Number(raw.completion_tokens_details?.duration_ns ?? raw.eval_duration ?? 0),
  });
}

/**
 * 解析 SSE 字节流为 JSON 行。
 * 处理跨 chunk 的不完整行与 `data: [DONE]` 终止标记。
 */
export class SseDecoder {
  #pending = '';

  /**
   * @param {string} text 增量文本
   * @returns {Array<{json: object} | {done: true}>}
   */
  push(text) {
    this.#pending += text;
    const out = [];
    while (true) {
      const nl = this.#pending.indexOf('\n');
      if (nl < 0) break;
      const line = this.#pending.slice(0, nl).trim();
      this.#pending = this.#pending.slice(nl + 1);
      if (!line) continue;
      if (line === 'data: [DONE]') {
        out.push({ done: true });
        continue;
      }
      const json = line.startsWith('data: ') ? line.slice(6) : line;
      try {
        out.push({ json: JSON.parse(json) });
      } catch {
        // 心跳包 / 不完整行，跳过。
      }
    }
    return out;
  }
}

/**
 * OpenAI 兼容后端。baseUrl 形如 `https://api.deepseek.com`，补 `/chat/completions`。
 */
export class OpenAiCompatBackend {
  constructor({ baseUrl, apiKey, timeoutMs = DEFAULT_TIMEOUT_MS }) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.apiKey = apiKey;
    this.timeoutMs = timeoutMs;
  }

  endpoint() {
    return `${this.baseUrl}/chat/completions`;
  }

  /**
   * @param {string} model
   * @param {object} payload { messages, tools?, temperature, extraOptions? }
   */
  async *stream(model, payload) {
    const body = {
      model,
      messages: payload.messages,
      stream: true,
      temperature: payload.temperature ?? 0.7,
      stream_options: { include_usage: true },
      ...(payload.tools?.length ? { tools: payload.tools } : {}),
      ...(payload.extraOptions ?? {}),
    };

    const headers = { 'Content-Type': 'application/json' };
    if (this.apiKey) headers.Authorization = `Bearer ${this.apiKey}`;

    const resp = await fetch(this.endpoint(), {
      method: 'POST',
      headers,
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });

    if (!resp.ok) {
      const text = await resp.text().catch(() => '');
      throw new Error(`provider ${this.baseUrl} returned ${resp.status}: ${text.slice(0, 500)}`);
    }

    const decoder = new SseDecoder();
    const reader = resp.body.getReader();
    const textDecoder = new TextDecoder();
    let usage = TokenUsage.zero();

    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      for (const ev of decoder.push(textDecoder.decode(value, { stream: true }))) {
        if (ev.done) continue;
        const chunk = ev.json;
        if (chunk.usage) usage = normalizeUsage(chunk.usage);
        const choice = chunk.choices?.[0];
        if (!choice) continue;
        const delta = choice.delta ?? {};
        if (delta.content) yield { type: 'text', text: delta.content };
        if (delta.tool_calls?.length) {
          for (const tc of delta.tool_calls) {
            yield { type: 'tool_call', call: { id: tc.id, function: tc.function } };
          }
        }
        if (choice.finish_reason) {
          yield { type: 'done', reason: choice.finish_reason, usage };
        }
      }
    }
    yield { type: 'done', reason: 'stop', usage };
  }

  /** 健康探针：列模型接口，任何 2xx 即视为可达。 */
  async healthCheck() {
    const headers = this.apiKey ? { Authorization: `Bearer ${this.apiKey}` } : {};
    const resp = await fetch(`${this.baseUrl}/models`, {
      headers,
      signal: AbortSignal.timeout(5000),
    });
    return { ok: resp.ok, status: resp.status };
  }
}

/**
 * Ollama 后端。baseUrl 形如 `http://localhost:11434`，走原生 `/api/chat`。
 * 消息体已是 OpenAI 兼容格式，Ollama 原生接口也接受该格式。
 */
export class OllamaBackend {
  constructor({ baseUrl, timeoutMs = DEFAULT_TIMEOUT_MS }) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.timeoutMs = timeoutMs;
  }

  endpoint() {
    return `${this.baseUrl}/api/chat`;
  }

  async *stream(model, payload) {
    const body = {
      model,
      messages: payload.messages,
      stream: true,
      keep_alive: '30m',
      think: false,
      options: { temperature: payload.temperature ?? 0.7, ...(payload.extraOptions ?? {}) },
      ...(payload.tools?.length ? { tools: payload.tools } : {}),
    };

    const resp = await fetch(this.endpoint(), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });

    if (!resp.ok) {
      const text = await resp.text().catch(() => '');
      throw new Error(`ollama ${this.baseUrl} returned ${resp.status}: ${text.slice(0, 500)}`);
    }

    const decoder = new SseDecoder();
    const reader = resp.body.getReader();
    const textDecoder = new TextDecoder();
    let lastUsage = TokenUsage.zero();
    let lastReason = 'stop';

    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      for (const ev of decoder.push(textDecoder.decode(value, { stream: true }))) {
        if (ev.done) continue;
        const chunk = ev.json;
        if (chunk.done_reason) lastReason = chunk.done_reason;
        if (chunk.done) {
          lastUsage = normalizeUsage({
            prompt_eval_count: chunk.prompt_eval_count,
            eval_count: chunk.eval_count,
            prompt_eval_duration: chunk.prompt_eval_duration,
            eval_duration: chunk.eval_duration,
          });
          continue;
        }
        const msg = chunk.message;
        if (!msg) continue;
        if (msg.tool_calls?.length) {
          for (const tc of msg.tool_calls) {
            yield { type: 'tool_call', call: { id: tc.id, function: tc.function } };
          }
        }
        if (msg.content) yield { type: 'text', text: msg.content };
      }
    }
    yield { type: 'done', reason: lastReason, usage: lastUsage };
  }

  async healthCheck() {
    const resp = await fetch(`${this.baseUrl}/api/tags`, {
      signal: AbortSignal.timeout(5000),
    });
    return { ok: resp.ok, status: resp.status };
  }
}

/**
 * 按 provider 配置构造对应后端。
 */
export function backendFor(config) {
  const protocol = config.protocol ?? 'openai';
  if (protocol === 'ollama') {
    return new OllamaBackend({ baseUrl: config.baseUrl, timeoutMs: config.timeoutMs });
  }
  return new OpenAiCompatBackend({
    baseUrl: config.baseUrl,
    apiKey: config.apiKey,
    timeoutMs: config.timeoutMs,
  });
}
