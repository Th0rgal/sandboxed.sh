import { For, Show, createMemo, createSignal, createEffect, onMount, onCleanup } from "solid-js";
import * as Ic from "./icons";
import { trapFocus } from "./focusScope";
import { PromptSheet } from "./Dialog";
import { slugify } from "./api";

export function ProjectPicker(p: {
  projects: { id: string; name: string }[];
  selected: string;
  canCreate: boolean;
  onSelect: (id: string) => void;
  onCreate: () => void;
  onMachine: () => void;
  onClose: () => void;
}) {
  const [query,setQuery]=createSignal("");
  const [active,setActive]=createSignal(0);
  const [armed,setArmed]=createSignal(false);
  const rows=createMemo(()=>p.projects.filter(x=>`${x.name} ${x.id}`.toLowerCase().includes(query().trim().toLowerCase())));
  let root!:HTMLDivElement;
  createEffect(()=>{rows();setActive(0);setArmed(false);});
  onMount(()=>{
    const release=trapFocus(root,p.onClose);
    const outside=(e:PointerEvent)=>{if(!root.contains(e.target as Node))p.onClose();};
    document.addEventListener("pointerdown",outside);
    onCleanup(()=>{document.removeEventListener("pointerdown",outside);release();});
  });
  const move=(offset:number)=>{
    const count=rows().length;if(!count)return;
    setArmed(true);
    setActive((active()+offset+count)%count);
    root.querySelector(`#project-option-${active()}`)?.scrollIntoView({block:"nearest"});
  };
  return <div class="project-picker" ref={root} role="dialog" aria-label="Choose project" onPointerDown={e=>e.stopPropagation()}>
    <input class="project-search" autofocus role="combobox" aria-label="Search projects" aria-expanded="true" aria-controls="project-options" aria-autocomplete="list" aria-activedescendant={rows().length?`project-option-${active()}`:undefined}
      placeholder="Search projects…" value={query()} onInput={e=>setQuery(e.currentTarget.value)}
      onKeyDown={e=>{
        if(e.key==="ArrowDown"||e.key==="ArrowUp"){e.preventDefault();move(e.key==="ArrowDown"?1:-1);}
        else if(e.key==="Enter"){e.preventDefault();const row=rows()[active()];if(row)p.onSelect(row.id);}
      }}/>
    <div class="project-picker-label">Recents</div>
    <div class="project-options" id="project-options" role="listbox" aria-label="Projects">
      <For each={rows()}>{(row,index)=><button id={`project-option-${index()}`} role="option" aria-selected={row.id===p.selected} class={`project-option ${armed()&&index()===active()?"highlighted":""}`} onPointerEnter={()=>{setArmed(true);setActive(index());}} onClick={()=>p.onSelect(row.id)}>
        <Ic.FolderIcon size={15}/><span>{row.name}</span><Show when={row.id===p.selected}><span class="project-check" aria-label="Current project">✓</span></Show>
      </button>}</For>
      <Show when={!rows().length}><p class="project-empty">{p.projects.length?"No matching projects":"No projects yet"}</p></Show>
    </div>
    <div class="project-picker-actions">
      <Show when={p.canCreate}><button onClick={p.onCreate}><Ic.PlusIcon size={15}/>New project…</button></Show>
      <button onClick={p.onMachine}><Ic.MachinesIcon size={15}/>Choose machine…</button>
    </div>
  </div>;
}

export function ProjectCreation(p:{anchor?:HTMLElement;existingIds: string[];onCreate:(title:string,slug:string)=>Promise<void>;onClose:()=>void}) {
  const [name,setName]=createSignal("");
  const slug=()=>slugify(name());
  const [busy,setBusy]=createSignal(false);
  const [error,setError]=createSignal<string|null>(null);
  const close=()=>{if(!busy())p.onClose();};
  const submit=async()=>{
    if(busy())return;
    if(!name().trim()){setError("Enter a project name.");return;}
    if(!/^[a-z0-9][a-z0-9_-]*$/.test(slug())){setError("Choose a name containing letters or numbers.");return;}
    if(p.existingIds.includes(slug())){setError("A project with this name already exists. Choose it from Recents or use another name.");return;}
    setBusy(true);setError(null);
    try{await p.onCreate(name().trim(),slug());}
    catch(e){setError(e instanceof Error?e.message:String(e));}
    finally{setBusy(false);}
  };
  return <PromptSheet anchor={p.anchor} class="project-creation" title="New project" label="Project name" placeholder="Name your project…" value={name()} onInput={v=>{setName(v);setError(null);}} action={busy()?"Creating…":"Create project"} busy={busy()} disabled={!name().trim()} error={error()} onAction={()=>void submit()} onClose={close} />;
}
