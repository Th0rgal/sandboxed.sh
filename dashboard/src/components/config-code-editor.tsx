'use client';

import Editor from 'react-simple-code-editor';
import { highlight, languages } from 'prismjs';
import 'prismjs/components/prism-bash';
import 'prismjs/components/prism-markdown';
import 'prismjs/components/prism-yaml';
import 'prismjs/components/prism-json';
import { cn } from '@/lib/utils';

type SupportedLanguage = 'markdown' | 'bash' | 'text' | 'json';

interface ConfigCodeEditorProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  editorClassName?: string;
  minHeight?: number | string;
  language?: SupportedLanguage;
  padding?: number;
}

const languageMap: Record<SupportedLanguage, Prism.Grammar | undefined> = {
  markdown: languages.markdown,
  bash: languages.bash,
  text: undefined,
  json: languages.json,
};

const escapeHtml = (code: string) =>
  code
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');

export function ConfigCodeEditor({
  value,
  onChange,
  placeholder,
  disabled = false,
  className,
  editorClassName,
  minHeight = '100%',
  language = 'markdown',
  padding = 12,
}: ConfigCodeEditorProps) {
  const grammar = languageMap[language];
  const highlightCode = (code: string) => {
    if (!grammar) return escapeHtml(code);
    return highlight(code, grammar, language);
  };

  return (
    <div
      className={cn(
        'rounded-lg bg-[#0d0d0e] border border-white/[0.06] overflow-auto focus-within:border-indigo-500/50 transition-colors',
        disabled && 'opacity-60',
        className
      )}
      aria-disabled={disabled}
    >
      <Editor
        value={value}
        onValueChange={onChange}
        highlight={highlightCode}
        padding={padding}
        placeholder={placeholder}
        readOnly={disabled}
        spellCheck={false}
        className={cn('config-code-editor', editorClassName)}
        textareaClassName="focus:outline-none"
        style={{
          fontFamily:
            'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
          fontSize: 14,
          lineHeight: 1.6,
          color: 'rgba(255, 255, 255, 0.9)',
          minHeight,
          height: '100%',
        }}
      />
    </div>
  );
}
