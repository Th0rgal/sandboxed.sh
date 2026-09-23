import type { JSX } from "solid-js";

type P = { size?: number; class?: string };

const I = (p: P & { children: JSX.Element; sw?: number }) => (
  <svg
    class={p.class}
    width={p.size ?? 16}
    height={p.size ?? 16}
    viewBox="0 0 16 16"
    fill="none"
    stroke="currentColor"
    stroke-width={p.sw ?? 1.1}
    stroke-linecap="round"
    stroke-linejoin="round"
  >
    {p.children}
  </svg>
);

export const SidebarIcon = (p: P) => (
  <I {...p}>
    <rect x="2" y="3" width="12" height="10" rx="2" />
    <path d="M6 3v10" />
  </I>
);
export const SearchIcon = (p: P) => (
  <I {...p}>
    <circle cx="7.2" cy="7.2" r="4.2" />
    <path d="M10.4 10.4 13.5 13.5" />
  </I>
);
export const ArrowLeft = (p: P) => (
  <I {...p}>
    <path d="M13 8H3M7 4 3 8l4 4" />
  </I>
);
export const ArrowRight = (p: P) => (
  <I {...p}>
    <path d="M3 8h10M9 4l4 4-4 4" />
  </I>
);
export const NewAgentIcon = (p: P) => (
  <I {...p}>
    <path d="M2.5 3h11L9.5 8.2V13l-3-1.6V8.2z" />
  </I>
);
export const MachinesIcon = (p: P) => (
  <I {...p}>
    <rect x="2" y="2.4" width="12" height="8.2" rx="1.5" />
    <path d="M8 10.6v2.2M5.2 13.6h5.6" />
  </I>
);
export const ProvidersIcon = (p: P) => (
  <I {...p}>
    <path d="M6 2.4v3M10 2.4v3" />
    <path d="M4.3 5.4h7.4v3.4c0 2-1.7 3.7-3.7 3.7s-3.7-1.7-3.7-3.7z" />
    <path d="M8 12.5v2.1" />
  </I>
);
export const GearIcon = (p: P) => (
  <I {...p}>
    <circle cx="8" cy="8" r="2" />
    <path d="M8 1.8 9.2 3.6l2.1-.4.7 2 1.9 1-.9 1.8.9 1.8-1.9 1-.7 2-2.1-.4L8 14.2l-1.2-1.8-2.1.4-.7-2-1.9-1L3 8l-.9-1.8 1.9-1 .7-2 2.1.4z" />
  </I>
);
export const SlidersIcon = (p: P) => (
  <I {...p}>
    <path d="M2.5 5h11M2.5 11h11" />
    <circle cx="6.2" cy="5" r="1.5" fill="currentColor" stroke="none" />
    <circle cx="10.2" cy="11" r="1.5" fill="currentColor" stroke="none" />
  </I>
);
export const CubeIcon = (p: P) => (
  <I {...p}>
    <path d="M8 2.4 13.4 5.4v5.2L8 13.6 2.6 10.6V5.4z" />
    <path d="M2.6 5.4 8 8.4l5.4-3M8 8.4v5.2" />
  </I>
);
export const AppearanceIcon = (p: P) => (
  <I {...p}>
    <circle cx="8" cy="8" r="5.4" />
    <path d="M8 2.6v10.8" />
    <path d="M8 2.6a5.4 5.4 0 0 0 0 10.8z" fill="currentColor" stroke="none" />
  </I>
);
export const ExternalIcon = (p: P) => (
  <I {...p}>
    <path d="M9.2 3.2h3.6v3.6M12.6 3.4 7.2 8.8" />
    <path d="M10.2 6.4v5.4c0 .7-.6 1.2-1.2 1.2H4.2c-.7 0-1.2-.5-1.2-1.2V7c0-.7.5-1.2 1.2-1.2h5" />
  </I>
);
export const CloseIcon = (p: P) => (
  <I {...p} sw={1.4}>
    <path d="M4 4l8 8M12 4l-8 8" />
  </I>
);
export const BellIcon = (p: P) => (
  <I {...p}>
    <path d="M8 2.6c-1.8 0-3.2 1.5-3.2 3.2v2.1L3.2 10.4h9.6L11.2 7.9V5.8C11.2 4.1 9.8 2.6 8 2.6z" />
    <path d="M6.4 12.4a1.6 1.6 0 0 0 3.2 0" />
  </I>
);
export const ArchiveIcon = (p: P) => (
  <I {...p}>
    <path d="M2.5 4.2h11v2.2H2.5z" />
    <path d="M3.4 6.4v6.4h9.2V6.4" />
    <path d="M6.4 9.2h3.2" />
  </I>
);
export const TrashIcon = (p: P) => (
  <I {...p}>
    <path d="M3.2 4.4h9.6M6 4.4V3.2h4v1.2M4.4 4.4l.6 9h6l.6-9" />
  </I>
);
export const PencilIcon = (p: P) => (
  <I {...p}>
    <path d="M9.2 3.4 12.6 6.8 6 13.4H2.6v-3.4z" />
    <path d="M8 4.6 11.4 8" />
  </I>
);
export const KeyIcon = (p: P) => (
  <I {...p}>
    <circle cx="6" cy="8" r="2.4" />
    <path d="M8.2 8h5.2l-1.3 1.4M11.2 8v1.6" />
  </I>
);
export const FileIcon = (p: P) => (
  <I {...p}>
    <path d="M4.2 2.4h5.2L12 5.2v8.4c0 .7-.6 1.2-1.3 1.2H4.2c-.7 0-1.2-.5-1.2-1.2V3.6c0-.7.5-1.2 1.2-1.2z" />
    <path d="M9.2 2.5v2.8H12" />
  </I>
);
export const FolderIcon = (p: P) => (
  <I {...p}>
    <path d="M2 4.8C2 3.8 2.8 3 3.8 3h2.3l1.5 1.6h4.6c1 0 1.8.8 1.8 1.8v5c0 1-.8 1.8-1.8 1.8H3.8c-1 0-1.8-.8-1.8-1.8z" />
  </I>
);
export const FolderOpenIcon = (p: P) => (
  <I {...p}>
    <path d="M2.2 12.2V4.8C2.2 3.8 3 3 4 3h2.1l1.5 1.6H11c1 0 1.8.8 1.8 1.8v.4" />
    <path d="M2.3 12.4 4 7.9c.2-.7.8-1.1 1.500-1.100h7.700c.9 0 1.500.9 1.200 1.700l-1.200 3.600c-.2.7-.9 1.100-1.600 1.100H3.500c-.7 0-1.300-.4-1.200-.800z" />
  </I>
);
/** Collapsed finished-missions group: the same family as the sidebar status dot. */
export const FinishedIcon = (p: P) => (
  <I {...p}>
    <circle cx="8" cy="8" r="5.2" />
  </I>
);
/** Expanded finished-missions group: the ring opens onto the inner dot. */
export const FinishedOpenIcon = (p: P) => (
  <I {...p}>
    <circle cx="8" cy="8" r="5.2" />
    <circle cx="8" cy="8" r="2" fill="currentColor" stroke="none" />
  </I>
);
export const LaptopIcon = (p: P) => (
  <I {...p}>
    <path d="M3.5 4.2c0-.7.5-1.200 1.200-1.200h6.600c.7 0 1.200.5 1.200 1.200V10h-9zM2 12.500 3.500 10h9l1.500 2.500z" />
  </I>
);
export const CloudIcon = (p: P) => (
  <I {...p}>
    <path d="M4.600 12.500a2.800 2.800 0 0 1-.4-5.600 4 4 0 0 1 7.700 1 2.300 2.300 0 0 1-.4 4.600z" />
  </I>
);
export const CmdIcon = (p: P) => (
  <I {...p} sw={1.2}>
    <path d="M5.2 5.2h2.4v2.4H5.2zM8.4 5.2h2.4v2.4H8.4zM5.2 8.4h2.4v2.4H5.2zM8.4 8.4h2.4v2.4H8.4z" />
  </I>
);
export const DotsIcon = (p: P) => (
  <svg class={p.class} width={p.size ?? 16} height={p.size ?? 16} viewBox="0 0 16 16" fill="currentColor">
    <circle cx="4" cy="8" r="1" />
    <circle cx="8" cy="8" r="1" />
    <circle cx="12" cy="8" r="1" />
  </svg>
);
export const ChevronDown = (p: P) => (
  <I {...p}>
    <path d="m4 6 4 4 4-4" />
  </I>
);
export const ChevronRight = (p: P) => (
  <I {...p}>
    <path d="m6 4 4 4-4 4" />
  </I>
);
export const PlusIcon = (p: P) => (
  <I {...p}>
    <path d="M8 3v10M3 8h10" />
  </I>
);
export const ReplyIcon = (p: P) => (
  <I {...p}>
    <path d="M6.500 4 3 7.500 6.500 11" />
    <path d="M3 7.500h6c2.200 0 4 1.500 4 3.800V12" />
  </I>
);
export const MicIcon = (p: P) => (
  <svg class={p.class} width={p.size ?? 16} height={p.size ?? 16} viewBox="0 0 16 16" fill="none">
    <rect x="5.700" y="1.800" width="4.600" height="8" rx="2.300" fill="currentColor" />
    <path d="M3.800 7.800a4.200 4.200 0 0 0 8.400 0M8 12v2.200" stroke="currentColor" stroke-width="1.300" stroke-linecap="round" />
  </svg>
);
export const ArrowUpIcon = (p: P) => (
  <I {...p} sw={1.6}>
    <path d="M8 13V3M3.500 7.500 8 3l4.500 4.500" />
  </I>
);
export const CheckIcon = (p: P) => (
  <I {...p} sw={1.7}>
    <path d="M3.2 8.4 6.5 11.6 12.8 4.6" />
  </I>
);
export const TargetIcon = (p: P) => (
  <I {...p} sw={1.3}>
    <circle cx="8" cy="8" r="5.5" />
    <circle cx="8" cy="8" r="2.2" />
    <path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2" />
  </I>
);
export const StopIcon = (p: P) => (
  <svg class={p.class} width={p.size ?? 16} height={p.size ?? 16} viewBox="0 0 16 16" fill="currentColor">
    <rect x="4.500" y="4.500" width="7" height="7" rx="1.500" />
  </svg>
);
export const PrClosedIcon = (p: P) => (
  <I {...p}>
    <circle cx="4.500" cy="12.200" r="1.500" />
    <circle cx="11.500" cy="12.200" r="1.500" />
    <path d="M4.500 10.700V5M11.500 10.700V8M3 2.500l3 3M6 2.500l-3 3M10 3l3 3M13 3l-3 3" />
  </I>
);
export const PrMergedIcon = (p: P) => (
  <I {...p}>
    <circle cx="4.500" cy="3.800" r="1.500" />
    <circle cx="4.500" cy="12.200" r="1.500" />
    <circle cx="11.500" cy="9" r="1.500" />
    <path d="M4.500 5.300v5.400M4.500 5.300c0 2.500 2.500 3.700 5.500 3.700" />
  </I>
);
export const CopyIcon = (p: P) => (
  <I {...p}>
    <rect x="5.500" y="5.500" width="8" height="8" rx="1.500" />
    <path d="M10.500 5.500V4c0-.8-.7-1.500-1.500-1.500H4c-.8 0-1.500.7-1.500 1.500v5c0 .8.7 1.500 1.500 1.500h1.500" />
  </I>
);
export const Spinner = (p: P) => (
  <svg class={`spin ${p.class ?? ""}`} width={p.size ?? 16} height={p.size ?? 16} viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.500" stroke-linecap="round">
    <circle cx="8" cy="8" r="5.500" opacity="0.25" />
    <path d="M13.500 8A5.500 5.500 0 0 0 8 2.500" />
  </svg>
);
export const ContextRing = (p: { pct: number }) => {
  const c = 2 * Math.PI * 5;
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke-width="2">
      <circle cx="7" cy="7" r="5" stroke="currentColor" opacity="0.25" />
      <circle cx="7" cy="7" r="5" stroke="currentColor" stroke-dasharray={`${(c * p.pct) / 100} ${c}`} transform="rotate(-90 7 7)" />
    </svg>
  );
};

