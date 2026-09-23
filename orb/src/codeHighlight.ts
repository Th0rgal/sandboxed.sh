import "./prismSetup";
import Prism from "prismjs";
import "prismjs/components/prism-python";
import "prismjs/components/prism-typescript";
import "prismjs/components/prism-rust";
import "prismjs/components/prism-bash";
import "prismjs/components/prism-json";
import "prismjs/components/prism-yaml";
import "prismjs/components/prism-sql";
import "prismjs/components/prism-go";
import "prismjs/components/prism-c";
import "prismjs/components/prism-cpp";
import "prismjs/components/prism-solidity";
import "prismjs/components/prism-toml";
import "prismjs/components/prism-diff";

export function highlightCode(text: string, label: string): string | null {
  const lang = label.toLowerCase().split(/\s+/)[0];
  const grammar = Prism.languages[lang];
  if (!grammar || text.length > 100_000) return null;
  return Prism.highlight(text, grammar, lang);
}
