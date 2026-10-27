// MCP 客户端（sidecar 侧）。
//
// 职责：用官方 `@modelcontextprotocol/sdk` 承载 MCP 协议细节，把用户配置的
// 多个 MCP server（stdio / streamable HTTP）统一成一个工具池，供 Rust 侧
// 两个薄转发工具（mcp_list_tools / mcp_call）触达。
//
// 关键取舍：
// - 懒连接：只在第一次 list/call 时才 spawn/connect，避免 sidecar 启动即拉起一堆进程。
// - 连接缓存 + 失败重置：成功缓存 Client，失败清掉，下次重试；绝不缓存一个坏连接。
// - 工具名跨 server 归并：list 时带 server 归属；call 时同名多命要求显式指定 server。

import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';

const CLIENT_INFO = { name: 'e-assisant-sidecar', version: '0.1.0' };

/**
 * 从环境变量加载 MCP server 配置。
 * 文件（mcp-servers.json）由 config.js 统一按包目录加载，这里不隐式读文件。
 * 优先级：SIDECAR_MCP_SERVERS（JSON 数组）> 空数组。
 */
export function loadMcpServers() {
  if (!process.env.SIDECAR_MCP_SERVERS) return [];
  try {
    const raw = JSON.parse(process.env.SIDECAR_MCP_SERVERS);
    return Array.isArray(raw) ? raw : raw?.servers ?? [];
  } catch (err) {
    console.error('[mcp] 解析 SIDECAR_MCP_SERVERS 失败，使用空配置:', err.message);
    return [];
  }
}

function transportFor(config) {
  if (config.transport === 'http' || config.url) {
    if (!config.url) throw new Error('http 传输的 MCP server 缺少 url 字段');
    return new StreamableHTTPClientTransport(new URL(config.url));
  }
  if (!config.command) throw new Error('stdio 传输的 MCP server 缺少 command 字段');
  return new StdioClientTransport({
    command: config.command,
    args: config.args ?? [],
    env: config.env,
    cwd: config.cwd,
    stderr: 'inherit',
  });
}

function normalizeTool(server, tool) {
  return {
    server: server.id,
    name: tool.name,
    description: tool.description ?? '',
    inputSchema: tool.inputSchema ?? {},
  };
}

/**
 * 单个 MCP server 的运行时连接。
 */
class ServerRuntime {
  constructor(config, logger) {
    this.id = config.id ?? config.name ?? `server-${Math.random().toString(36).slice(2, 8)}`;
    this.name = config.name ?? this.id;
    this.transport = config.transport ?? (config.url ? 'http' : 'stdio');
    this.config = config;
    this.logger = logger;
    this.client = null;
    this.lastError = null;
  }

  get connected() {
    return this.client !== null;
  }

  snapshot() {
    return {
      id: this.id,
      name: this.name,
      transport: this.transport,
      status: this.connected ? 'connected' : 'disconnected',
      lastError: this.lastError,
    };
  }

  async connect() {
    if (this.client) return this.client;
    const transport = transportFor(this.config);
    const client = new Client(CLIENT_INFO, { capabilities: {} });
    try {
      await client.connect(transport);
    } catch (err) {
      this.lastError = err.message;
      try {
        await client.close();
      } catch {
        /* ignore */
      }
      throw err;
    }
    this.client = client;
    this.lastError = null;
    this.logger?.debug?.(`MCP server ${this.id} 已连接`);
    return client;
  }

  async disconnect() {
    if (!this.client) return;
    try {
      await this.client.close();
    } catch {
      /* ignore */
    }
    this.client = null;
  }

  async listTools() {
    const client = await this.connect();
    const res = await client.listTools();
    return (res.tools ?? []).map((t) => normalizeTool(this, t));
  }

  async callTool(name, args) {
    const client = await this.connect();
    return client.callTool({ name, arguments: args ?? {} });
  }
}

export class McpManager {
  /**
   * @param {Array<object>} servers MCP server 配置
   * @param {object} [options]
   * @param {object} [options.logger]
   */
  constructor(servers, { logger, onChange } = {}) {
    this.logger = logger;
    this.servers = (servers ?? []).map((c) => new ServerRuntime(c, logger));
    this.onChange = onChange;
  }

  get count() {
    return this.servers.length;
  }

  status() {
    return this.servers.map((s) => s.snapshot());
  }

