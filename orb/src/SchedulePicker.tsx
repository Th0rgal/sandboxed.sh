import { For, Match, Show, Switch, createMemo, createSignal } from "solid-js";

/** What Hermes' schedule grammar can express, as structured state. */
type Parsed =
  | { mode: "interval"; n: number; unit: "m" | "h" | "d" }
  | { mode: "days"; hour: number; minute: number; days: number[] } // 0 = Sunday … 6 = Saturday
  | { mode: "once"; local: string } // yyyy-MM-ddTHH:mm
  | { mode: "custom"; raw: string };

const DAY_LABELS = ["S", "M", "T", "W", "T", "F", "S"];
const DAY_NAMES = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEK_ORDER = [1, 2, 3, 4, 5, 6, 0]; // Monday first
const UNIT_WORD = { m: "minute", h: "hour", d: "day" } as const;

function parseDow(spec: string): number[] | null {
  if (spec === "*") return [0, 1, 2, 3, 4, 5, 6];
  const out = new Set<number>();
  for (const part of spec.split(",")) {
    const range = part.match(/^(\d)-(\d)$/);
    if (range) {
      const a = Number(range[1]);
      const b = Number(range[2]);
      if (a > b) return null;
      for (let d = a; d <= b; d++) out.add(d % 7);
    } else if (/^\d$/.test(part)) {
      out.add(Number(part) % 7);
    } else {
      return null;
    }
  }
  return out.size ? [...out].sort((a, b) => a - b) : null;
}

export function parseSchedule(value: string): Parsed {
  const raw = value.trim();
  const every = raw.match(/^(?:every\s+)?(\d+)\s*(m|min|mins|minutes?|h|hr|hrs|hours?|d|days?)$/i);
  if (every) {
    const unit = every[2][0].toLowerCase() as "m" | "h" | "d";
    return { mode: "interval", n: Math.max(1, Number(every[1])), unit };
  }
  const cron = raw.match(/^(\d{1,2})\s+(\d{1,2})\s+\*\s+\*\s+(\S+)$/);
  if (cron) {
    const days = parseDow(cron[3]);
    const minute = Number(cron[1]);
    const hour = Number(cron[2]);
    if (days && minute < 60 && hour < 24) return { mode: "days", hour, minute, days };
  }
  if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/.test(raw)) return { mode: "once", local: raw.slice(0, 16) };
  return { mode: "custom", raw };
}

function dowSpec(days: number[]): string {
  const sorted = [...new Set(days)].sort((a, b) => a - b);
  if (sorted.length === 7) return "*";
  if (sorted.join(",") === "1,2,3,4,5") return "1-5";
  return sorted.join(",");
}

export function formatSchedule(p: Parsed): string {
  switch (p.mode) {
    case "interval":
      return `every ${p.n}${p.unit}`;
    case "days":
      return `${p.minute} ${p.hour} * * ${dowSpec(p.days.length ? p.days : [1])}`;
    case "once":
      return p.local;
    case "custom":
      return p.raw;
  }
}

const pad = (n: number) => String(n).padStart(2, "0");

export function describeSchedule(p: Parsed): string {
  switch (p.mode) {
    case "interval":
      return p.n === 1 ? `Every ${UNIT_WORD[p.unit]}` : `Every ${p.n} ${UNIT_WORD[p.unit]}s`;
    case "days": {
      const time = `${pad(p.hour)}:${pad(p.minute)}`;
      const key = dowSpec(p.days);
      if (key === "*") return `Every day at ${time}`;
      if (key === "1-5") return `Weekdays at ${time}`;
      if (key === "0,6") return `Weekends at ${time}`;
      return `${WEEK_ORDER.filter((d) => p.days.includes(d)).map((d) => DAY_NAMES[d]).join(", ")} at ${time}`;
    }
    case "once": {
      const d = new Date(p.local);
      return Number.isNaN(d.getTime())
        ? "Once"
        : `Once, ${d.toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" })} at ${pad(d.getHours())}:${pad(d.getMinutes())}`;
    }
    case "custom":
      return p.raw ? "Custom expression" : "No schedule";
  }
}

const MODES: { id: Parsed["mode"]; label: string }[] = [
  { id: "interval", label: "Interval" },
  { id: "days", label: "Days & time" },
  { id: "once", label: "Once" },
  { id: "custom", label: "Custom" },
];

