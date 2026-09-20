import { For, Show, createMemo, createSignal, createEffect, onMount, onCleanup } from "solid-js";
import * as Ic from "./icons";
import { trapFocus } from "./focusScope";
import { Dialog, Field } from "./Dialog";
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
  const rows=createMemo(()=>p.projects.filter(x=>`${x.name} ${x.id}`.toLowerCase().includes(query().trim().toLowerCase())));
  let root!:HTMLDivElement;
  createEffect(()=>{rows();setActive(0);});
  onMount(()=>{
    const release=trapFocus(root,p.onClose);
    const outside=(e:PointerEvent)=>{if(!root.contains(e.target as Node))p.onClose();};
    document.addEventListener("pointerdown",outside);
    onCleanup(()=>{document.removeEventListener("pointerdown",outside);release();});
  });
  const move=(offset:number)=>{
    const count=rows().length;if(!count)return;
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
      <For each={rows()}>{(row,index)=><button id={`project-option-${index()}`} role="option" aria-selected={row.id===p.selected} class={`project-option ${index()===active()?"highlighted":""}`} onMouseMove={()=>setActive(index())} onClick={()=>p.onSelect(row.id)}>
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

export function ProjectCreation(p:{existingIds: string[];onCreate:(title:string,slug:string)=>Promise<void>;onClose:()=>void}) {
  const [name,setName]=createSignal("");
  const [id,setId]=createSignal<string|null>(null);
  const slug=()=>id()??slugify(name());
  const [busy,setBusy]=createSignal(false);
  const [error,setError]=createSignal<string|null>(null);
  const close=()=>{if(!busy())p.onClose();};
  const submit=async()=>{
    if(busy())return;
    if(!name().trim()){setError("Enter a project name.");return;}
    if(!/^[a-z0-9][a-z0-9_-]*$/.test(slug())){setError("Use letters, numbers, hyphens or underscores for the project ID.");return;}
    if(p.existingIds.includes(slug())){setError("A project with this ID already exists. Choose it from Recents or use a different ID.");return;}
    setBusy(true);setError(null);
    try{await p.onCreate(name().trim(),slug());}
    catch(e){setError(e instanceof Error?e.message:String(e));}
    finally{setBusy(false);}
  };
  return <Dialog title="New project" onClose={close} footer={<><button class="s-btn" disabled={busy()} onClick={close}>Cancel</button><button class="s-btn primary" disabled={busy()||!name().trim()} onClick={()=>void submit()}>{busy()?"Creating…":"Create project"}</button></>}>
    <form class="project-create" onSubmit={e=>{e.preventDefault();void submit();}}>
      <Field label="Project name"><input class="s-input" autofocus value={name()} disabled={busy()} onInput={e=>setName(e.currentTarget.value)} placeholder="My project"/></Field>
      <Field label="Project ID"><input class="s-input" value={slug()} disabled={busy()} onInput={e=>setId(e.currentTarget.value)} /></Field>
      <div class="project-location"><span>Location</span><p>Project files on the connected backend</p><small>Folders and files belong to this project. Choose where agents run with the machine picker.</small></div>
      <Show when={error()}><p class="cs-warn" role="alert">{error()}</p></Show>
      <button type="submit" hidden />
    </form>
  </Dialog>;
}
