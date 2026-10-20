// TTS 朗读引擎
//
// 为什么不是直接 new SpeechSynthesisUtterance 念整段：
// Chromium（含 WebView2）的 speechSynthesis 有三个固有问题——
//   1. 单条 utterance 过长（实测 300~400 字以上）会被静默截断，甚至完全不发声；
//   2. 长朗读过程中会随机进入 paused 状态，表现为「念到一半自己停了」；
//   3. Markdown 语法符号会被逐字念出来（“星号星号”“井号”），代码块更灾难。
// 另外 Windows 中文音色的语速默认偏快，念长句听不清。
//
// 所以这里做成分句队列 + 周期性 resume 心跳 + 文本清洗。

export type TtsVoiceInfo = {
  name: string;
  lang: string;
  local: boolean;
};

export type TtsSettings = {
  /** 指定音色名；null 表示自动挑第一个中文音色 */
  voiceName: string | null;
  rate: number;
  pitch: number;
  volume: number;
  /** 单句最大字数，超过则按逗号再切 */
  maxChars: number;
};

export type TtsEndReason = "done" | "stopped" | "error";

export type TtsState = {
  speaking: boolean;
  /** 当前正在念第几句（从 0 开始） */
  index: number;
  total: number;
  /** 供 UI 显示的一段预览 */
  preview: string;
};

const STORAGE_KEY = "pcg.tts.settings";

const DEFAULT_SETTINGS: TtsSettings = {
  voiceName: null,
  rate: 1.05,
  pitch: 1,
  volume: 1,
  maxChars: 90,
};

export function loadTtsSettings(): TtsSettings {
  if (typeof window === "undefined") return { ...DEFAULT_SETTINGS };
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_SETTINGS };
    const parsed = JSON.parse(raw) as Partial<TtsSettings>;
    return {
      voiceName: typeof parsed.voiceName === "string" ? parsed.voiceName : null,
      rate: clamp(numberOr(parsed.rate, DEFAULT_SETTINGS.rate), 0.5, 2),
      pitch: clamp(numberOr(parsed.pitch, DEFAULT_SETTINGS.pitch), 0.5, 2),
      volume: clamp(numberOr(parsed.volume, DEFAULT_SETTINGS.volume), 0, 1),
      maxChars: Math.round(clamp(numberOr(parsed.maxChars, DEFAULT_SETTINGS.maxChars), 30, 300)),
    };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

export function saveTtsSettings(next: Partial<TtsSettings>): TtsSettings {
  const merged = { ...loadTtsSettings(), ...next };
  if (typeof window !== "undefined") {
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(merged));
    } catch {
      /* 隐私模式等情况忽略 */
    }
  }
  return merged;
}

