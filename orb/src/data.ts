export type AgentStatus = "idle" | "running" | "pr-closed" | "pr-merged";

export type Block =
  | { kind: "p"; text: string }
  | { kind: "ul"; items: string[] }
  | { kind: "code"; lang: string; file?: string; text: string }
  | { kind: "tool"; verb: string; target: string; meta?: string }
  | { kind: "note"; strong: string; rest: string };

export type Turn =
  | { role: "user"; text: string }
  | { role: "agent"; worked: string; blocks: Block[] };

export interface Agent {
  id: string;
  title: string;
  status: AgentStatus;
  cloud?: boolean;
  diff?: number;
  context: number;
  archived?: boolean;
  turns: Turn[];
}

export interface DocFile {
  id: string;
  name: string;
  text: string;
}

export interface Folder {
  id: string;
  name: string;
  files: DocFile[];
}

export interface Project {
  id: string;
  name: string;
  agents: Agent[];
  folders: Folder[];
}

const loremA: Turn[] = [
  {
    role: "user",
    text: "lorem ipsum dolor sit amet, consectetur adipiscing elit — sed do eiusmod tempor incididunt ut labore et dolore magna aliqua",
  },
  {
    role: "agent",
    worked: "35s",
    blocks: [
      { kind: "p", text: "Lorem ipsum is installed. Dolor is disconnected, and consectetur sign-in is waiting." },
      { kind: "p", text: "**Enter this code** at the page I opened: [lorem.ipsum/link](https://example.com)" },
      { kind: "p", text: "`3EK9AQXD`" },
      {
        kind: "p",
        text: "That finishes adipiscing. The desktop app is compiling in the background; I’ll install it as soon as the build finishes.",
      },
    ],
  },
  {
    role: "user",
    text: "ut enim ad minim veniam, quis nostrud exercitation ullamco laboris. Nisi ut aliquip ex ea commodo consequat, duis aute irure dolor in reprehenderit (in voluptate velit esse cillum dolore)",
  },
  {
    role: "agent",
    worked: "1m 57s",
    blocks: [
      { kind: "tool", verb: "Read", target: "src/lorem/ipsum.rs", meta: "L1-184" },
      { kind: "tool", verb: "Grepped", target: "use_keychain", meta: "12 matches" },
      { kind: "tool", verb: "Edited", target: "src/config/dolor.rs", meta: "+31 -4" },
      {
        kind: "p",
        text: "Open issues were #49 (`.lorem`), #48 (ipsum title bar), #35 (dolor), and #8 (amet). **#49** was the one worth doing now: the CLI could already import consectetur packs, the desktop app could not.",
      },
      {
        kind: "p",
        text: "**#49 — `.lorem` import** Profile menu now has **Modpack**. Pick a `.lorem` and Orb creates the profile. CLI path is still `orb modpack import <file.lorem>`. Sed/Quis packs work; Tempor/Incididunt still fail with a clear error.",
      },
      {
        kind: "p",
        text: "**#48 — Linux window chrome** Lorem no longer strips the native title bar, so close/minimize/maximize stay visible.",
      },
      { kind: "p", text: "**Keychain is optional, and off on this machine**" },
      {
        kind: "ul",
        items: ["Settings → General → **Use system keychain**", "`orb config set-use-keychain false`", "or `ORB_USE_KEYCHAIN=0`"],
      },
      {
        kind: "code",
        lang: "rust",
        file: "src/config/dolor.rs",
        text: `pub fn use_keychain(cfg: &Config) -> bool {
    match std::env::var("ORB_USE_KEYCHAIN").as_deref() {
        Ok("0") | Ok("false") => false,
        Ok(_) => true,
        Err(_) => cfg.general.use_keychain,
    }
}`,
      },
      {
        kind: "p",
        text: "Your install is already off. Lorem’s tokens are in [`~/.orb/tokens.json`](#) (mode `600`), and `orb account check` is clean: MSA ~32 minutes, Ipsum ~23 hours. Homebrew’s older `0.1.25` CLI is unlinked so `orb` is the cargo `0.1.26` build. The desktop app is in `/Applications/Orb Launcher.app`.",
      },
      {
        kind: "p",
        text: "Reconnect from earlier completed: Lorem is signed in again. Turn keychain back on anytime with `orb config set-use-keychain true`.",
      },
      { kind: "note", strong: "Finished", rest: "3 background tasks" },
    ],
  },
];

