// 模型后端：OpenAI 兼容 + Ollama 两套协议，统一输出 ChatChunk 语义。
//
// 设计对齐 Rust 侧 `llm::ChatChunk`：
//   { type: 'text', text }
//   { type: 'tool_call', call }
//   { type: 'done', reason, usage }
//   { type: 'error', message }（由 router 兜底，后端正常情况下不产出，只抛异常）
//
// 每个后端暴露 `stream(model, payload) -> AsyncGenerator<Chunk>`，
// payload 为 OpenAI 兼容消息体，Ollama 后端在内部做字段转换。
// 协议差异被隔离在本文件，上层（router / http / socket）只见统一接口。

export { TokenUsage, normalizeUsage } from './usage.js';
export { SseDecoder } from './sse.js';
export { DEFAULT_TIMEOUT_MS } from '../config.js';

import { TokenUsage, normalizeUsage } from './usage.js';
import { SseDecoder } from './sse.js';
import { DEFAULT_TIMEOUT_MS } from '../config.js';

/** 把外部取消信号与超时信号合并成一个 fetch 可用的 signal。 */
function combineSignals(...signals) {
  const list = signals.filter(Boolean);
  if (list.length === 0) return undefined;
  if (typeof AbortSignal.any === 'function') return AbortSignal.any(list);

  // Node 18 兜底：AbortSignal.any 不可用时手动桥接。
  const controller = new AbortController();
  if (list.some((s) => s.aborted)) {
    controller.abort();
    return controller.signal;
  }
  const onAbort = (event) => controller.abort(event.target.reason);
  for (const signal of list) {
    signal.addEventListener('abort', onAbort, { once: true });
  }
  return controller.signal;
}

/** 把取消原因翻译成可读错误（区分超时与主动取消）。 */
function abortError(timeoutSignal, cancelSignal, fallback) {
  if (cancelSignal?.aborted) return new Error('请求已取消');
  if (timeoutSignal?.aborted) return new Error(fallback.timeoutMessage);
  return fallback.error;
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
   * @param {object} payload { messages, tools?, temperature?, extraOptions?, signal? }
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

    const timeoutSignal = AbortSignal.timeout(this.timeoutMs);
    const signal = combineSignals(payload.signal, timeoutSignal);

    let resp;
    try {
      resp = await fetch(this.endpoint(), {
        method: 'POST',
        headers,
        body: JSON.stringify(body),
        signal,
      });
    } catch (err) {
      throw abortError(timeoutSignal, payload.signal, {
        error: err,
        timeoutMessage: `provider ${this.baseUrl} 连接超时（${this.timeoutMs}ms）`,
      });
    }

    if (!resp.ok) {
      const text = await resp.text().catch(() => '');
      throw new Error(`provider ${this.baseUrl} returned ${resp.status}: ${text.slice(0, 500)}`);
    }

    const decoder = new SseDecoder();
    const reader = resp.body.getReader();
    const textDecoder = new TextDecoder();
    let usage = TokenUsage.zero();
    let finalReason = null;
    let serverDone = false;
    // index -> { id, name, args }，finish_reason 时聚合输出。
    const pendingCalls = new Map();

    const drainCalls = () => {
      const calls = [];
      for (const index of [...pendingCalls.keys()].sort((a, b) => a - b)) {
        const call = pendingCalls.get(index);
        if (call.name) {
          calls.push({
            type: 'tool_call',
            call: {
              id: call.id ?? `call_${index}`,
              type: 'function',
              function: { name: call.name, arguments: call.args },
            },
          });
        }
      }
      pendingCalls.clear();
      return calls;
    };

    try {
      while (!serverDone) {
        const chunk = await reader.read();
        if (chunk.done) break;
        if (payload.signal?.aborted) break;

        for (const event of decoder.push(textDecoder.decode(chunk.value, { stream: true }))) {
          if (event.done) {
            serverDone = true;
            break;
          }
          const data = event.json;
          if (data.usage) usage = normalizeUsage(data.usage);
          // 部分服务在出错时仍以 200 + SSE 返回错误体。
          if (data.error) {
            throw new Error(data.error.message ?? JSON.stringify(data.error));
          }

          const choice = data.choices?.[0];
          if (!choice) continue;
          const delta = choice.delta ?? {};

          if (delta.tool_calls?.length) {
            for (const tc of delta.tool_calls) {
              const index = tc.index ?? 0;
              if (!pendingCalls.has(index)) {
                pendingCalls.set(index, { id: tc.id ?? null, name: '', args: '' });
              }
              const current = pendingCalls.get(index);
              if (tc.id) current.id = tc.id;
              if (tc.function?.name) current.name += tc.function.name;
              if (tc.function?.arguments) current.args += tc.function.arguments;
            }
          }

          if (delta.content) yield { type: 'text', text: delta.content };

          if (choice.finish_reason) {
            finalReason = choice.finish_reason;
            // 工具调用在 finish_reason 即确定，立刻聚合输出；
            // 之后可能还有一个只带 usage 的空 choices chunk。
            for (const call of drainCalls()) yield call;
          }
        }
      }

      for (const event of decoder.flush()) {
        if (event.done) continue;
        if (event.json.usage) usage = normalizeUsage(event.json.usage);
      }

      // 兜底：服务端没发 finish_reason 也没发 [DONE]，把残片吐出来。
      for (const call of drainCalls()) yield call;
      yield { type: 'done', reason: finalReason ?? 'stop', usage };
    } catch (err) {
      throw abortError(timeoutSignal, payload.signal, {
        error: err,
        timeoutMessage: `provider ${this.baseUrl} 生成超时（${this.timeoutMs}ms）`,
      });
    } finally {
      try {
        await reader.cancel();
      } catch {
        /* ignore */
      }
    }
  }

  /** 健康探针：列模型接口，任何 2xx 即视为可达。 */
  async healthCheck({ signal } = {}) {
    const resp = await fetch(`${this.baseUrl}/models`, {
      headers: this.apiKey ? { Authorization: `Bearer ${this.apiKey}` } : {},
      signal: combineSignals(signal, AbortSignal.timeout(5000)),
    }).catch((err) => ({ ok: false, status: 0, err }));
    return { ok: Boolean(resp.ok), status: resp.status ?? 0 };
  }
}