  configs() {
    return this.servers.map((server) => ({ ...server.config, id: server.id, name: server.name }));
  }

  /**
   * 安装并立即握手一个 MCP server。只有成功列出工具后才加入运行池并持久化，
   * 因此“安装成功”同时意味着 AI 已经可以通过 mcp_list_tools 发现它。
   */
  async installServer(config) {
    const id = String(config?.id ?? '').trim();
    if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]{1,63}$/.test(id)) {
      throw new Error('id 只能包含字母、数字、点、下划线和短横线，长度 2-64');
    }
    if (this.servers.some((server) => server.id === id)) {
      throw new Error(`MCP server 已存在：${id}`);
    }

    const candidate = new ServerRuntime({ ...config, id }, this.logger);
    let tools;
    try {
      tools = await candidate.listTools();
      if (tools.length === 0) throw new Error('连接成功，但 server 没有公开任何工具');
      this.servers.push(candidate);
      await this.onChange?.(this.configs());
    } catch (error) {
      await candidate.disconnect();
      this.servers = this.servers.filter((server) => server !== candidate);
      throw error;
    }

    return {
      server: candidate.snapshot(),
      tools: tools.map((tool) => ({ name: tool.name, description: tool.description })),
    };
  }

  async removeServer(id) {
    const index = this.servers.findIndex((server) => server.id === id);
    if (index < 0) return false;
    const [server] = this.servers.splice(index, 1);
    try {
      await this.onChange?.(this.configs());
    } catch (error) {
      this.servers.splice(index, 0, server);
      throw error;
    }
    await server.disconnect();
    return true;
  }

  /** 关闭全部连接（优雅退出用）。 */
  async closeAll() {
    await Promise.allSettled(this.servers.map((s) => s.disconnect()));
  }

  /**
   * 拉取全部 server 的工具，按 server 归组并摊平。
   * 单个 server 失败不影响其他 server。
   * @returns {Promise<{servers: Array, tools: Array}>}
   */
  async listAllTools() {
    const servers = [];
    const tools = [];
    for (const server of this.servers) {
      let serverTools = [];
      let error = null;
      try {
        serverTools = await server.listTools();
      } catch (err) {
        error = err.message;
        // 连接失败就重置，下次重试；不要缓存坏连接。
        await server.disconnect();
      }
      servers.push({
        ...server.snapshot(),
        error,
        tools: serverTools.map((t) => ({ name: t.name, description: t.description })),
      });
      tools.push(...serverTools);
    }
    return { servers, tools };
  }

  /**
   * 按工具名调用。可显式指定 server；未指定时按工具名在所有 server 中解析。
   * @returns {Promise<object>} 归一化结果，永远带 isError 字段。
   */
  async callTool({ name, arguments: args, server: serverId }) {
    let candidates = this.servers;
    if (serverId) {
      candidates = candidates.filter((s) => s.id === serverId);
      if (candidates.length === 0) {
        return {
          server: serverId,
          tool: name,
          isError: true,
          error: `未找到 MCP server: ${serverId}`,
        };
      }
    }

    // 未指定 server 时，按工具名归并：找出真正提供该工具的 server。
    if (!serverId) {
      const matched = [];
      for (const server of candidates) {
        try {
          const tools = await server.listTools();
          if (tools.some((t) => t.name === name)) matched.push(server);
        } catch {
          /* 连不上的 server 跳过，不挡其它 server */
        }
      }
      if (matched.length === 0) {
        return {
          tool: name,
          isError: true,
          error: `没有任何已连接的 MCP server 提供工具「${name}」`,
        };
      }
      if (matched.length > 1) {
        return {
          tool: name,
          isError: true,
          error: `多个 MCP server 提供同名工具「${name}」，请用 server 参数指定：${matched
            .map((s) => s.id)
            .join(', ')}`,
        };
      }
      candidates = matched;
    }

    const server = candidates[0];
    try {
      const result = await server.callTool(name, args);
      return {
        server: server.id,
        tool: name,
        isError: Boolean(result.isError),
        content: result.content ?? [],
        ...(result.structuredContent !== undefined
          ? { structuredContent: result.structuredContent }
          : {}),
      };
    } catch (err) {
      await server.disconnect();
      return { server: server.id, tool: name, isError: true, error: err.message };
    }
  }
}
