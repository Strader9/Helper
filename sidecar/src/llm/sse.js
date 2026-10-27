// SSE（Server-Sent Events）字节流解码器。
//
// 职责：把增量文本切成事件，处理：
// - 跨 chunk 的不完整行（数据在两次推送之间被截断）；
// - '\r\n' 与 '\n' 两种行尾；
// - 注释行（': ping' 心跳）与空行；
// - `data: [DONE]` 终止标记。
//
// OpenAI / Ollama 每个事件只占一行 JSON，因此见到可解析的 data 行立即产出，
// 不等待空行分隔（这也让调用方在测试中可以逐行断言）。

/**
 * 解码增量 SSE 文本。
 * @returns {Array<{json: object} | {done: true}>}
 */
export class SseDecoder {
  #pending = '';

  push(text) {
    this.#pending += text;
    const out = [];

    while (true) {
      const nl = this.#pending.indexOf('\n');
      if (nl < 0) break;

      let line = this.#pending.slice(0, nl);
      this.#pending = this.#pending.slice(nl + 1);
      if (line.endsWith('\r')) line = line.slice(0, -1);
      line = line.trim();
      if (!line || line.startsWith(':')) continue;

      const event = this.#parseLine(line);
      if (event) out.push(event);
    }

    return out;
  }

  /** 流结束时取出剩余的最后一行（若存在且可解析）。 */
  flush() {
    let line = this.#pending.trim();
    this.#pending = '';
    if (!line || line.startsWith(':')) return [];
    const event = this.#parseLine(line);
    return event ? [event] : [];
  }

  #parseLine(line) {
    if (line === 'data: [DONE]') return { done: true };
    const payload = line.startsWith('data:') ? line.slice(5).trimStart() : line;
    try {
      return { json: JSON.parse(payload) };
    } catch {
      // 不完整行 / 非 JSON 心跳，跳过。
      return null;
    }
  }
}