function numberOr(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/**
 * 把模型输出清洗成适合朗读的纯文本。
 * 原则：宁可少念，不要念出符号噪音。
 */
export function stripMarkupForSpeech(raw: string): string {
  if (!raw) return "";
  let t = raw;

  // 围栏代码块：整段替换成提示语，否则会把代码逐字念出来
  t = t.replace(/```[\s\S]*?```/g, "（这里有一段代码）");
  // 行内代码保留内容
  t = t.replace(/`([^`]+)`/g, "$1");
  // 图片/链接只保留可见文字
  t = t.replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1");
  t = t.replace(/\[([^\]]+)\]\([^)]*\)/g, "$1");
  // HTML 标签
  t = t.replace(/<[^>]{1,80}>/g, "");
  // 标题、引用、列表符号
  t = t.replace(/^[ \t]{0,3}#{1,6}[ \t]+/gm, "");
  t = t.replace(/^[ \t]{0,3}>[ \t]?/gm, "");
  t = t.replace(/^[ \t]{0,3}[-*+][ \t]+/gm, "");
  t = t.replace(/^[ \t]{0,3}\d{1,3}[.)][ \t]+/gm, "");
  // 表格分隔行（|---|---|）
  t = t.replace(/^[ \t]*\|?[ \t:|-]{4,}\|?[ \t]*$/gm, "");
  // 表格竖线换成停顿
  t = t.replace(/\|/g, "，");
  // 强调符、删除线、分隔线
  t = t.replace(/(\*\*|__|~~)/g, "");
  t = t.replace(/(^|[^\w])\*([^*\n]+)\*/g, "$1$2");
  t = t.replace(/^[ \t]*(-{3,}|\*{3,}|_{3,})[ \t]*$/gm, "");
  // emoji 与常见装饰符号（保留中文标点）
  t = t.replace(
    /[\u{1F000}-\u{1FAFF}\u{2600}-\u{27BF}\u{2B00}-\u{2BFF}\u{FE0F}\u{200D}]/gu,
    ""
  );
  // 空白归一
  t = t.replace(/[ \t\u00a0]+/g, " ");
  t = t.replace(/\n{2,}/g, "\n");
  return t.trim();
}

/**
 * 切成适合逐句朗读的片段。
 * 优先按段落 → 句末标点 → 逗号顿号 → 硬切，保证每段不超过 maxChars。
 */
export function splitForSpeech(text: string, maxChars: number): string[] {
  const limit = Math.max(20, maxChars);
  const out: string[] = [];

  const pushChunk = (chunk: string) => {
    const s = chunk.trim();
    if (!s) return;
    if (s.length <= limit) {
      out.push(s);
      return;
    }
    // 超长：按逗号/顿号/分号再切
    const parts = s.split(/(?<=[，,、；;：:])/);
    let buf = "";
    for (const p of parts) {
      if ((buf + p).length > limit) {
        if (buf.trim()) out.push(buf.trim());
        if (p.length > limit) {
          for (let i = 0; i < p.length; i += limit) {
            const seg = p.slice(i, i + limit).trim();
            if (seg) out.push(seg);
          }
          buf = "";
        } else {
          buf = p;
        }
      } else {
        buf += p;
      }
    }
    if (buf.trim()) out.push(buf.trim());
  };

  for (const para of text.split(/\n+/)) {
    // 按句末标点切；保留标点本身
    const sentences = para.split(/(?<=[。！？!?；;…])/);
    for (const s of sentences) pushChunk(s);
  }

  return out;
}

type SpeakHooks = {
  onStart?: () => void;
  onEnd?: (reason: TtsEndReason) => void;
  onProgress?: (index: number, total: number) => void;
};

class TtsEngine {
  private settings: TtsSettings = DEFAULT_SETTINGS;
  private voices: SpeechSynthesisVoice[] = [];
  private queue: string[] = [];
  private cursor = 0;
  private hooks: SpeakHooks = {};
  private keepAliveTimer: number | null = null;
  private stopRequested = false;
  private active = false;
  private currentIndex = 0;
  private currentTotal = 0;
  private listeners = new Set<() => void>();
  private voicesBound = false;

  constructor() {
    if (typeof window === "undefined") return;
    this.settings = loadTtsSettings();
    this.bindVoices();
  }

  /** 浏览器是否支持 TTS */
  get supported(): boolean {
    return typeof window !== "undefined" && "speechSynthesis" in window;
  }

  get state(): TtsState {
    return {
      speaking: this.active,
      index: this.currentIndex,
      total: this.currentTotal,
      preview: this.queue[this.currentIndex] ?? "",
    };
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }

  private bindVoices() {
    if (!this.supported || this.voicesBound) return;
    this.voicesBound = true;
    const sync = () => {
      try {
        this.voices = window.speechSynthesis.getVoices() ?? [];
      } catch {
        this.voices = [];
      }
      this.emit();
    };
    sync();
    // getVoices() 首次通常是空的，音色表异步加载
    window.speechSynthesis.addEventListener("voiceschanged", sync);
  }

  getVoices(): TtsVoiceInfo[] {
    if (this.voices.length === 0 && this.supported) {
      try {
        this.voices = window.speechSynthesis.getVoices() ?? [];
      } catch {
        /* ignore */
      }
    }
    return this.voices.map((v) => ({
      name: v.name,
      lang: v.lang,
      local: v.localService,
    }));
  }

  getSettings(): TtsSettings {
    return { ...this.settings };
  }

  updateSettings(patch: Partial<TtsSettings>): TtsSettings {
    this.settings = saveTtsSettings(patch);
    this.emit();
    return this.getSettings();
  }

  /**
   * 选音色：用户指定 → 按偏好挑中文音色 → 系统默认。
   *
   * 本机实测（Windows OneCore）：Huihui / Kangkang / Yaoyao 三个中文音色。
   * getVoices() 的顺序不稳定，所以显式排优先级，否则默认音色随机，
   * 有时落到男声 Kangkang，听感差别很明显。
   */
  private resolveVoice(): SpeechSynthesisVoice | null {
    if (this.voices.length === 0) return null;
    const wanted = this.settings.voiceName;
    if (wanted) {
      const hit = this.voices.find((v) => v.name === wanted);
      if (hit) return hit;
    }
    const zh = this.voices.filter((v) => /^zh/i.test(String(v.lang)));
    if (zh.length === 0) return null;
    // 自然度优先级：慧慧 > 晓晓 > 云希 > 瑶瑶 > 康康
    const preferred = ["Huihui", "Xiaoxiao", "Yunxi", "Yaoyao", "Kangkang"];
    for (const key of preferred) {
      const hit = zh.find((v) => v.name.includes(key));
      if (hit) return hit;
    }
    return zh.find((v) => v.localService) ?? zh[0];
  }

  /**
   * 朗读一段文本。会打断当前朗读。
   * @returns 是否真的开始朗读
   */
  speak(text: string, hooks: SpeakHooks = {}): boolean {
    if (!this.supported) return false;
    const clean = stripMarkupForSpeech(text);
    if (!clean) {
      hooks.onEnd?.("done");
      return false;
    }

    this.stopInternal();
    this.bindVoices();

    const chunks = splitForSpeech(clean, this.settings.maxChars);
    if (chunks.length === 0) {
      hooks.onEnd?.("done");
      return false;
    }

    this.queue = chunks;
    this.cursor = 0;
    this.currentIndex = 0;
    this.currentTotal = chunks.length;
    this.hooks = hooks;
    this.stopRequested = false;
    this.active = true;
    this.emit();
    hooks.onStart?.();
    hooks.onProgress?.(0, chunks.length);

    this.startKeepAlive();
    this.speakCurrent();
    return true;
  }

  private speakCurrent() {
    if (this.stopRequested) return;
    const chunk = this.queue[this.cursor];
    if (!chunk) {
      this.finish("done");
      return;
    }

    const utter = new SpeechSynthesisUtterance(chunk);
    const voice = this.resolveVoice();
    if (voice) {
      utter.voice = voice;
      utter.lang = voice.lang;
    } else {
      utter.lang = "zh-CN";
    }
    utter.rate = this.settings.rate;
    utter.pitch = this.settings.pitch;
    utter.volume = this.settings.volume;

    const myCursor = this.cursor;
    utter.onstart = () => {
      if (this.stopRequested || myCursor !== this.cursor) return;
      this.currentIndex = myCursor;
      this.emit();
      this.hooks.onProgress?.(myCursor, this.queue.length);
    };
    utter.onend = () => {
      // 防止旧句子的 onend 推进新队列
      if (this.stopRequested || myCursor !== this.cursor) return;
      this.cursor += 1;
      this.currentIndex = this.cursor;
      this.emit();
      this.hooks.onProgress?.(this.cursor, this.queue.length);
      if (this.cursor >= this.queue.length) {
        this.finish("done");
      } else {
        this.speakCurrent();
      }
    };
    utter.onerror = (ev) => {
      if (this.stopRequested || myCursor !== this.cursor) return;
      // canceled 是主动打断的正常结果，不当错误
      const err = String((ev as SpeechSynthesisErrorEvent)?.error ?? "");
      if (err === "canceled" || err === "interrupted") {
        this.finish(this.stopRequested ? "stopped" : "done");
        return;
      }
      // 单句失败不中断整段：跳到下一句
      this.cursor += 1;
      this.currentIndex = this.cursor;
      if (this.cursor >= this.queue.length) {
        this.finish("error");
      } else {
        this.speakCurrent();
      }
    };

    try {
      window.speechSynthesis.speak(utter);
    } catch {
      this.finish("error");
    }
  }

  /** Chromium 长朗读会随机卡在 paused，需要周期性唤醒 */
  private startKeepAlive() {
    this.clearKeepAlive();
    this.keepAliveTimer = window.setInterval(() => {
      if (!this.active) {
        this.clearKeepAlive();
        return;
      }
      try {
        const s = window.speechSynthesis;
        if (s.paused) s.resume();
      } catch {
        /* ignore */
      }
    }, 6000);
  }

  private clearKeepAlive() {
    if (this.keepAliveTimer !== null) {
      window.clearInterval(this.keepAliveTimer);
      this.keepAliveTimer = null;
    }
  }

  private finish(reason: TtsEndReason) {
    const hooks = this.hooks;
    this.hooks = {};
    this.active = false;
    this.queue = [];
    this.cursor = 0;
    this.currentIndex = 0;
    this.currentTotal = 0;
    this.clearKeepAlive();
    this.emit();
    hooks.onEnd?.(reason);
  }

  /**
   * 主动停止。
   * 注意：必须先 finish 再清 hooks——否则调用方收不到 onEnd，
   * 「实时对话」模式会卡在 speaking 状态不再重启识别。
   */
  stop() {
    if (!this.supported) return;
    if (!this.active) {
      // 未在朗读也顺手清一下系统队列，避免残留
      try {
        window.speechSynthesis.cancel();
      } catch {
        /* ignore */
      }
      return;
    }
    this.stopRequested = true;
    this.clearKeepAlive();
    try {
      window.speechSynthesis.cancel();
    } catch {
      /* ignore */
    }
    this.finish("stopped");
  }

  private stopInternal() {
    this.stopRequested = true;
    this.clearKeepAlive();
    try {
      window.speechSynthesis.cancel();
    } catch {
      /* ignore */
    }
  }

  /** 暂停/继续（部分平台支持） */
  pause() {
    if (!this.supported) return;
    try {
      window.speechSynthesis.pause();
    } catch {
      /* ignore */
    }
  }

  resume() {
    if (!this.supported) return;
    try {
      window.speechSynthesis.resume();
    } catch {
      /* ignore */
    }
  }

  get isSpeaking(): boolean {
    return this.active;
  }
}

let engine: TtsEngine | null = null;

/** 全局唯一的 TTS 引擎（多个页面共用，避免互相打断对方的状态） */
export function getTts(): TtsEngine {
  if (!engine) engine = new TtsEngine();
  return engine;
}

export type { TtsEngine };
