// MCP 客户端（sidecar 侧）。
//
// 职责：用官方 `@modelcontextprotocol/sdk` 承载 MCP 协议细节，把用户配置的
// 多个 MCP server（stdio / streamable HTTP）统一成一个工具池，供 Rust 侧
// 两个薄转发工具（mcp_list_tools / mcp_call）触达。
//
// 关键取舍：
// - 懒连接：只在第一次 list/call 时才 spawn/connect，避免 sidecar 启动即拉起一堆进程。
// - 连接缓存 + 失败重置：成功缓存 Client，失败清掉，下次重试；绝不缓存一个坏连接。
// - 工具名跨 server 归并：list 时带 server 归属；call 时若同名多命则要求显式指定 server。

import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';

const CLIENT_INFO = { name: 'pc-guardian-sidecar', version: '0.1.0' };

/**
 * 从环境变量或文件加载 MCP server 配置。
 * 优先级：SIDECAR_MCP_SERVERS（JSON 数组）> ./mcp-servers.json > 空数组。
 */
export function loadMcpServers() {
  if (process.env.SIDECAR_MCP_SERVERS) {
    try {
      return JSON.parse(process.env.SIDECAR_MCP_SERVERS);
    } catch (err) {
      console.error('[mcp] 解析 SIDECAR_MCP_SERVERS 失败，回退文件/空配置:', err.message);
    }
  }
  // 文件读取交给上层（server.js）注入，避免这里隐式依赖 fs 路径。
  return [];
}

function transportFor(config) {
  if (config.transport === 'http' || config.url) {
    return new StreamableHTTPClientTransport(new URL(config.url));
  }
  // 默认 stdio
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
  constructor(config) {
    this.id = config.id ?? config.name ?? `server-${Math.random().toString(36).slice(2, 8)}`;
    this.name = config.name ?? this.id;
    this.transport = config.transport ?? (config.url ? 'http' : 'stdio');
    this.config = config;
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
  constructor(servers) {
    this.servers = (servers ?? []).map((c) => new ServerRuntime(c));
  }

  get count() {
    return this.servers.length;
  }

  status() {
    return this.servers.map((s) => s.snapshot());
  }

  /**
   * 拉取全部 server 的工具，按 server 归组并摊平。
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
        return { server: serverId, tool: name, isError: true, error: `未找到 MCP server: ${serverId}` };
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
        return { tool: name, isError: true, error: `没有任何已连接的 MCP server 提供工具「${name}」` };
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
        ...(result.structuredContent !== undefined ? { structuredContent: result.structuredContent } : {}),
      };
    } catch (err) {
      await server.disconnect();
      return { server: server.id, tool: name, isError: true, error: err.message };
    }
  }
}
