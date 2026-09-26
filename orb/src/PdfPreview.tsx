import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { getDocument, GlobalWorkerOptions, type PDFDocumentProxy, type RenderTask } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";

GlobalWorkerOptions.workerSrc = workerUrl;

/** Render one page at a time; no browser PDF plugin required in the Tauri webview. */
export default function PdfPreview(p: { name: string; load: (signal: AbortSignal) => Promise<Uint8Array>; close: () => void }) {
  const [doc, setDoc] = createSignal<PDFDocumentProxy>();
  const [page, setPage] = createSignal(1);
  const [width, setWidth] = createSignal(0);
  const [error, setError] = createSignal("");
  const [painting, setPainting] = createSignal(false);
  const abort = new AbortController();
  let root!: HTMLElement, viewport!: HTMLDivElement, canvas!: HTMLCanvasElement;
  const go = (number: number) => { if (doc()) setPage(Math.max(1, Math.min(number, doc()!.numPages))); };
  const keydown = (event: KeyboardEvent) => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey || (event.target as HTMLElement).closest("input, textarea, select, [contenteditable=true]")) return;
    const next = ({ArrowLeft:page()-1, ArrowRight:page()+1, PageUp:page()-1, PageDown:page()+1, Home:1, End:doc()?.numPages ?? 1} as Record<string, number>)[event.key];
    if (next === undefined) return;
    event.preventDefault(); event.stopPropagation(); go(next); root.focus({preventScroll:true});
  };
  let loading: ReturnType<typeof getDocument> | undefined;
  let rendering: RenderTask | undefined;
  let revision = 0;
  onMount(() => {
    root.focus({preventScroll:true});
    const observer = new ResizeObserver(([entry]) => setWidth(Math.max(1, entry.contentRect.width)));
    observer.observe(viewport);
    onCleanup(() => observer.disconnect());
    void (async () => {
      try {
        const bytes = await p.load(abort.signal);
        if (abort.signal.aborted) return;
        loading = getDocument({ data: bytes });
        const document = await loading.promise;
        if (!abort.signal.aborted) setDoc(document);
      } catch (e) { if (!abort.signal.aborted) setError(e instanceof Error ? e.message : String(e)); }
    })();
  });
  createEffect(() => {
    const document = doc(), number = page(), available = width();
    const current = ++revision;
    const previous = rendering;
    previous?.cancel();
    if (!document || !available) return;
    setPainting(true);
    void (async () => {
      try {
        // A cancelled render must release the canvas before it can be reused.
        await previous?.promise.catch(() => {});
        const pdfPage = await document.getPage(number);
        if (current !== revision || abort.signal.aborted) return;
        const scale = Math.min(available / pdfPage.getViewport({scale:1}).width, 2);
        const view = pdfPage.getViewport({scale});
        const ratio = Math.min(window.devicePixelRatio || 1, 2);
        canvas.width = Math.ceil(view.width * ratio); canvas.height = Math.ceil(view.height * ratio);
        canvas.style.width = `${view.width}px`; canvas.style.height = `${view.height}px`;
        rendering = pdfPage.render({ canvas, viewport: view, transform: [ratio,0,0,ratio,0,0] });
        await rendering.promise;
        if (current === revision) { setPainting(false); viewport.scrollTop = 0; }
      } catch (e) {
        if (current === revision && !abort.signal.aborted) { setPainting(false); setError(e instanceof Error ? e.message : String(e)); }
      }
    })();
  });
  onCleanup(() => { abort.abort(); revision++; rendering?.cancel(); void loading?.destroy(); });
  return <section class="pdf-viewer" ref={root} tabIndex={0} onKeyDown={keydown} aria-label={`PDF: ${p.name}`} aria-keyshortcuts="ArrowLeft ArrowRight PageUp PageDown Home End">
    <div class="pdf-toolbar" role="toolbar" aria-label="PDF controls">
      <button class="s-btn pdf-back" title="Back to file details" aria-label="Back" onClick={p.close}>‹</button>
      <div class="pdf-pagination">
        <button class="s-btn" aria-label="Previous page" disabled={!doc() || page() <= 1} title="Previous page (← or Page Up)" onClick={() => { go(page()-1); root.focus({preventScroll:true}); }}>‹</button>
        <span aria-live="polite">{doc() ? `${page()} / ${doc()!.numPages}` : "Loading…"}</span>
        <button class="s-btn" aria-label="Next page" disabled={!doc() || page() >= doc()!.numPages} title="Next page (→ or Page Down)" onClick={() => { go(page()+1); root.focus({preventScroll:true}); }}>›</button>
      </div>
    </div>
    <Show when={error()}><p class="file-muted" role="alert">Couldn’t display this PDF. {error()}</p></Show>
    <div class="pdf-pages" ref={viewport} aria-busy={!doc() || painting()}>
      <Show when={!doc() && !error()}><p class="file-muted">Loading PDF…</p></Show>
      <canvas onPointerDown={() => root.focus({preventScroll:true})} ref={canvas} aria-label={`Page ${page()} of ${p.name}`} style={{visibility: doc() && !error() ? "visible" : "hidden"}} />
    </div>
  </section>;
}
