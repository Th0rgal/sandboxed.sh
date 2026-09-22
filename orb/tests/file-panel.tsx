import { render } from "solid-js/web";
import { FilePanelProvider, FilePanelButton } from "../src/FilePanel";
import { MdView } from "../src/Markdown";
import "../src/styles.css";
localStorage.setItem("orb.apiUrl", location.origin);
localStorage.setItem("orb.jwt", "test");
document.documentElement.dataset.theme =
  new URLSearchParams(location.search).get("theme") ?? "dark";
render(
  () => (
    <div class="app" style={{ "--sb-w": "220px" }}>
      <FilePanelProvider scope={{ project: "verity-pareto" }}>
        <aside class="sidebar">
          <div class="sb-top" />
          <nav class="sb-scroll">
            <button class="row">New Agent</button>
            <button class="row">Machines</button>
            <button class="row">Providers</button>
            <p class="file-muted">PROJECTS</p>
            <button class="row">Verity</button>
            <button class="row active">Pareto audit</button>
          </nav>
        </aside>
        <header class="titlebar">
          <span>Status de l’audit Pareto</span>
          <FilePanelButton />
        </header>
        <main class="main">
          <div class="scroll">
            <div class="col">
              <div class="user">
                Peux-tu me faire un résumé du status de l’audit Pareto ?
              </div>
              <MdView
                text={
                  "## Audit Pareto\n\nLes garanties sont décrites dans `audit/guarantees.yaml`. Le brief propriétaire est disponible dans [IMPLEMENTATION-BRIEF.md](audit/IMPLEMENTATION-BRIEF.md).\n\n| Garantie | Source |\n| --- | --- |\n| PRICE-1 | `audit/guarantees.yaml:3` |\n\nLe controller fournit le contexte courant dans `controller.md`."
                }
              />
            </div>
          </div>
          <div class="dock">
            <textarea aria-label="Draft" placeholder="Send follow-up" />
          </div>
        </main>
      </FilePanelProvider>
    </div>
  ),
  document.getElementById("root")!,
);
