// Tauri 适配层：替换 EAssistantCore 的 apiClient，改用 Tauri invoke
import { invoke } from '@tauri-apps/api/core';

const LOCAL_BACKEND_ORIGIN = 'http://127.0.0.1:3000';

export function apiUrl(path: string): string {
  if (/^https?:\/\//i.test(path)) return path;
  const normalized = path.startsWith('/') ? path : `/${path}`;
  return `${LOCAL_BACKEND_ORIGIN}${normalized}`;
}

export async function apiFetch<T = any>(endpoint: string, options?: RequestInit): Promise<T> {
  // 优先尝试 Tauri invoke，失败则返回空数据避免 UI 崩溃
  const path = endpoint.replace(/^\/api\//, '');
  try {
    const result = await invoke<any>('api_proxy', { path, options });
    return result as T;
  } catch {
    return {} as T;
  }
}

export const apiClient = {
  get: <T = any>(url: string) => apiFetch<T>(url, { method: 'GET' }),
  post: <T = any>(url: string, body?: any) => apiFetch<T>(url, { method: 'POST', body: JSON.stringify(body) }),
  put: <T = any>(url: string, body?: any) => apiFetch<T>(url, { method: 'PUT', body: JSON.stringify(body) }),
  delete: <T = any>(url: string) => apiFetch<T>(url, { method: 'DELETE' }),
};