/** Cursor-style "running" glyph: a 3x3 dot grid with a diagonal opacity wave. */
export const RunningDots = () => (
  <svg class="rdots" width="14" height="14" viewBox="0 0 14 14" fill="currentColor">
    {[0, 1, 2].flatMap((y) => [0, 1, 2].map((x) => <circle cx={3 + x * 4} cy={3 + y * 4} r="1" style={{ "animation-delay": `${(x + (2 - y)) * 0.12}s` }} />))}
  </svg>
);

export const PauseIcon = (p: P) => (
  <I {...p} sw={1.6}><path d="M5.5 4v8M10.5 4v8" /></I>
);

/** Core: stacked server rack. Workers: a single compute unit. */
export const CoreServerIcon = (p: P) => (
  <I {...p}><rect x="2.5" y="2" width="11" height="5" rx="1.3" /><rect x="2.5" y="9" width="11" height="5" rx="1.3" /><path d="M5 4.5h.01M5 11.5h.01M8 4.5h3M8 11.5h3" /></I>
);
export const ComputeNodeIcon = (p: P) => (
  <I {...p}><rect x="2.5" y="3.5" width="11" height="9" rx="1.5" /><path d="M5 6.5h.01M8 6.5h3M5 9.5h6" /></I>
);

export const BranchIcon = (p: P) => <I {...p}><circle cx="4" cy="3" r="1.5" /><circle cx="12" cy="3" r="1.5" /><circle cx="4" cy="13" r="1.5" /><path d="M4 4.5v7M12 4.5v1c0 3-8 1-8 4" /></I>;

export const PlanIcon = (p: P) => (<I {...p}><circle cx="3" cy="4" r="1"/><circle cx="3" cy="8" r="1"/><circle cx="3" cy="12" r="1"/><path d="M7 4h6M7 8h6M7 12h4"/></I>);
