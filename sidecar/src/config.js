// 配置加载：环境变量优先，包目录下的 JSON 文件次之，最后回落默认值。
//
// 关键约定：
// - providers.json / mcp-servers.json 一律按「包目录」（sidecar/）解析，
//   不再依赖进程当前工作目录（旧实现写 './providers.json'，会随启动目录漂移）。
// - 凭据只从环境变量 / 文件读取，日志与健康快照中绝不回显。

import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { readFileSync, existsSync, renameSync, writeFileSync } from 'node:fs';

export const PACKAGE_ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
export const SIDECAR_VERSION = '0.1.0';
export const DEFAULT_PORT = 4770;
export const DEFAULT_HOST = '127.0.0.1';
export const DEFAULT_TIMEOUT_MS = 60_000;
export const DEFAULT_OLLAMA_KEEP_ALIVE = '30m';

/** 读取并解析环境变量中的 JSON；不存在返回 null；非法直接抛错（配置错不能静默）。 */
function readJsonEnv(name) {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return null;
  try {
    return JSON.parse(raw);
  } catch (err) {
    throw new Error(`环境变量 ${name} 不是合法 JSON：${err.message}`);
  }
}

/** 读取并解析 JSON 文件。 */
function readJsonFile(file, label) {
  try {
    return JSON.parse(readFileSync(file, 'utf8'));
  } catch (err) {
    throw new Error(`解析 ${label} 失败（${file}）：${err.message}`);
  }
}

/** 归一化单条 provider 配置：补齐默认值、统一 baseUrl 形态、解析 apiKeyEnv。 */
function normalizeProvider(raw) {
  const protocol = raw.protocol ?? 'openai';
  const baseUrl = String(raw.baseUrl ?? '').replace(/\/+$/, '');
  const id = raw.id ?? raw.name ?? `${protocol}-${baseUrl || 'unknown'}`;
  const apiKey = raw.apiKey ?? (raw.apiKeyEnv ? process.env[raw.apiKeyEnv] : undefined);

  const provider = {
    ...raw,
    id,
    name: raw.name ?? id,
    protocol,
    baseUrl,
    model: raw.model ?? null,
    apiKey: apiKey || undefined,
    timeoutMs: Number(raw.timeoutMs ?? DEFAULT_TIMEOUT_MS),
  };

  if (protocol === 'ollama') {
    provider.keepAlive = raw.keepAlive ?? DEFAULT_OLLAMA_KEEP_ALIVE;
    if (raw.think !== undefined) provider.think = Boolean(raw.think);
  }

  return provider;
}

/**
 * 加载 provider 列表。
 * 优先级：SIDECAR_PROVIDERS（JSON 数组）> <包目录>/providers.json > 默认本地 Ollama。
 */
export function loadProviders(logger = console) {
  let raw = null;
  try {
    raw = readJsonEnv('SIDECAR_PROVIDERS');
  } catch (err) {
    logger.error?.(`[config] ${err.message}，改用文件/默认配置`);
  }

  const file = join(PACKAGE_ROOT, 'providers.json');
  if (!raw && existsSync(file)) {
    try {
      raw = readJsonFile(file, 'providers.json');
    } catch (err) {
      logger.error?.(`[config] ${err.message}，使用默认配置`);
    }
  }

  if (!raw) {
    raw = [
      {
        id: 'ollama-local',
        name: 'Ollama 本地',
        protocol: 'ollama',
        baseUrl: process.env.OLLAMA_BASE_URL ?? 'http://localhost:11434',
        model: process.env.SIDECAR_DEFAULT_MODEL ?? 'qwen3:8b',
      },
    ];
  }

  const list = Array.isArray(raw) ? raw : raw.providers ?? [];
  return list.filter(Boolean).map(normalizeProvider);
}

/**
 * 加载 MCP server 配置。
 * 优先级：SIDECAR_MCP_SERVERS（JSON 数组）> <包目录>/mcp-servers.json > 空数组。
 */
export function loadMcpServers(logger = console) {
  let raw = null;
  try {
    raw = readJsonEnv('SIDECAR_MCP_SERVERS');
  } catch (err) {
    logger.error?.(`[config] ${err.message}，改用文件/空配置`);
  }

  const file = join(PACKAGE_ROOT, 'mcp-servers.json');
  if (!raw && existsSync(file)) {
    try {
      raw = readJsonFile(file, 'mcp-servers.json');
    } catch (err) {
      logger.error?.(`[config] ${err.message}，使用空配置`);
    }
  }

  const list = Array.isArray(raw) ? raw : raw?.servers ?? [];
  return list.filter(Boolean);
}

/**
 * 原子保存 MCP server 清单。安装接口只在握手成功后调用，避免半成品配置
 * 留在磁盘里导致 sidecar 下次启动反复失败。
 */
export function saveMcpServers(servers) {
  const file = join(PACKAGE_ROOT, 'mcp-servers.json');
  const temp = `${file}.tmp`;
  writeFileSync(temp, `${JSON.stringify({ servers }, null, 2)}\n`, 'utf8');
  renameSync(temp, file);
}

/** 监听地址配置。 */
export function listenConfig() {
  return {
    port: Number(process.env.SIDECAR_PORT ?? DEFAULT_PORT),
    host: process.env.SIDECAR_HOST ?? DEFAULT_HOST,
  };
}
