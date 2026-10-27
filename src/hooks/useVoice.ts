// 语音交互 Hook —— 浏览器原生 Web Speech API
// 包含：语音识别 (SpeechRecognition) + 语音合成 (speechSynthesis) + 实时对话状态机
// Tauri WebView2 基于 Chromium，支持 webkitSpeechRecognition / speechSynthesis
import { useState, useRef, useCallback, useEffect } from 'react';
import { getTts } from '../lib/tts';

/** 语音整体状态：idle=空闲 listening=聆听中 thinking=等待 AI 回复 speaking=朗读中 */
export type VoiceStatus = 'idle' | 'listening' | 'thinking' | 'speaking';

export interface UseVoiceOptions {
  /**
   * 实时对话模式下，每轮语音识别完成后的最终文本通过此回调交给调用方发送。
   * hook 本身不直接调用任何 invoke / 聊天接口，由调用方（EAssistantChat）决定如何发送。
   */
  onSendMessage?: (text: string) => void;
}

export interface UseVoiceReturn {
  // 语音识别
  isListening: boolean;
  supported: boolean;
  startListening: () => void;
  stopListening: () => void;
  transcript: string; // 已识别完成的稳定文本
  interimTranscript: string; // 临时（未最终）识别结果
  clearTranscript: () => void;

  // TTS
  isSpeaking: boolean;
  speak: (text: string) => void;
  stopSpeaking: () => void;

  // 实时对话模式
  isVoiceMode: boolean;
  toggleVoiceMode: () => void;

  // 状态机
  voiceStatus: VoiceStatus;
}

// 宽松类型声明：TS 内置 DOM lib 不包含 SpeechRecognition
type AnyRecognition = {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  maxAlternatives: number;
  start: () => void;
  stop: () => void;
  abort: () => void;
  onstart: (() => void) | null;
  onresult: ((event: any) => void) | null;
  onerror: ((event: any) => void) | null;
  onend: (() => void) | null;
};

function getRecognitionCtor(): (new () => AnyRecognition) | null {
  if (typeof window === 'undefined') return null;
  const w = window as unknown as Record<string, unknown>;
  return (w.SpeechRecognition || w.webkitSpeechRecognition) as (new () => AnyRecognition) | null;
}

/**
 * 实时对话状态机：
 *   listening →(用户说完 onend)→ sending/thinking →(调用方拿到回复后调用 speak)→
 *   speaking →(朗读结束 onend)→ listening → ……
 * 退出条件：用户 toggleVoiceMode(false)、麦克风权限被拒、组件卸载。
 */
