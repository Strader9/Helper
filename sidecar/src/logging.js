// 轻量日志：只做分级 + 统一前缀，不引第三方依赖。
// 级别由 SIDECAR_LOG_LEVEL 控制（debug/info/warn/error，默认 info）。

const LEVELS = { debug: 10, info: 20, warn: 30, error: 40 };

function formatValue(value) {
  if (value instanceof Error) {
    return value.stack ? value.stack.split('\n').slice(0, 3).join('\n') : String(value);
  }
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export function createLogger(component = 'sidecar') {
  const threshold =
    LEVELS[(process.env.SIDECAR_LOG_LEVEL ?? 'info').toLowerCase()] ?? LEVELS.info;

  const emit = (level, args) => {
    if (LEVELS[level] < threshold) return;
    const text = args.map(formatValue).join(' ');
    const line = `${new Date().toISOString()} ${level.toUpperCase().padEnd(5)} [${component}] ${text}`;
    if (level === 'error' || level === 'warn') {
      console.error(line);
    } else {
      console.log(line);
    }
  };

  return {
    debug: (...args) => emit('debug', args),
    info: (...args) => emit('info', args),
    warn: (...args) => emit('warn', args),
    error: (...args) => emit('error', args),
  };
}
