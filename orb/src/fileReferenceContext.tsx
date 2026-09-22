import {
  createContext,
  useContext,
  For,
  Show,
  createSignal,
  createEffect,
  onMount,
  onCleanup,
  type JSX,
} from "solid-js";
import {
  parseFileTarget,
  splitFileReferences,
  type FileRef,
} from "./fileResources";
export interface ReferenceResolver {
  loadImage?: (path: string) => Promise<string | null>;
  resolve: (raw: string) => Promise<FileRef[]>;
  open: (refs: FileRef[]) => void;
  search: (query: string) => void;
}
export const FileReferenceContext = createContext<ReferenceResolver>();
export function FileReference(p: { raw: string; children?: JSX.Element }) {
  const resolver = useContext(FileReferenceContext);
  const target = () => parseFileTarget(p.raw);
  const [matches, setMatches] = createSignal<FileRef[]>([]);
  let el: HTMLSpanElement | undefined;
  const [visible, setVisible] = createSignal(false);
  onMount(() => {
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          observer.disconnect();
          setVisible(true);
        }
      },
      { rootMargin: "200px" },
    );
    observer.observe(el!);
    onCleanup(() => observer.disconnect());
  });
  createEffect(() => {
    const raw = p.raw;
    setMatches([]);
    if (!resolver || !visible() || !target()) return;
    let cancelled = false;
    void resolver
      .resolve(raw)
      .then((r) => {
        if (!cancelled) setMatches(r);
      })
      .catch(() => {});
    onCleanup(() => {
      cancelled = true;
    });
  });
  return (
    <span
      ref={el}
      onContextMenu={(e) => {
        if (resolver && /…|\.\.\./.test(p.raw)) {
          e.preventDefault();
          resolver.search(p.raw.split("/").at(-1) ?? p.raw);
        }
      }}
    >
      <Show when={matches().length} fallback={p.children ?? p.raw}>
        <button
          class="file-reference"
          title={matches()
            .map((r) => `${r.source}: ${r.path}`)
            .join("\n")}
          onClick={() => resolver?.open(matches())}
        >
          {p.children ?? p.raw}
        </button>
      </Show>
    </span>
  );
}
export function FileReferenceText(p: { text: string }) {
  const resolver = useContext(FileReferenceContext);
  return (
    <>
      {resolver ? (
        <For each={splitFileReferences(p.text)}>
          {(part) =>
            part.target ? <FileReference raw={part.text} /> : part.text
          }
        </For>
      ) : (
        p.text
      )}
    </>
  );
}
