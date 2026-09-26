import {createSignal, createEffect, onMount, onCleanup, Show} from 'solid-js';
import {CloseIcon, SearchIcon} from './icons';
import {indexText,matchOffsets,matchRange,type TextIndex} from './searchIndex';

/** Search rendered text without mutating Solid-owned transcript or syntax nodes. */
export function FindBar() {
  const [open,setOpen]=createSignal(false), [query,setQuery]=createSignal('');
  const [sensitive,setSensitive]=createSignal(false), [whole,setWhole]=createSignal(false);
  const [matches,setMatches]=createSignal<{start:number;end:number}[]>([]), [index,setIndex]=createSignal(0);
  const [label,setLabel]=createSignal('conversation');
  let input!:HTMLInputElement, bar!:HTMLDivElement, scope:HTMLElement|undefined, last:HTMLElement|undefined;
  const registry=()=> (CSS as any).highlights;
  const paint=(name:string,ranges:Range[])=>{const H=(window as any).Highlight;if(H&&registry())registry().set(name,new H(...ranges));};
  const clear=()=>{registry()?.delete('orb-find');registry()?.delete('orb-find-current');};
  let cached:TextIndex|undefined, observer:MutationObserver|undefined, timer:ReturnType<typeof setTimeout>|undefined;
  let previousFocus:HTMLElement|undefined;
  const [position,setPosition]=createSignal({top:'54px',right:'24px'});
  const positionBar=()=>{if(!scope)return;const r=scope.getBoundingClientRect();setPosition({top:`${Math.max(8,r.top+8)}px`,right:`${Math.max(8,innerWidth-r.right+12)}px`});};
  const close=()=>{setOpen(false);clear();observer?.disconnect();clearTimeout(timer);previousFocus?.focus({preventScroll:true});};
  const track=(e:Event)=>{if(!bar?.contains(e.target as Node))last=e.target as HTMLElement;};
  function search() {
    if(!scope||!open())return;
    cached ??= indexText(scope);
    const result=matchOffsets(cached.text,query(),sensitive(),whole());
    setIndex(i=>Math.min(i,Math.max(0,result.length-1)));setMatches(result);
  }
  createEffect(()=>{query();sensitive();whole();if(!open())return;clearTimeout(timer);timer=setTimeout(search,60);});
  createEffect(()=>{if(!open()||!cached)return;
    const all=matches(),at=index();
    // Keep the full count, but only materialize a bounded neighborhood of highlights.
    const nearby=all.slice(Math.max(0,at-40),at+81).map(m=>matchRange(cached!,m)).filter((r):r is Range=>!!r);
    paint('orb-find',nearby);
    const r=all[at]&&matchRange(cached,all[at]);paint('orb-find-current',r?[r]:[]);if(!r)return;
    const rect=r.getBoundingClientRect(),box=scope!.getBoundingClientRect();
    if(rect.top<box.top+48 || rect.bottom>box.bottom-12) scope!.scrollTop+=rect.top-box.top-scope!.clientHeight/2;
    if(rect.left<box.left || rect.right>box.right) scope!.scrollLeft+=rect.left-box.left-24;
  });
  const step=(delta:number)=>{if(matches().length)setIndex(i=>(i+delta+matches().length)%matches().length);};
  function keys(e:KeyboardEvent){
    if((e.metaKey||e.ctrlKey)&&!e.shiftKey&&!e.altKey&&e.key.toLowerCase()==='f'){
      const target=last??document.activeElement as HTMLElement;
      scope=target?.closest<HTMLElement>('.file-preview,.btw-thread,.scroll,[data-find-conversation]')??target?.closest('.file-panel')?.querySelector<HTMLElement>('.file-preview')??document.querySelector<HTMLElement>('.scroll')??undefined;
      if(!scope)return;
      e.preventDefault();e.stopImmediatePropagation();previousFocus=document.activeElement as HTMLElement;cached=undefined;observer?.disconnect();observer=new MutationObserver(()=>{cached=undefined;clearTimeout(timer);timer=setTimeout(search,120);});observer.observe(scope,{subtree:true,childList:true,characterData:true});positionBar();setLabel(scope.matches('.file-preview')?'file':'conversation');setOpen(true);search();requestAnimationFrame(()=>{input.focus();input.select();});
    }else if(open()&&e.key==='Escape'){e.preventDefault();e.stopImmediatePropagation();close();}
  }
  onMount(()=>{window.addEventListener('resize',positionBar);window.addEventListener('keydown',keys,true);window.addEventListener('pointerdown',track,true);window.addEventListener('focusin',track,true);});
  onCleanup(()=>{clear();observer?.disconnect();clearTimeout(timer);window.removeEventListener('resize',positionBar);window.removeEventListener('keydown',keys,true);window.removeEventListener('pointerdown',track,true);window.removeEventListener('focusin',track,true);});
  return <Show when={open()}><div ref={bar} class="find-bar" style={position()} role="search" aria-label={`Find in ${label()}`}>
    <SearchIcon size={15}/><input ref={input} aria-label={`Find in ${label()}`} placeholder="Find…" title={`Find in ${label()}`} value={query()} onInput={e=>setQuery(e.currentTarget.value)} onKeyDown={e=>{if(e.key==='Enter'){e.preventDefault();step(e.shiftKey?-1:1);}}}/>
    <button title="Match case" aria-pressed={sensitive()} onClick={()=>setSensitive(v=>!v)}>Aa</button>
    <button class="find-whole" title="Whole word" aria-pressed={whole()} onClick={()=>setWhole(v=>!v)}>ab</button>
    <span class="find-count" aria-live="polite">{query()?matches().length?`${index()+1} / ${matches().length}`:'No results':''}</span>
    <button title="Previous match (Shift+Enter)" disabled={!matches().length} onClick={()=>step(-1)}>↑</button><button title="Next match (Enter)" disabled={!matches().length} onClick={()=>step(1)}>↓</button>
    <button title="Close search (Esc)" onClick={close}><CloseIcon size={15}/></button>
  </div></Show>;
}
