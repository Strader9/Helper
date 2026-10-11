import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./WebStudio.css";

const MAX_FILE_BYTES = 30 * 1024 * 1024;
// 清洗与梳理都是纯规则处理，十万字也是毫秒级；这里只做一次兜底上限。
const MAX_REQUIREMENT_CHARS = 300_000;
const PREVIEW_CSP = '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; img-src data: blob:; style-src \'unsafe-inline\'; script-src \'unsafe-inline\'; font-src data:; connect-src \'none\'; form-action \'none\'; base-uri \'none\'">';

interface GenerationEvent {
  generation_id: string;
  event_type: "log" | "progress" | "chunk" | "complete" | "error" | "spec" | "digest";
  message: string;
  html?: string | null;
  delta?: string | null;
  spec?: string | null;
  digest?: string | null;
}

interface GenerationLog {
  id: number;
  time: string;
  message: string;
}

function previewDocument(html: string): string {
  if (/<head[^>]*>/i.test(html)) return html.replace(/<head([^>]*)>/i, `<head$1>${PREVIEW_CSP}`);
  return html.replace(/<html([^>]*)>/i, `<html$1><head>${PREVIEW_CSP}</head>`);
}

async function readRequirementFile(file: File): Promise<string> {
  if (file.size > MAX_FILE_BYTES) throw new Error("文件不能超过 30 MB。");
  const extension = file.name.split(".").pop()?.toLowerCase();
  if (extension === "txt" || extension === "md" || extension === "markdown") {
    return file.text();
  }
  if (extension === "docx") {
    const { strFromU8, unzipSync } = await import("fflate");
    const archive = unzipSync(new Uint8Array(await file.arrayBuffer()), {
      filter: (entry) => entry.name === "word/document.xml" && entry.originalSize <= 20 * 1024 * 1024,
    });
    const documentXml = archive["word/document.xml"];
    if (!documentXml) throw new Error("无法在 DOCX 文件中找到正文。");
    const xml = new DOMParser().parseFromString(strFromU8(documentXml), "application/xml");
    if (xml.querySelector("parsererror")) throw new Error("DOCX 文档结构无效。");
    return Array.from(xml.getElementsByTagNameNS("*", "p"))
      .map((paragraph) => Array.from(paragraph.getElementsByTagNameNS("*", "t")).map((part) => part.textContent || "").join(""))
      .join("\n");
  }
  if (extension === "pdf") {
    const pdfjs = await import("pdfjs-dist");
    pdfjs.GlobalWorkerOptions.workerSrc = new URL(
      "pdfjs-dist/build/pdf.worker.min.mjs",
      import.meta.url,
    ).toString();
    const document = await pdfjs.getDocument({ data: new Uint8Array(await file.arrayBuffer()) }).promise;
    const pages: string[] = [];
    for (let pageNumber = 1; pageNumber <= document.numPages; pageNumber += 1) {
      const page = await document.getPage(pageNumber);
      const content = await page.getTextContent();
      pages.push(content.items.map((item) => "str" in item ? item.str : "").join(" "));
    }
    return pages.join("\n\n");
  }
  throw new Error("暂支持 PDF、DOCX、Markdown 和 TXT 文档。");
}

