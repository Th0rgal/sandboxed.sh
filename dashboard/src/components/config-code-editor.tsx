'use client';

import dynamic from 'next/dynamic';
import { cn } from '@/lib/utils';

// Dynamic import to avoid SSR issues with react-simple-code-editor
const Editor = dynamic(() => import('react-simple-code-editor').then(mod => mod.default), {
  ssr: false,
  loading: () => <div className="animate-pulse bg-white/5 rounded h-32" />,
});

type Language = 'json' | 'markdown' | 'bash' | 'plain';

interface ConfigCodeEditorProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  editorClassName?: string;
  minHeight?: number | string;
  padding?: number;
  /** Enable highlighting of <encrypted>...</encrypted> tags */
  highlightEncrypted?: boolean;
  /** Language for syntax highlighting */
  language?: Language;
}

// Escape HTML special characters
const escapeHtml = (str: string): string =>
  str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');

// Simple JSON syntax highlighting
function highlightJson(code: string): string {
  let html = escapeHtml(code);

  // Process in a single pass to avoid double-highlighting
  // Match strings, numbers, booleans/null, and punctuation
  html = html.replace(
    /(&quot;)((?:[^&]|&(?!quot;))*)(&quot;)|(-?\b\d+\.?\d*\b)|\b(true|false|null)\b|([{}\[\]:,])/g,
    (match, strOpen, strContent, strClose, num, bool, punct) => {
      if (strOpen) {
        // String (key or value)
        return `<span class="token string">${strOpen}${strContent}${strClose}</span>`;
      } else if (num) {
        // Number
        return `<span class="token number">${num}</span>`;
      } else if (bool) {
        // Boolean or null
        return `<span class="token boolean">${bool}</span>`;
      } else if (punct) {
        // Punctuation
        return `<span class="token punctuation">${punct}</span>`;
      }
      return match;
    }
  );

  return html;
}

