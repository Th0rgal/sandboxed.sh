import Prism from "prismjs";

// Prism does not ship a Lean grammar. Keep this shared by the worker and tests.
const lean: Prism.Grammar = {
  comment: [
    { pattern: /\/-(?:[^/-]|\/(?!-)|-(?!\/)|\/-[\s\S]*?-\/)*-\//, greedy: true },
    { pattern: /--[^\r\n]*/, greedy: true },
  ],
  string: { pattern: /"(?:\\[\s\S]|[^"\\])*"/, greedy: true },
  keyword: /\b(?:abbrev|axiom|by|calc|class|def|deriving|do|else|end|example|export|extends|for|forall|from|fun|have|if|import|in|inductive|infix|infixl|infixr|instance|let|macro|match|mutual|namespace|noncomputable|notation|opaque|open|partial|private|protected|return|section|set_option|show|structure|syntax|termination_by|then|theorem|universe|unsafe|variable|variables|where|with)\b|[∀∃λ]/,
  builtin: /\b(?:apply|assumption|cases|constructor|decide|exact|ext|intro|intros|rcases|refine|rfl|rw|simp|simpa|sorry|subst|unfold|use|omega|linarith|nlinarith|norm_num|ring|aesop)\b/,
  "class-name": /\b(?:Bool|Char|Fin|Float|Int|List|Nat|Option|Prop|Sort|String|Type|UIntN|UInt8|UInt16|UInt32|UInt64|Unit)\b/,
  boolean: /\b(?:true|false|True|False)\b/,
  number: /\b(?:0[xX][\da-fA-F]+|0[bB][01]+|\d+(?:\.\d+)?)\b/,
  operator: /:=|=>|->|<-|[→←↔≤≥≠∧∨¬∈∉⊆⊂∪∩+*/%=<>!^|&~:-]/,
  punctuation: /[{}[\]();,.⟨⟩]/,
};
Prism.languages.lean = lean;
Prism.languages.lean4 = lean;
