import type { StreamItem } from "./transcriptModel";

type Tool = Extract<StreamItem, { kind: "tool" }>;
export type TaskStatus = "pending" | "in_progress" | "completed" | "cancelled";
export interface TaskItem { text: string; status: TaskStatus }
export interface Checklist { key: string; tasks: TaskItem[] }

export function toolName(name: string): string {
  return name.replace(/^functions\./, "").toLowerCase();
}
export function toolArgs(value: unknown): Record<string, unknown> | null {
  if (typeof value === "string") { try { value = JSON.parse(value); } catch { return null; } }
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

/** Exact harness adapters, not a search for task-like text in arbitrary results. */
export function parseChecklist(name: string, input: unknown): TaskItem[] | null {
  const normalized = toolName(name), args = toolArgs(input);
  if (!args || !["todowrite", "update_plan"].includes(normalized)) return null;
  const codex = normalized === "update_plan";
  const list = args[codex ? "plan" : "todos"];
  if (!Array.isArray(list) || list.length > 500) return null;
  const tasks: TaskItem[] = [];
  for (const entry of list) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) return null;
    const text = entry[codex ? "step" : "content"], status = entry.status;
    if (typeof text !== "string" || !text.trim() || typeof status !== "string") return null;
    // OpenCode supports cancellation; Claude and Codex only advertise three states.
    const allowed = name === "todowrite" ? ["pending", "in_progress", "completed", "cancelled"] : ["pending", "in_progress", "completed"];
    if (!allowed.includes(status)) return null;
    tasks.push({ text, status: status as TaskStatus });
  }
  return tasks;
}
export function latestChecklist(items: StreamItem[]): Checklist | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.kind !== "tool") continue;
    const tasks = parseChecklist(item.name, item.args);
    if (tasks !== null) return { key: item.key, tasks };
  }
  return null;
}

type Kind = "read" | "search" | "command" | "edit" | "other";
export function workKind(name: string): Kind {
  switch (toolName(name)) {
    case "read": case "read_file": case "readfile": case "workspace_read_file": return "read";
    case "grep": case "glob": case "search": case "websearch": case "web_search": case "list_files": return "search";
    case "bash": case "shell": case "shell_command": case "exec_command": case "run_terminal_command": return "command";
    case "edit": case "write": case "multiedit": case "apply_patch": case "write_file": case "edit_file": return "edit";
    default: return "other";
  }
}
const fileTarget = (tool: Tool): string | null => {
  const args = toolArgs(tool.args);
  if (!args) return null;
  for (const key of ["file_path", "filePath", "path", "file"]) {
    const value = args[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
};
export function workSummary(items: StreamItem[]): string {
  const groups: Record<Kind, Tool[]> = { read: [], search: [], command: [], edit: [], other: [] };
  for (const item of items) if (item.kind === "tool") groups[workKind(item.name)].push(item);
  const parts: string[] = [];
  const count = (n: number, single: string, plural = `${single}s`) => `${n} ${n === 1 ? single : plural}`;
  for (const kind of ["read", "search", "command", "edit", "other"] as const) {
    const tools = groups[kind]; if (!tools.length) continue;
    if (kind === "read" || kind === "edit") {
      const targets = tools.map(fileTarget);
      parts.push(targets.every((target): target is string => target !== null)
        ? `${kind === "read" ? "Read" : "Edited"} ${count(new Set(targets).size, "file")}`
        : count(tools.length, kind));
    } else parts.push(count(tools.length, kind === "other" ? "other tool" : kind, kind === "search" ? "searches" : undefined));
  }
  return parts.length ? parts.join(" · ") : "Thought";
}
