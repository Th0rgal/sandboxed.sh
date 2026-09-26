import { createSignal, Show } from "solid-js";
import { render } from "solid-js/web";
import { ConfirmDialog, Dialog, DialogButton, PromptSheet } from "../src/Dialog";
import { CronForm } from "../src/ControllerSettings";
import { getProjectCronFromJob } from "../src/cronSchema";
import fixtures from "./fixtures/hermes-jobs.json";
import "../src/styles.css";

document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") ?? "dark";
function Harness() {
  const [kind, setKind] = createSignal("");
  const [name, setName] = createSignal("Research notes");
  return <main style={{ background: "var(--bg)", height: "100vh", padding: "40px", color: "var(--fg)" }}>
    <h1>Projects</h1><p>Research notes · Verification · Weekly review</p>
    <div style={{ display: "flex", gap: "8px" }}>
      <DialogButton onClick={() => setKind("rename")}>Rename project</DialogButton>
      <DialogButton onClick={() => setKind("confirm")}>Delete chain</DialogButton>
      <DialogButton onClick={() => setKind("cron")}>New cron</DialogButton>
    </div>
    <Show when={kind() === "rename"}><PromptSheet title="Rename" hint="research-notes" label="Project name" value={name()}
      onInput={setName} action="Save" onAction={() => setKind("")} onClose={() => setKind("")} /></Show>
    <Show when={kind() === "confirm"}><ConfirmDialog title="Delete chain?" description="Delete “Default routing”? This cannot be undone."
      action="Delete chain" destructive onConfirm={() => setKind("")} onClose={() => setKind("")} /></Show>
    <Show when={kind() === "cron"}><Dialog title="New cron" size="wide" onClose={() => setKind("")} footer={<span>Unfinished drafts are kept until saved or discarded.</span>}>
      <CronForm creating draftKey="dialog-browser" view={getProjectCronFromJob("notes", fixtures.hourly)}
        save={async () => getProjectCronFromJob("notes", fixtures.hourly)} onSaved={() => setKind("")} onClose={() => setKind("")} />
    </Dialog></Show>
  </main>;
}
render(() => <Harness />, document.getElementById("root")!);
