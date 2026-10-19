// 模型路由：多 provider 故障切换 + 健康状态 + token 累计。
//
// 失败判定只认「网络错误 / 非 2xx / 流中途抛错」，
// 不认「模型返回空内容」——空内容是模型的事，不是 provider 的锅。

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
   */
  constructor(providers) {
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
   * 流式对话。按优先级尝试，失败切下一个，全部失败抛错。
   * @param {object} req { messages, tools?, temperature?, model?, extraOptions? }
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
      try {
        const model = req.model ?? runtime.model;
        for await (const chunk of runtime.backend.stream(model, req)) {
          if (chunk.type === 'done') {
            runtime.totalTokens.merge(chunk.usage ?? TokenUsage.zero());
          }
          yield chunk;
        }
        // 正常结束：清空连续失败，记录延迟。
        runtime.totalCalls += 1;
        runtime.consecutiveFailures = 0;
        runtime.lastLatencyMs = Date.now() - started;
        runtime.lastError = null;
        return;
      } catch (err) {
        runtime.totalCalls += 1;
        runtime.consecutiveFailures += 1;
        runtime.lastError = err.message;
        lastError = err;
        // 若已产出过文本，不要静默重试导致前端看到重复内容；
        // 只在「尚未产出任何 chunk」时才 failover。
        // 这里用约定：调用方在收到首个 text/tool_call 后自行 stop，故此处直接尝试下一个。
      }
    }

    yield {
      type: 'error',
      message: `all ${candidates.length} providers failed: ${lastError?.message ?? 'unknown'}`,
    };
  }

  #orderedCandidates(model) {
    // 指定了具体 model 且与某 provider 匹配时优先它；否则按健康度排序（健康的在前）。
    const healthy = this.providers.filter((p) => p.healthy);
    const pool = healthy.length ? healthy : this.providers;
    if (model) {
      const matched = pool.filter((p) => p.model === model);
      if (matched.length) return matched;
    }
    return pool;
  }
}