/**
 * Ollama 后端。baseUrl 形如 `http://localhost:11434`，走原生 `/api/chat`。
 * 消息体是 OpenAI 兼容格式，Ollama 原生接口也接受该格式。
 */
export class OllamaBackend {
  constructor({
    baseUrl,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    keepAlive = '30m',
    think = false,
  } = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.timeoutMs = timeoutMs;
    this.keepAlive = keepAlive;
    this.think = think;
  }

  endpoint() {
    return `${this.baseUrl}/api/chat`;
  }

  async *stream(model, payload) {
    const body = {
      model,
      messages: payload.messages,
      stream: true,
      keep_alive: payload.keepAlive ?? this.keepAlive,
      think: payload.think ?? this.think ?? false,
      options: {
        temperature: payload.temperature ?? 0.7,
        ...(payload.extraOptions ?? {}),
      },
      ...(payload.tools?.length ? { tools: payload.tools } : {}),
    };

    const timeoutSignal = AbortSignal.timeout(this.timeoutMs);
    const signal = combineSignals(payload.signal, timeoutSignal);

    let resp;
    try {
      resp = await fetch(this.endpoint(), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        signal,
      });
    } catch (err) {
      throw abortError(timeoutSignal, payload.signal, {
        error: err,
        timeoutMessage: `ollama ${this.baseUrl} 连接超时（${this.timeoutMs}ms）`,
      });
    }

    if (!resp.ok) {
      const text = await resp.text().catch(() => '');
      throw new Error(`ollama ${this.baseUrl} returned ${resp.status}: ${text.slice(0, 500)}`);
    }

    const decoder = new SseDecoder();
    const reader = resp.body.getReader();
    const textDecoder = new TextDecoder();
    let usage = TokenUsage.zero();
    let reason = 'stop';
    let doneEmitted = false;

    try {
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        if (payload.signal?.aborted) break;

        for (const event of decoder.push(textDecoder.decode(chunk.value, { stream: true }))) {
          if (event.done) continue;
          const data = event.json;
          if (data.error) {
            throw new Error(
              typeof data.error === 'string' ? data.error : JSON.stringify(data.error),
            );
          }
          if (data.done_reason) reason = data.done_reason;
          if (data.done) {
            usage = normalizeUsage({
              prompt_eval_count: data.prompt_eval_count,
              eval_count: data.eval_count,
              prompt_eval_duration: data.prompt_eval_duration,
              eval_duration: data.eval_duration,
            });
            doneEmitted = true;
            yield { type: 'done', reason, usage };
            break;
          }

          const message = data.message;
          if (!message) continue;

          if (message.tool_calls?.length) {
            for (const tc of message.tool_calls) {
              const fn = tc.function ?? {};
              // Ollama 原生返回对象型 arguments，OpenAI 语义要求字符串，统一 stringify。
              let args = fn.arguments ?? '';
              if (typeof args !== 'string') args = JSON.stringify(args);
              yield {
                type: 'tool_call',
                call: {
                  id: tc.id ?? null,
                  type: 'function',
                  function: { name: fn.name ?? '', arguments: args },
                },
              };
            }
          }

          if (message.content) yield { type: 'text', text: message.content };
        }
      }

      // 兜底：连接结束但没收到 done 标记。
      if (!doneEmitted) yield { type: 'done', reason, usage };
    } catch (err) {
      throw abortError(timeoutSignal, payload.signal, {
        error: err,
        timeoutMessage: `ollama ${this.baseUrl} 生成超时（${this.timeoutMs}ms）`,
      });
    } finally {
      try {
        await reader.cancel();
      } catch {
        /* ignore */
      }
    }
  }

  async healthCheck({ signal } = {}) {
    const resp = await fetch(`${this.baseUrl}/api/tags`, {
      signal: combineSignals(signal, AbortSignal.timeout(5000)),
    }).catch((err) => ({ ok: false, status: 0, err }));
    return { ok: Boolean(resp.ok), status: resp.status ?? 0 };
  }
}

/** 按 provider 配置构造对应后端。 */
export function backendFor(config) {
  const protocol = config.protocol ?? 'openai';
  if (protocol === 'ollama') {
    return new OllamaBackend({
      baseUrl: config.baseUrl,
      timeoutMs: config.timeoutMs,
      keepAlive: config.keepAlive,
      think: config.think,
    });
  }
  return new OpenAiCompatBackend({
    baseUrl: config.baseUrl,
    apiKey: config.apiKey,
    timeoutMs: config.timeoutMs,
  });
}