// Simple Markdown syntax highlighting
function highlightMarkdown(code: string): string {
  let html = escapeHtml(code);

  // Code blocks (``` ... ```) - must be done first
  html = html.replace(
    /^(```)(\w*)([\s\S]*?)(```)$/gm,
    '<span class="token comment">$1$2$3$4</span>'
  );

  // Inline code (`...`)
  html = html.replace(
    /(`[^`\n]+`)/g,
    '<span class="token string">$1</span>'
  );

  // Headers (# ## ### etc)
  html = html.replace(
    /^(#{1,6}\s.*)$/gm,
    '<span class="token keyword">$1</span>'
  );

  // Bold (**text** or __text__)
  html = html.replace(
    /(\*\*|__)([^*_]+)(\*\*|__)/g,
    '<span class="token important">$1$2$3</span>'
  );

  // Links [text](url)
  html = html.replace(
    /(\[)([^\]]+)(\]\()([^)]+)(\))/g,
    '<span class="token punctuation">$1</span><span class="token string">$2</span><span class="token punctuation">$3</span><span class="token url">$4</span><span class="token punctuation">$5</span>'
  );

  // List items (- or * or numbers)
  html = html.replace(
    /^(\s*)([-*]|\d+\.)\s/gm,
    '$1<span class="token punctuation">$2</span> '
  );

  // YAML frontmatter delimiter
  html = html.replace(
    /^(---)\s*$/gm,
    '<span class="token comment">$1</span>'
  );

  // YAML-style keys in frontmatter (key: value)
  html = html.replace(
    /^(\s*)(\w+)(:)/gm,
    '$1<span class="token property">$2</span><span class="token punctuation">$3</span>'
  );

  return html;
}

// Simple Bash syntax highlighting
function highlightBash(code: string): string {
  let html = escapeHtml(code);

  // Comments
  html = html.replace(
    /^(\s*)(#.*)$/gm,
    '$1<span class="token comment">$2</span>'
  );

  // Strings
  html = html.replace(
    /(&quot;[^&]*(?:&(?!quot;)[^&]*)*&quot;)/g,
    '<span class="token string">$1</span>'
  );
  html = html.replace(
    /('[^']*')/g,
    '<span class="token string">$1</span>'
  );

  // Variables
  html = html.replace(
    /(\$\w+|\$\{[^}]+\})/g,
    '<span class="token variable">$1</span>'
  );

  // Keywords
  html = html.replace(
    /\b(if|then|else|elif|fi|for|while|do|done|case|esac|function|return|exit|export|source|alias)\b/g,
    '<span class="token keyword">$1</span>'
  );

  return html;
}

// Highlight encrypted tags (including failed-to-decrypt ones)
function highlightEncryptedTags(html: string): string {
  // First handle failed-to-decrypt tags (show in red)
  html = html.replace(
    /&lt;encrypted-failed(?:\s+v=&quot;\d+&quot;)?&gt;(.*?)&lt;\/encrypted-failed&gt;/g,
    '<span class="token-encrypted-failed-tag">&lt;encrypted-failed&gt;</span><span class="token-encrypted-failed-value">$1</span><span class="token-encrypted-failed-tag">&lt;/encrypted-failed&gt;</span>'
  );

  // Then handle normal encrypted tags (show in yellow)
  html = html.replace(
    /&lt;encrypted(?:\s+v=&quot;\d+&quot;)?&gt;(.*?)&lt;\/encrypted&gt;/g,
    '<span class="token-encrypted-tag">&lt;encrypted&gt;</span><span class="token-encrypted-value">$1</span><span class="token-encrypted-tag">&lt;/encrypted&gt;</span>'
  );

  return html;
}

export function ConfigCodeEditor({
  value,
  onChange,
  placeholder,
  disabled = false,
  className,
  editorClassName,
  minHeight = '100%',
  padding = 12,
  highlightEncrypted = false,
  language = 'plain',
}: ConfigCodeEditorProps) {
  const highlightCode = (code: string): string => {
    let html: string;

    switch (language) {
      case 'json':
        html = highlightJson(code);
        break;
      case 'markdown':
        html = highlightMarkdown(code);
        break;
      case 'bash':
        html = highlightBash(code);
        break;
      default:
        html = escapeHtml(code);
    }

    // Apply encrypted tag highlighting if enabled
    if (highlightEncrypted) {
      html = highlightEncryptedTags(html);
    }

    return html;
  };

  // Check if value contains encrypted tags for visual indicator
  const hasEncryptedContent = highlightEncrypted && /<encrypted(?:\s+v="\d+")?>/i.test(value);
  const hasFailedEncryptedContent = highlightEncrypted && /<encrypted-failed/i.test(value);

  return (
    <div
      className={cn(
        'rounded-lg bg-[#0d0d0e] border border-white/[0.06] focus-within:border-indigo-500/50 transition-colors overflow-auto relative',
        disabled && 'opacity-60',
        className
      )}
      aria-disabled={disabled}
    >
      {hasFailedEncryptedContent && (
        <div className="absolute top-2 right-2 px-2 py-0.5 rounded text-[10px] font-medium bg-red-500/20 text-red-400 border border-red-500/30 pointer-events-none z-10">
          ⚠️ Decryption failed - re-enter values
        </div>
      )}
      {hasEncryptedContent && !hasFailedEncryptedContent && (
        <div className="absolute top-2 right-2 px-2 py-0.5 rounded text-[10px] font-medium bg-amber-500/20 text-amber-400 border border-amber-500/30 pointer-events-none z-10">
          Contains encrypted values
        </div>
      )}
      <Editor
        value={value}
        onValueChange={onChange}
        highlight={highlightCode}
        padding={padding}
        placeholder={placeholder}
        readOnly={disabled}
        className={cn('config-code-editor', editorClassName)}
        textareaClassName="focus:outline-none"
        style={{
          fontFamily:
            '"JetBrainsMono Nerd Font Mono", ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
          fontSize: 14,
          lineHeight: 1.5,
          fontVariantLigatures: 'none',
          fontFeatureSettings: '"liga" 0, "calt" 0',
          fontKerning: 'none',
          letterSpacing: '0',
          color: 'rgba(255, 255, 255, 0.9)',
          minHeight,
        }}
      />
    </div>
  );
}
