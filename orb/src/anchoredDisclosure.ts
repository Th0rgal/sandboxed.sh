import {onCleanup} from 'solid-js';

/** Keep the trigger in place when details are inserted above it. */
export function anchoredDisclosure() {
 let frame=0;
 onCleanup(()=>cancelAnimationFrame(frame));
 return (trigger:HTMLElement, change:()=>void)=>{
  let scroller:HTMLElement|null=trigger.parentElement;
  while(scroller&&!/(auto|scroll)/.test(getComputedStyle(scroller).overflowY))scroller=scroller.parentElement;
  const container=scroller??document.scrollingElement as HTMLElement;
  const before=trigger.getBoundingClientRect().top;
  change();
  cancelAnimationFrame(frame);
  frame=requestAnimationFrame(()=>{if(trigger.isConnected&&container)container.scrollTop+=trigger.getBoundingClientRect().top-before;});
 };
}
