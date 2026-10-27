// token 用量统计。只用于观测，不影响生成——拿不到就按 0。
//
// 字段兼容两套协议：
// - OpenAI：prompt_tokens / completion_tokens
// - Ollama：prompt_eval_count / eval_count（以及 *_duration 纳秒）

/**
 * 统一用量。
 */
export class TokenUsage {
  constructor({
    promptTokens = 0,
    completionTokens = 0,
    promptNanos = 0,
    evalNanos = 0,
  } = {}) {
    this.promptTokens = promptTokens;
    this.completionTokens = completionTokens;
    this.promptNanos = promptNanos;
    this.evalNanos = evalNanos;
  }

  static zero() {
    return new TokenUsage();
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
 * 从任意兼容 usage 对象归一化。
 */
export function normalizeUsage(raw) {
  if (!raw || typeof raw !== 'object') return TokenUsage.zero();
  const num = (value) => {
    const n = Number(value);
    return Number.isFinite(n) && n > 0 ? n : 0;
  };
  return new TokenUsage({
    promptTokens: num(
      raw.prompt_tokens ?? raw.promptTokens ?? raw.prompt_eval_count,
    ),
    completionTokens: num(
      raw.completion_tokens ?? raw.completionTokens ?? raw.eval_count,
    ),
    promptNanos: num(
      raw.prompt_tokens_details?.duration_ns ?? raw.prompt_eval_duration,
    ),
    evalNanos: num(
      raw.completion_tokens_details?.duration_ns ?? raw.eval_duration,
    ),
  });
}
