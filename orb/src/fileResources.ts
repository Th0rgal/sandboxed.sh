import {
  api,
  getApiUrl,
  listProjectFiles,
  readProjectFile,
  getProjectController,
  getProjectCron,
  getJwt,
  type Mission,
} from "./api";
import { localBinding } from "./localAgents";
export interface FileScope {
  mission?: Mission | null;
  project?: string;
  controller?: string;
}
export interface FileSource {
  id: string;
  label: string;
  path?: string;
  machine?: string;
  available: boolean;
  legacy?: boolean;
  local?: boolean;
}
export interface FileEntry {
  name: string;
  path: string;
  kind: string;
  size?: number;
  modified?: number;
}
export interface FileRef extends FileEntry {
  source: string;
  line?: number;
}
export interface FileRead {
  content: string | null;
  binary: boolean;
  size: number;
  truncated: boolean;
  modified?: number;
}
export interface FileReply {
  sources?: FileSource[];
  entries?: FileEntry[];
  results?: { reference: string; matches: FileEntry[] }[];
  content?: string | null;
  binary?: boolean;
  size?: number;
  truncated?: boolean;
  bytes?: number[];
  next?: number;
}
export interface FileOp {
  action: string;
  path?: string;
  query?: string;
  paths?: string[];
  offset?: number;
}
export const fileScopeKey = (s: FileScope) =>
  `${getApiUrl()}:${s.mission?.id ?? `project:${s.project ?? ""}:controller:${s.controller ?? ""}`}${s.mission?.machine_transfer ? `:transfer:${s.mission.machine_transfer.id}` : ""}`;