/** Structured editor for a Hermes cron schedule; emits the schedule string. */
export function SchedulePicker(p: { value: string; onChange: (v: string) => void }) {
  const parsed = createMemo(() => parseSchedule(p.value));
  // The mode the user picked wins over what the string happens to parse as,
  // so switching to "Custom" with an interval string keeps the raw editor.
  const [forced, setForced] = createSignal<Parsed["mode"] | null>(null);
  const mode = () => forced() ?? parsed().mode;
  const emit = (next: Parsed) => p.onChange(formatSchedule(next));

  const switchTo = (m: Parsed["mode"]) => {
    setForced(m);
    const cur = parsed();
    if (m === cur.mode) return;
    if (m === "interval") emit({ mode: "interval", n: 45, unit: "m" });
    else if (m === "days") emit({ mode: "days", hour: 9, minute: 0, days: [1, 2, 3, 4, 5] });
    else if (m === "once") {
      const d = new Date(Date.now() + 3600_000);
      emit({ mode: "once", local: `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:00` });
    }
    // custom keeps the current string as-is
  };

  const interval = () => (parsed().mode === "interval" ? (parsed() as Extract<Parsed, { mode: "interval" }>) : { mode: "interval" as const, n: 45, unit: "m" as const });
  const days = () => (parsed().mode === "days" ? (parsed() as Extract<Parsed, { mode: "days" }>) : { mode: "days" as const, hour: 9, minute: 0, days: [1, 2, 3, 4, 5] });
  const once = () => (parsed().mode === "once" ? (parsed() as Extract<Parsed, { mode: "once" }>).local : "");

  const toggleDay = (d: number) => {
    const cur = days();
    const next = cur.days.includes(d) ? cur.days.filter((x) => x !== d) : [...cur.days, d];
    if (next.length) emit({ ...cur, days: next });
  };

  return (
    <div class="sp">
      <div class="cs-seg sp-modes">
        <For each={MODES}>
          {(m) => (
            <button class={mode() === m.id ? "on" : ""} onClick={() => switchTo(m.id)}>
              {m.label}
            </button>
          )}
        </For>
      </div>

      <div class="sp-body">
        <Switch>
          <Match when={mode() === "interval"}>
            <span class="sp-word">Every</span>
            <input
              class="s-input sp-num"
              inputmode="numeric"
              value={interval().n}
              onInput={(e) => {
                const n = Number(e.currentTarget.value.replace(/[^0-9]/g, ""));
                if (n > 0) emit({ ...interval(), n });
              }}
            />
            <div class="cs-seg">
              <For each={["m", "h", "d"] as const}>
                {(u) => (
                  <button class={interval().unit === u ? "on" : ""} onClick={() => emit({ ...interval(), unit: u })}>
                    {UNIT_WORD[u]}s
                  </button>
                )}
              </For>
            </div>
          </Match>

          <Match when={mode() === "days"}>
            <div class="sp-days">
              <For each={WEEK_ORDER}>
                {(d) => (
                  <button class={`sp-day ${days().days.includes(d) ? "on" : ""}`} title={DAY_NAMES[d]} onClick={() => toggleDay(d)}>
                    {DAY_LABELS[d]}
                  </button>
                )}
              </For>
            </div>
            <span class="sp-word">at</span>
            <input
              class="s-input sp-time"
              type="time"
              value={`${pad(days().hour)}:${pad(days().minute)}`}
              onInput={(e) => {
                const [h, m] = e.currentTarget.value.split(":").map(Number);
                if (Number.isFinite(h) && Number.isFinite(m)) emit({ ...days(), hour: h, minute: m });
              }}
            />
            <div class="sp-presets">
              <button onClick={() => emit({ ...days(), days: [1, 2, 3, 4, 5] })}>Weekdays</button>
              <button onClick={() => emit({ ...days(), days: [0, 1, 2, 3, 4, 5, 6] })}>Every day</button>
            </div>
          </Match>

          <Match when={mode() === "once"}>
            <span class="sp-word">On</span>
            <input
              class="s-input sp-datetime"
              type="datetime-local"
              value={once()}
              onInput={(e) => e.currentTarget.value && emit({ mode: "once", local: e.currentTarget.value })}
            />
          </Match>

          <Match when={mode() === "custom"}>
            <input
              class="s-input sp-raw"
              spellcheck={false}
              placeholder="0 9 * * 1-5"
              value={p.value}
              onInput={(e) => p.onChange(e.currentTarget.value)}
            />
          </Match>
        </Switch>
      </div>

      <div class="sp-foot">
        <span class="sp-summary">{describeSchedule(parsed())}</span>
        <Show when={parsed().mode !== "interval"}>
          <span class="sp-tz">server time</span>
        </Show>
        <code class="sp-expr">{p.value.trim() || "—"}</code>
      </div>
    </div>
  );
}