const short = (q: string, a: string): Turn[] => [
  { role: "user", text: q },
  {
    role: "agent",
    worked: "48s",
    blocks: [
      { kind: "tool", verb: "Explored", target: "4 files", meta: "2 searches" },
      { kind: "p", text: a },
      {
        kind: "ul",
        items: [
          "Lorem ipsum dolor sit amet, **consectetur** adipiscing elit",
          "Sed do eiusmod tempor `incididunt` ut labore",
          "Ut enim ad minim veniam, quis nostrud exercitation",
        ],
      },
      { kind: "p", text: "Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur." },
    ],
  },
];

// A long transcript to make scroll performance measurable.
const long: Turn[] = Array.from({ length: 40 }, (_, i) => [
  { role: "user", text: `Lorem ipsum request #${i + 1}: sed ut perspiciatis unde omnis iste natus error sit voluptatem` } as Turn,
  loremA[3],
]).flat();

export const projects: Project[] = [
  {
    id: "orb",
    name: "orb",
    agents: [{ id: "a1", title: "Issues and optional keychain", status: "idle", diff: 49, context: 63, turns: loremA }],
    folders: [
      {
        id: "notes",
        name: "notes",
        files: [
          {
            id: "keychain",
            name: "keychain.md",
            text: "# Keychain\n\nOptional, and off on this machine.\n\n- Settings → General → **Use system keychain**\n- `orb config set-use-keychain false`\n",
          },
        ],
      },
    ],
  },
  {
    id: "lorem",
    name: "lorem",
    agents: [
      {
        id: "a2",
        title: "Review Lorem v1.219.1",
        status: "running",
        context: 21,
        turns: short("review the lorem release and list what changed", "Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."),
      },
    ],
    folders: [],
  },
  {
    id: "ipsum_marketing",
    name: "ipsum_marketing",
    agents: [
      {
        id: "a3",
        title: "DOLOR-8282 review and update",
        status: "idle",
        diff: 12,
        context: 38,
        turns: short("review DOLOR-8282 and update the landing copy", "Sed ut perspiciatis unde omnis iste natus error sit voluptatem accusantium doloremque laudantium."),
      },
    ],
    folders: [],
  },
  { id: "dolor-proof-closure", name: "dolor-srv3-proof-closure", agents: [], folders: [] },
  {
    id: "amet-proof-closure",
    name: "amet-8282-proof-closure",
    agents: [
      { id: "a4", title: "Extraction slots retracted proof", status: "pr-closed", cloud: true, diff: 210, context: 77, turns: long },
      {
        id: "a5",
        title: "Abstract transaction semantics",
        status: "pr-merged",
        cloud: true,
        diff: 88,
        context: 45,
        turns: short("abstract the transaction semantics", "Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit."),
      },
    ],
    folders: [],
  },
  { id: "portfolio", name: "portfolio", agents: [], folders: [] },
  { id: "consectetur", name: "consectetur", agents: [], folders: [] },
  { id: "adipiscing.md", name: "adipiscing.md", agents: [], folders: [] },
  { id: "home-manager", name: "home-manager", agents: [], folders: [] },
  { id: "elit", name: "elit", agents: [], folders: [] },
];

export const LOREM_REPLY =
  "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud `exercitation` ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in **reprehenderit** in voluptate velit esse cillum dolore eu fugiat nulla pariatur.";