export function createFileClient(scope: FileScope) {
  const project = scope.mission?.project ?? scope.project;
  const binding = scope.mission && (!scope.mission.machine_transfer || scope.mission.machine_transfer.destination.kind === "client") ? localBinding(scope.mission.id) : undefined;
  const server = (source: string, op: FileOp) =>
    api<FileReply>("/api/file-resources", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        mission_id: scope.mission?.id,
        project,
        source,
        ...op,
      }),
    });
  let sources: FileSource[] = [];
  let controllerText: string | undefined;
  const artifacts = new Map<string, string>();

  async function roots() {
    try {
      sources = (await server("", { action: "roots" })).sources ?? [];
    } catch (e) {
      // Only a missing route enables compatibility. Auth/network errors remain errors.
      if (!(e instanceof Error) || !/404|405/.test(e.message)) throw e;
      sources = project
        ? [
            {
              id: "context",
              label: "Project context · current",
              available: true,
              legacy: true,
            },
          ]
        : [];
      if (scope.mission && !binding)
        sources.unshift({
          id: "unavailable",
          label: "Workspace · backend update required",
          available: false,
        });
    }
    if (binding)
      sources.unshift({
        id: "local",
        label: "Workspace · this computer",
        path: binding.cwd,
        available: true,
        local: true,
      });
    if (project) {
      try {
        const c = scope.controller
          ? await getProjectCron(project, scope.controller)
          : await getProjectController(project, 1);
        if (c.settings?.prompt) {
          controllerText = c.settings.prompt;
          sources.push({
            id: "controller",
            label: "Controller · current",
            available: true,
          });
        }
      } catch {
        /* No explicitly associated controller. */
      }
    }
    if (scope.mission) {
      try {
        const rows = await api<unknown>(
          `/api/control/missions/${scope.mission.id}/events?limit=4000`,
        );
        const events = Array.isArray(rows)
          ? rows
          : ((rows as { events?: unknown[] }).events ?? []);
        for (const row of events as {
          metadata?: {
            shared_files?: { name?: string; path?: string; url: string }[];
          };
        }[])
          for (const file of row.metadata?.shared_files ?? []) {
            const url = new URL(file.url, getApiUrl());
            if (
              url.origin !== new URL(getApiUrl()).origin ||
              url.pathname !== "/api/fs/download"
            )
              continue;
            const name =
              file.name ??
              file.path?.split("/").at(-1) ??
              url.searchParams.get("path")?.split("/").at(-1) ??
              "artifact";
            artifacts.set(name, url.toString());
          }
        if (artifacts.size)
          sources.push({
            id: "artifacts",
            label: "Published artifacts",
            available: true,
          });
      } catch {
        /* Artifacts unavailable; workspace reads remain available. */
      }
    }
    return sources;
  }
  async function call(source: string, op: FileOp): Promise<FileReply> {
    const root = sources.find((s) => s.id === source);
    if (!root?.available) throw new Error("File source is unavailable");
    if (source === "controller" || source === "artifacts") {
      const entries: FileEntry[] =
        source === "controller"
          ? [{ name: "controller.md", path: "controller.md", kind: "file" }]
          : [...artifacts.keys()].map((name) => ({
              name,
              path: name,
              kind: "file",
            }));
      if (op.action === "list") return { entries };
      if (op.action === "search")
        return {
          entries: entries.filter((e) =>
            e.name.toLowerCase().includes((op.query ?? "").toLowerCase()),
          ),
        };
      if (op.action === "resolve")
        return {
          results: (op.paths ?? []).map((reference) => ({
            reference,
            matches: entries.filter((e) => e.path === reference),
          })),
        };
      if (!entries.some((e) => e.path === op.path))
        throw new Error("File unavailable");
      let bytes: Uint8Array;
      if (source === "controller")
        bytes = new TextEncoder().encode(controllerText ?? "");
      else {
        const response = await fetch(artifacts.get(op.path!)!, {
          headers: { Authorization: `Bearer ${getJwt() ?? ""}` },
        });
        if (!response.ok) throw new Error("Artifact unavailable");
        bytes = new Uint8Array(await response.arrayBuffer());
      }
      const offset = op.offset ?? 0,
        part = bytes.slice(offset, offset + 1024 * 1024);
      if (op.action === "download")
        return {
          bytes: [...part],
          size: bytes.length,
          next: offset + part.length,
        };
      return {
        content: part.includes(0) ? null : new TextDecoder().decode(part),
        binary: part.includes(0),
        truncated: bytes.length > part.length,
        size: bytes.length,
      };
    }
    if (root.local) {
      const invoke = (
        window as unknown as {
          __TAURI__?: {
            core?: {
              invoke: (name: string, args: unknown) => Promise<FileReply>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      if (!invoke || !binding)
        throw new Error("Open this mission on the computer that started it");
      return invoke("browse_local_files", {
        root: binding.cwd,
        request: { path: "", query: "", paths: [], offset: 0, ...op },
      });
    }
    if (root.legacy && project) {
      if (op.action === "list")
        return {
          entries: (await listProjectFiles(project, op.path ?? "")).map(
            (e) => ({
              ...e,
              path: [op.path, e.name].filter(Boolean).join("/"),
              modified: undefined,
            }),
          ),
        };
      if (op.action === "read") {
        const content = await readProjectFile(project, op.path ?? "");
        return {
          content,
          binary: false,
          truncated: false,
          size: new TextEncoder().encode(content).length,
        };
      }
      if (op.action === "resolve")
        return {
          results: await Promise.all(
            (op.paths ?? []).map(async (reference) => {
              try {
                const slash = reference.lastIndexOf("/");
                const entries =
                  (
                    await call(source, {
                      action: "list",
                      path: slash < 0 ? "" : reference.slice(0, slash),
                    })
                  ).entries ?? [];
                return {
                  reference,
                  matches: entries.filter(
                    (e) => e.kind === "file" && e.path === reference,
                  ),
                };
              } catch {
                return { reference, matches: [] };
              }
            }),
          ),
        };
      throw new Error(
        "Update the backend to search or download workspace files",
      );
    }
    return server(source, op);
  }
  return { roots, call };
}
export function parseFileTarget(
  raw: string,
): { path: string; line?: number } | null {
  let path = raw.trim();
  if (
    !path ||
    /^(https?:|mailto:|javascript:|data:|file:)/i.test(path) ||
    path.includes("…") ||
    path.includes("...") ||
    /[\n\r\0]/.test(path)
  )
    return null;
  const suffix = path.match(/(?::(\d+)(?::\d+)?|#L(\d+)(?:-L?\d+)?)$/);
  const line = suffix ? Number(suffix[1] ?? suffix[2]) : undefined;
  if (suffix) path = path.slice(0, -suffix[0].length);
  if (
    !/(?:^|\/)[\w. -]+\.[a-zA-Z0-9_-]{1,12}$/.test(path) ||
    /[<>|]/.test(path)
  )
    return null;
  return { path: path.replace(/^\.\//, ""), line };
}
/** Only inline prose candidates. Markdown code blocks are handled by the renderer. */
export function splitFileReferences(
  text: string,
): { text: string; target?: ReturnType<typeof parseFileTarget> }[] {
  const re =
    /(?:\/?(?:[\w.@~-]+\/)+)?[\w@~-]+(?:\.[\w-]+)+(?:#L\d+(?:-L?\d+)?|:\d+(?::\d+)?)?/g;
  const urls = [...text.matchAll(/(?:https?:\/\/|mailto:)\S+/g)].map((m) => [
    m.index!,
    m.index! + m[0].length,
  ]);
  const out: { text: string; target?: ReturnType<typeof parseFileTarget> }[] =
    [];
  let offset = 0;
  for (const m of text.matchAll(re)) {
    const start = m.index!;
    const before = text.slice(Math.max(0, start - 10), start);
    if (urls.some(([from, to]) => start >= from && start < to)) continue;
    if (
      /(?:https?:\/\/|mailto:|\w:\/\/)[^\s]*$/.test(before) ||
      text[start - 1] === "@"
    )
      continue;
    if (start > offset) out.push({ text: text.slice(offset, start) });
    out.push({ text: m[0], target: parseFileTarget(m[0]) });
    offset = start + m[0].length;
  }
  if (offset < text.length) out.push({ text: text.slice(offset) });
  return out;
}

/** Resolve document links relative to their file, without escaping the source. */
export function relativeFilePath(
  document: string,
  target: string,
): string | null {
  if (target.startsWith("/")) return target;
  const parts = document.split("/").slice(0, -1);
  for (const part of target.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (!parts.length) return null;
      parts.pop();
    } else parts.push(part);
  }
  return parts.join("/");
}
