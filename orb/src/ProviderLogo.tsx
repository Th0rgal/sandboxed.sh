import { Show } from "solid-js";
import { ProvidersIcon } from "./icons";

/** Local monochrome Lobe Icons assets; never load branding from a remote host. */
export function ProviderLogo(p: { type: string; name?: string }) {
  const brand = () => {
    const key = p.type.toLowerCase();
    if (/spark|nvidia|local/.test(`${key} ${p.name?.toLowerCase() ?? ""}`)) return "nvidia";
    if (/anthropic|claude/.test(key)) return "anthropic";
    if (/openai|codex|chatgpt/.test(key)) return "openai";
    if (/kimi|moonshot/.test(key)) return "kimi";
    if (/xai|grok/.test(key)) return "xai";
    if (/meta|llama/.test(key)) return "meta";
    if (/minimax/.test(key)) return "minimax";
    if (/z[._-]?ai|zhipu|glm/.test(key)) return "zai";
    return undefined;
  };
  return <span class="provider-logo" aria-hidden="true"><Show when={brand()} fallback={<ProvidersIcon size={19} />}>
    {name => <span style={{ "mask-image": `url(/provider-icons/${name()}.svg)`, "-webkit-mask-image": `url(/provider-icons/${name()}.svg)` }} />}
  </Show></span>;
}
