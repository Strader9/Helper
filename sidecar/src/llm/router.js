// 模型路由：多 provider 故障切换 + 健康状态 + token 累计。
//
// 失败判定只认「网络错误 / 非 2xx / 流中途抛错」，
// 不认「模型返回空内容」——空内容是模型的事，不是 provider 的锅。
//
// 关键正确性约定：
// **一旦已经向调用方产出过任何内容（text / tool_call），就不能再切换 provider。**
// 否则第二个 provider 会从头生成，调用方会看到重复、拼接的输出。
// 首字节之前失败则按优先级继续尝试下一个 provider。

import { TokenUsage, backendFor } from './backends.js';

const MAX_CONSECUTIVE_FAILURES = 3;

class ProviderRuntime {
  constructor(config) {
    this.id = config.id;
    this.name = config.name ?? config.id;
    this.model = config.model;
    this.config = config;
    this.backend = backendFor(config);
    this.consecutiveFailures = 0;
    this.lastError = null;
    this.lastLatencyMs = null;
    this.totalCalls = 0;
    this.totalTokens = TokenUsage.zero();
  }

  get healthy() {
    return this.consecutiveFailures < MAX_CONSECUTIVE_FAILURES;
  }

  recordSuccess(latencyMs, usage) {
    this.totalCalls += 1;
    this.consecutiveFailures = 0;
    this.lastLatencyMs = latencyMs;
    this.lastError = null;
    if (usage) this.totalTokens.merge(usage);
  }

  recordFailure(latencyMs, message) {
    this.totalCalls += 1;
    this.consecutiveFailures += 1;
    this.lastError = message;
    this.lastLatencyMs = latencyMs;
  }

  /** 对展示/日志脱敏：永不回显 apiKey。 */
  snapshot() {
    return {
      id: this.id,
      name: this.name,
      model: this.model,
      protocol: this.config.protocol ?? 'openai',
      baseUrl: this.config.baseUrl,
      hasApiKey: Boolean(this.config.apiKey),
      healthy: this.healthy,
      consecutiveFailures: this.consecutiveFailures,
      lastError: this.lastError,
      lastLatencyMs: this.lastLatencyMs,
      totalCalls: this.totalCalls,
      usage: this.totalTokens,
    };
  }
}

export class ModelRouter {
  /**
   * @param {Array<object>} providers 按优先级排序的 provider 配置数组
   * @param {object} [options]
   * @param {object} [options.logger]
   */
  constructor(providers, { logger } = {}) {
    this.logger = logger;
    this.providers = providers.map((c) => new ProviderRuntime(c));
  }

  get count() {
    return this.providers.length;
  }

  health() {
    return this.providers.map((p) => p.snapshot());
  }

  /** 默认 provider（第一个），没有则返回 null。 */
  get defaultModel() {
    return this.providers[0]?.model ?? null;
  }

  /**
   * 流式对话。按优先级尝试，**首字节之前**失败切下一个，全部失败产出 error chunk。
   * @param {object} req { messages, tools?, temperature?, model?, extraOptions?, signal? }
   * @returns {AsyncGenerator<object>} ChatChunk 语义
   */
  async *chatStream(req) {
    const candidates = this.#orderedCandidates(req.model);
    if (candidates.length === 0) {
      yield { type: 'error', message: 'no model provider configured' };
      return;
    }

    let lastError = null;

    for (const runtime of candidates) {
      const started = Date.now();
      const attempt = new AbortController();
      const onParentAbort = () => attempt.abort();
      if (req.signal) {
        if (req.signal.aborted) {
          yield { type: 'error', message: '请求已取消' };
          return;
        }
        req.signal.addEventListener('abort', onParentAbort, { once: true });
      }

      let emitted = false;
      let gotDone = false;
      try {
        const model = req.model ?? runtime.model;
        for await (const chunk of runtime.backend.stream(model, {
          ...req,
          signal: attempt.signal,
        })) {
          if (chunk.type === 'text' || chunk.type === 'tool_call') emitted = true;

          if (chunk.type === 'done') {
            runtime.recordSuccess(Date.now() - started, chunk.usage ?? TokenUsage.zero());
            gotDone = true;
            yield chunk;
            return;
          }

          if (chunk.type === 'error') {
            // 后端/路由的语义错误：与「首字节后失败」同样处理，不再尝试其他 provider。
            runtime.recordFailure(Date.now() - started, chunk.message);
            yield chunk;
            return;
          }

          yield chunk;
        }

        // 正常结束但后端没产出 done（理论上后端会兜底）：补一个，保持协议完整。
        runtime.recordSuccess(Date.now() - started);
        if (!gotDone) yield { type: 'done', reason: 'stop', usage: TokenUsage.zero() };
        return;
      } catch (err) {
        lastError = err;

        // 调用方主动取消：不算 provider 故障，直接结束。
        if (req.signal?.aborted || attempt.signal.aborted) {
          yield { type: 'error', message: `请求已取消：${err.message}` };
          return;
        }

        runtime.recordFailure(Date.now() - started, err.message);

        // 已经向调用方吐过内容，切换 provider 会造成重复输出，只能如实报错。
        if (emitted) {
          yield {
            type: 'error',
            message: `provider ${runtime.id} 在输出过程中断：${err.message}（已开始输出，不切换其他 provider）`,
          };
          return;
        }

        this.logger?.warn?.(
          `provider ${runtime.id} 调用失败（${err.message}），切换下一个 provider`,
        );
      } finally {
        req.signal?.removeEventListener('abort', onParentAbort);
      }
    }

    yield {
      type: 'error',
      message: `all ${candidates.length} providers failed: ${lastError?.message ?? 'unknown'}`,
    };
  }

  #orderedCandidates(model) {
    // 指定了具体 model 且与某 provider 匹配时优先它；否则健康的在前，全挂时全量兜底。
    const healthy = this.providers.filter((p) => p.healthy);
    const pool = healthy.length ? healthy : this.providers;
    if (model) {
      const matched = pool.filter((p) => p.model === model);
      if (matched.length) return matched;
    }
    return pool;
  }
}
