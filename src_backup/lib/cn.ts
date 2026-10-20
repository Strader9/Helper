import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * 合并 className：clsx 处理条件与数组，twMerge 消解 Tailwind 冲突
 * （`px-2 px-4` → `px-4`）。组件一律走这里，调用方传的 className
 * 永远能覆盖内置样式。
 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