function WebStudio() {
  const fileInput = useRef<HTMLInputElement>(null);
  const [requirements, setRequirements] = useState("");
  const [fileName, setFileName] = useState("");
  const [feedback, setFeedback] = useState("");
  const [html, setHtml] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [generationLogs, setGenerationLogs] = useState<GenerationLog[]>([]);
  const [streamedOutput, setStreamedOutput] = useState("");
  const [spec, setSpec] = useState("");
  const [digest, setDigest] = useState("");
  const [publishedUrl, setPublishedUrl] = useState("");
  const [publishedProvider, setPublishedProvider] = useState("");
  const [publishing, setPublishing] = useState(false);
  const [publishError, setPublishError] = useState("");
  const [copied, setCopied] = useState(false);
  const activeGenerationId = useRef("");
  /** 已经发布过的内容，避免同一份产物反复推送。 */
  const publishedHtml = useRef("");
  /** 事件回调里要读最新文件名，state 会过期。 */
  const fileNameRef = useRef("");

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen<GenerationEvent>("webpage-generation:event", ({ payload }) => {
      if (payload.generation_id !== activeGenerationId.current) return;
      const delta = payload.delta;
      if (delta) setStreamedOutput((existing) => `${existing}${delta}`.slice(-200_000));
      if (payload.event_type !== "chunk") {
        setGenerationLogs((existing) => [...existing, {
          id: Date.now() + existing.length,
          time: new Date().toLocaleTimeString(),
          message: payload.message,
        }].slice(-300));
      }
      if (payload.event_type === "spec") {
        setSpec(payload.spec ?? "");
      }
      if (payload.event_type === "digest") {
        setDigest(payload.digest ?? "");
      }
      if (payload.event_type === "complete") {
        if (payload.html) {
          setHtml(payload.html);
          setStreamedOutput(payload.html);
        }
        setFeedback("");
        setLoading(false);
      } else if (payload.event_type === "error") {
        setError(payload.message);
        setLoading(false);
      }
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    fileNameRef.current = fileName.replace(/\.[^.]+$/, "") || "generated-webpage";
  }, [fileName]);

  // 生成完就发布到公网，地址栏里直接就是链接 —— 不用再点一次「导出」。
  useEffect(() => {
    if (!html || loading || publishedHtml.current === html) return;
    publishedHtml.current = html;
    void publish(html);
  }, [html, loading]);

  /** 把当前这份 HTML 发布出去，拿到公网链接。 */
  async function publish(source: string) {
    if (!source.trim()) return;
    setPublishing(true);
    setPublishError("");
    try {
      const outcome = await invoke<{ url: string; provider: string }>("publish_webpage", {
        html: source,
        fileName: fileNameRef.current,
      });
      setPublishedUrl(outcome.url);
      setPublishedProvider(outcome.provider);
    } catch (reason) {
      setPublishedUrl("");
      setPublishedProvider("");
      setPublishError(String(reason));
    } finally {
      setPublishing(false);
    }
  }

  async function copyAddress() {
    try {
      await navigator.clipboard.writeText(publishedUrl);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setPublishError("复制失败，请手动选中地址复制。");
    }
  }

  function openAddress() {
    void invoke("open_external", { url: publishedUrl }).catch((reason) => setPublishError(String(reason)));
  }

  async function handleFile(file?: File) {
    if (!file) return;
    setError("");
    try {
      const text = (await readRequirementFile(file)).trim();
      if (!text) throw new Error("文档中没有提取到文字内容；扫描版 PDF 暂不支持 OCR。");
      if (text.length > MAX_REQUIREMENT_CHARS) throw new Error(`提取内容超过 ${(MAX_REQUIREMENT_CHARS / 10_000).toFixed(0)} 万字，请精简文档后再导入。`);
      setRequirements(text);
      setFileName(file.name);
      setHtml("");
      resetDerived();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  /** 需求原文变了，梳理稿和规格都作废。 */
  function resetDerived() {
    setDigest("");
    setSpec("");
  }

  /** 梳理稿被手工改过，规格需要重新推导，但仍会复用这份梳理稿。 */
  function handleDigestChange(value: string) {
    setDigest(value);
    setSpec("");
  }

  async function generate() {
    setError("");
    setLoading(true);
    const generationId = crypto.randomUUID();
    activeGenerationId.current = generationId;
    setStreamedOutput("");
    setGenerationLogs([{ id: Date.now(), time: new Date().toLocaleTimeString(), message: "准备启动网页生成任务。" }]);
    try {
      await invoke("start_webpage_generation", {
        generationId,
        requirements,
        feedback: feedback.trim() || null,
        currentHtml: html || null,
        // 三级短路：已加工好的规格 > 编辑过的梳理稿 > 完整流水线。
        normalizedSpec: spec || null,
        digest: digest || null,
      });
    } catch (reason) {
      setError(String(reason));
      setLoading(false);
      setGenerationLogs((existing) => [...existing, { id: Date.now(), time: new Date().toLocaleTimeString(), message: `任务启动失败：${String(reason)}` }]);
    }
  }

  function exportHtml() {
    if (!html) return;
    const blob = new Blob([html], { type: "text/html;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${fileName.replace(/\.[^.]+$/, "") || "generated-webpage"}.html`;
    anchor.click();
    URL.revokeObjectURL(url);
  }

  return (
    <div className="page-container web-studio-page">
      <header className="page-header">
        <div>
          <h1>网页工坊</h1>
          <p className="page-subtitle">从需求文档直接生成网页，自动发布成可分享的公网网址</p>
        </div>
        <span className="studio-model-chip"><i /> 当前配置的 Ollama 模型</span>
      </header>

      <div className="studio-workspace">
        <section className="section-card studio-brief">
          <div className="studio-section-heading">
            <div><span className="studio-step">01</span><h2>需求文档</h2></div>
            <span className="studio-muted">PDF · DOCX · MD · TXT</span>
          </div>
          <input ref={fileInput} className="studio-file-input" type="file" accept=".pdf,.docx,.md,.markdown,.txt" onChange={(event) => { void handleFile(event.target.files?.[0]); event.currentTarget.value = ""; }} />
          <button className="studio-upload" onClick={() => fileInput.current?.click()}>
            <span className="studio-upload-icon">↑</span>
            <strong>{fileName || "选择需求文档"}</strong>
            <span>点击选择文件，或直接粘贴需求内容</span>
          </button>
          <textarea
            className="studio-requirements"
            value={requirements}
            onChange={(event) => { setRequirements(event.target.value); resetDerived(); }}
            placeholder="例如：做一个个人记账网页，支持收入支出记录、分类统计、月度趋势图，整体简洁明亮……"
            aria-label="网页需求内容"
          />
          <div className="studio-count">{requirements.length.toLocaleString()} / {MAX_REQUIREMENT_CHARS.toLocaleString()}</div>
          {digest && (
            <details className="studio-digest" open={digest.length < 3_000}>
              <summary>梳理稿 · {digest.length.toLocaleString()} 字（可直接编辑，改完再生成）</summary>
              <textarea
                className="studio-digest-editor"
                value={digest}
                onChange={(event) => handleDigestChange(event.target.value)}
                aria-label="梳理稿，可编辑"
              />
              <p className="studio-digest-note">这是从原文档中筛出的需求相关内容，模型只会读到它。有遗漏可以直接补在这里。</p>
            </details>
          )}
          {error && <div className="studio-error" role="alert">{error}</div>}
          <button className="studio-generate" disabled={loading || !requirements.trim() || requirements.length > MAX_REQUIREMENT_CHARS} onClick={generate}>
            {loading ? <><span className="studio-spinner" />正在生成网页…</> : <><span>✦</span> 根据文档生成网页</>}
          </button>
          <p className="studio-local-note">导入的文档会先清洗（去页眉页脚、拼回硬换行）再由规则梳理成"梳理稿"，模型只对梳理稿做一次加工并生成网页，所以 PDF / DOCX 的排版不影响结果，十万字的文档也能处理。文档会发送到设置中配置的 Ollama 地址；若配置远程服务，文档也会发送到该服务。生成没有固定时间上限，请保持本页和 PC Guardian 开启；模型无法访问电脑文件或运行命令。</p>
        </section>

        <section className="section-card studio-preview-card">
          <div className="studio-section-heading">
            <div><span className="studio-step">02</span><h2>网页预览</h2></div>
            <div className="studio-address" title={publishError || publishedUrl || undefined}>
              <span className={`studio-address-dot ${publishedUrl ? "is-live" : publishing ? "is-pending" : "is-off"}`} />
              {publishedUrl && publishedProvider && (
                <span className="studio-address-provider">{publishedProvider}</span>
              )}
              {publishedUrl ? (
                <input
                  className="studio-address-input"
                  readOnly
                  value={publishedUrl}
                  onFocus={(event) => event.currentTarget.select()}
                  aria-label="已发布的网页地址"
                />
              ) : (
                <span className="studio-address-text">
                  {publishing
                    ? "正在发布到公网…"
                    : publishError || (html ? "尚未发布" : "生成完成后这里显示公网网址")}
                </span>
              )}
              {publishedUrl && (
                <button className="studio-address-button" onClick={() => void copyAddress()}>
                  {copied ? "已复制" : "复制"}
                </button>
              )}
              {publishedUrl && (
                <button className="studio-address-button" onClick={openAddress}>打开</button>
              )}
              {!publishing && !publishedUrl && publishError && (
                <button className="studio-address-button" onClick={() => void publish(html)}>重试</button>
              )}
              {html && (
                <button className="studio-address-button" onClick={exportHtml} title="下载 HTML 文件">下载</button>
              )}
            </div>
          </div>
          {publishError && (
            <p className="studio-address-note is-error">发布失败：{publishError}</p>
          )}
          {publishedUrl.includes("github.io") && (
            <p className="studio-address-note">
              链接已固定，以后每次生成都覆盖同一个仓库，网址不变。GitHub Pages 首次开启需要构建几十秒到一分钟，这期间打开会显示 404，稍后刷新即可。
            </p>
          )}
          {spec && (
            <details className="studio-spec-details">
              <summary>标准格式规格 · {spec.length.toLocaleString()} 字（模型实际读到的内容）</summary>
              <pre>{spec}</pre>
            </details>
          )}
          {html ? (
            <>
              <div className="studio-browser-bar"><span /><span /><span /><label>PC Guardian Preview</label></div>
              <iframe className="studio-preview" title="生成网页预览" srcDoc={previewDocument(html)} sandbox="allow-scripts" />
              <details className="studio-source-details" open={loading}><summary>查看模型生成的 HTML 源码</summary><pre>{streamedOutput || html}</pre></details>
              {generationLogs.length > 0 && <div className="studio-live-log" aria-live="polite"><div className="studio-log-heading"><span>实时生成日志</span><span>{loading ? "生成中" : "已完成"}</span></div><div className="studio-log-list">{generationLogs.map((entry) => <div className="studio-log-entry" key={entry.id}><time>{entry.time}</time><span>{entry.message}</span></div>)}</div></div>}
              <div className="studio-refine">
                <input value={feedback} onChange={(event) => setFeedback(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey && feedback.trim() && !loading) void generate(); }} placeholder="继续描述修改，例如：把主色调改成绿色" />
                <button disabled={loading || !feedback.trim()} onClick={generate}>{loading ? "生成中" : "继续修改 ↗"}</button>
              </div>
            </>
          ) : generationLogs.length > 0 ? (
            <div className="studio-generation-stage">
              <div className="studio-live-log studio-live-log-empty" aria-live="polite"><div className="studio-log-heading"><span>实时生成日志</span><span>{loading ? "生成中" : "已停止"}</span></div><div className="studio-log-list">{generationLogs.map((entry) => <div className="studio-log-entry" key={entry.id}><time>{entry.time}</time><span>{entry.message}</span></div>)}</div></div>
              <div className="studio-stream-output"><div className="studio-log-heading"><span>模型正在生成的 HTML</span><span>{streamedOutput.length.toLocaleString()} 字符</span></div><pre>{streamedOutput || "等待模型开始输出……"}</pre></div>
            </div>
          ) : (
            <div className="studio-empty">
              <div className="studio-empty-art"><span>⌘</span><i /><i /><i /></div>
              <strong>网页预览会显示在这里</strong>
              <p>导入需求文档后开始生成。生成完成会自动发布到公网，这里会直接给出一个能打开的网址。</p>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}

export default WebStudio;