export function useVoice(options: UseVoiceOptions = {}): UseVoiceReturn {
  const [isListening, setIsListening] = useState(false);
  const [transcript, setTranscript] = useState('');
  const [interimTranscript, setInterimTranscript] = useState('');
  const [isSpeaking, setIsSpeaking] = useState(false);
  const [isVoiceMode, setIsVoiceMode] = useState(false);
  const [voiceStatus, setVoiceStatus] = useState<VoiceStatus>('idle');

  const recognitionRef = useRef<AnyRecognition | null>(null);
  const finalTranscriptRef = useRef('');
  const errorRef = useRef<string | null>(null);
  const manualStopRef = useRef(false); // 用户主动停止识别（不触发自动循环）
  const voiceModeRef = useRef(false);
  const restartTimerRef = useRef<number | null>(null);
  const onSendRef = useRef(options.onSendMessage);
  onSendRef.current = options.onSendMessage;

  const RecognitionCtor = getRecognitionCtor();
  const supported = !!RecognitionCtor && typeof window !== 'undefined' && 'speechSynthesis' in window;

  /* ─────────────────── TTS：交给统一引擎（分句 + 清洗 + 心跳） ─────────────────── */
  // 音色选择、语速、Markdown 清洗、长文本分句都在 lib/tts.ts 里处理。
  // 这里只负责把引擎状态映射成 voiceStatus 状态机。
  const tts = getTts();

  useEffect(() => {
    if (!tts.supported) return;
    // 引擎可能被其他页面打断（全局单例），同步一下 isSpeaking
    return tts.subscribe(() => {
      setIsSpeaking(tts.state.speaking);
    });
  }, [tts]);

  /* ─────────────────────── 语音识别：初始化（只创建一次） ─────────────────────── */
  const startListeningRef = useRef<() => void>(() => undefined);

  const ensureRecognition = useCallback((): AnyRecognition | null => {
    if (recognitionRef.current || !RecognitionCtor) return recognitionRef.current;

    const recognition = new RecognitionCtor();
    recognition.lang = 'zh-CN';
    recognition.continuous = false;
    recognition.interimResults = true;
    recognition.maxAlternatives = 1;

    recognition.onstart = () => {
      errorRef.current = null;
      finalTranscriptRef.current = '';
      setTranscript('');
      setInterimTranscript('');
      setIsListening(true);
      setVoiceStatus('listening');
    };

    recognition.onresult = (event: any) => {
      let interim = '';
      let final = '';
      for (let i = event.resultIndex; i < event.results.length; i++) {
        const result = event.results[i];
        const text: string = result[0].transcript;
        if (result.isFinal) final += text;
        else interim += text;
      }
      if (final) {
        finalTranscriptRef.current += final;
        setTranscript(finalTranscriptRef.current);
      }
      setInterimTranscript(interim);
    };

    recognition.onerror = (event: any) => {
      errorRef.current = typeof event?.error === 'string' ? event.error : 'unknown';
      setIsListening(false);
      // 权限被拒：退出实时对话模式
      if (errorRef.current === 'not-allowed' || errorRef.current === 'service-not-allowed') {
        voiceModeRef.current = false;
        setIsVoiceMode(false);
      }
    };

    recognition.onend = () => {
      setIsListening(false);
      setInterimTranscript('');
      const finalText = finalTranscriptRef.current.trim();
      const err = errorRef.current;
      const manual = manualStopRef.current;
      manualStopRef.current = false;

      if (manual) {
        // 用户手动停止识别，不做任何自动循环
        setVoiceStatus(prev => (prev === 'listening' ? 'idle' : prev));
        return;
      }

      if (voiceModeRef.current) {
        if (err === 'not-allowed' || err === 'service-not-allowed') {
          setVoiceStatus('idle');
          return;
        }
        if (finalText) {
          // 用户说完 → 交给调用方发送，等待回复后由调用方调用 speak()
          finalTranscriptRef.current = '';
          setTranscript('');
          setVoiceStatus('thinking');
          onSendRef.current?.(finalText);
        } else {
          // 没听到内容（no-speech 等），短暂延迟后重新开始识别，保持对话循环
          if (restartTimerRef.current !== null) window.clearTimeout(restartTimerRef.current);
          restartTimerRef.current = window.setTimeout(() => {
            if (voiceModeRef.current && !manualStopRef.current) {
              try { recognitionRef.current?.start(); } catch { /* 已在运行 */ }
            }
          }, 700);
        }
      } else {
        setVoiceStatus('idle');
      }
    };

    recognitionRef.current = recognition;
    return recognition;
  }, [RecognitionCtor]);

  const startListening = useCallback(() => {
    if (!supported) return;
    const recognition = ensureRecognition();
    try {
      recognition?.start();
    } catch {
      // 已在监听中，忽略
    }
  }, [supported, ensureRecognition]);
  startListeningRef.current = startListening;

  const stopListening = useCallback(() => {
    manualStopRef.current = true;
    try { recognitionRef.current?.stop(); } catch { /* 未在运行 */ }
    setIsListening(false);
  }, []);

  const clearTranscript = useCallback(() => {
    finalTranscriptRef.current = '';
    setTranscript('');
    setInterimTranscript('');
  }, []);

  /* ───────────────────────────── TTS 朗读 ───────────────────────────── */
  const speak = useCallback(
    (text: string) => {
      const content = (text || '').trim();
      if (!content) return;

      tts.speak(content, {
        onStart: () => {
          setIsSpeaking(true);
          setVoiceStatus('speaking');
        },
        onEnd: (reason) => {
          setIsSpeaking(false);
          if (voiceModeRef.current) {
            if (reason !== 'stopped') {
              // 自然念完 → 自动开始下一轮识别，维持实时对话循环
              setVoiceStatus('listening');
              startListeningRef.current();
            } else {
              setVoiceStatus('idle');
            }
          } else {
            setVoiceStatus('idle');
          }
        },
      });
    },
    [tts]
  );

  const stopSpeaking = useCallback(() => {
    tts.stop();
    setIsSpeaking(false);
  }, [tts]);

  /* ────────────────────────── 实时对话模式开关 ────────────────────────── */
  const toggleVoiceMode = useCallback(() => {
    const next = !voiceModeRef.current;
    voiceModeRef.current = next;
    setIsVoiceMode(next);

    if (next) {
      // 开启：打断当前任何朗读，立即开始聆听
      stopSpeaking();
      startListening();
    } else {
      // 关闭：停止识别与朗读
      manualStopRef.current = true;
      try { recognitionRef.current?.stop(); } catch { /* 未在运行 */ }
      stopSpeaking();
      setVoiceStatus('idle');
    }
  }, [startListening, stopSpeaking]);

  /* ───────────────────────────── 卸载清理 ───────────────────────────── */
  useEffect(() => {
    return () => {
      voiceModeRef.current = false;
      manualStopRef.current = true;
      recognitionRef.current?.abort();
      recognitionRef.current = null;
      // 走引擎 stop 而不是直接 cancel：避免引擎 active 状态残留，
      // 否则再次进入页面时 isSpeaking 会一直是 true
      try {
        tts.stop();
      } catch {
        /* ignore */
      }
      if (restartTimerRef.current !== null) {
        window.clearTimeout(restartTimerRef.current);
      }
    };
  }, []);

  return {
    isListening,
    supported,
    startListening,
    stopListening,
    transcript,
    interimTranscript,
    clearTranscript,
    isSpeaking,
    speak,
    stopSpeaking,
    isVoiceMode,
    toggleVoiceMode,
    voiceStatus,
  };
}
