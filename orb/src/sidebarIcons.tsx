// SVG paths from Lucide (ISC). See ../licenses/lucide.txt.
import type { JSX } from "solid-js";
type Props = { size?: number; class?: string };
const Icon = (p: Props & { children: JSX.Element }) => <svg aria-hidden="true" class={p.class} width={p.size ?? 18} height={p.size ?? 18} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">{p.children}</svg>;

export const Bot = (p: Props) => <Icon {...p}><path d="M12 8V4H8" />
  <rect width="16" height="12" x="4" y="8" rx="2" />
  <path d="M2 14h2" />
  <path d="M20 14h2" />
  <path d="M15 13v2" />
  <path d="M9 13v2" /></Icon>;

export const LoaderCircle = (p: Props) => <Icon {...p}><path d="M21 12a9 9 0 1 1-6.219-8.56" /></Icon>;

export const Pause = (p: Props) => <Icon {...p}><rect x="14" y="3" width="5" height="18" rx="1" />
  <rect x="5" y="3" width="5" height="18" rx="1" /></Icon>;

export const CircleCheck = (p: Props) => <Icon {...p}><circle cx="12" cy="12" r="10" />
  <path d="m16 9-5.5 5.5L8 12" /></Icon>;

export const CircleAlert = (p: Props) => <Icon {...p}><circle cx="12" cy="12" r="10" />
  <line x1="12" x2="12" y1="8" y2="12" />
  <line x1="12" x2="12.01" y1="16" y2="16" /></Icon>;

export const Clock = (p: Props) => <Icon {...p}><circle cx="12" cy="12" r="10" />
  <path d="M12 6v6l4 2" /></Icon>;

export const MessageCircle = (p: Props) => <Icon {...p}><path d="M2.992 16.342a2 2 0 0 1 .094 1.167l-1.065 3.29a1 1 0 0 0 1.236 1.168l3.413-.998a2 2 0 0 1 1.099.092 10 10 0 1 0-4.777-4.719" /></Icon>;

export const Folder = (p: Props) => <Icon {...p}><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" /></Icon>;

export const FolderOpen = (p: Props) => <Icon {...p}><path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" /></Icon>;

export const Server = (p: Props) => <Icon {...p}><rect width="20" height="8" x="2" y="2" rx="2" ry="2" />
  <rect width="20" height="8" x="2" y="14" rx="2" ry="2" />
  <line x1="6" x2="6.01" y1="6" y2="6" />
  <line x1="6" x2="6.01" y1="18" y2="18" /></Icon>;

export const ChevronRight = (p: Props) => <Icon {...p}><path d="m9 18 6-6-6-6" /></Icon>;

export const Archive = (p: Props) => <Icon {...p}><rect width="20" height="5" x="2" y="3" rx="1" />
  <path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8" />
  <path d="M10 12h4" /></Icon>;
