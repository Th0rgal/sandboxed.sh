'use client';

import { useState, useEffect, useRef, useCallback, useMemo, forwardRef, useImperativeHandle, memo } from 'react';
import { listLibraryCommands, getBuiltinCommands as fetchBuiltinCommands, getVisibleAgents, type CommandSummary, type CommandParam } from '@/lib/api';
import { cn } from '@/lib/utils';

// Fallback builtin commands (used if API fails)
const FALLBACK_OPENCODE_COMMANDS: CommandSummary[] = [
  { name: 'ralph-loop', description: 'Start self-referential development loop until completion', path: 'builtin' },
  { name: 'cancel-ralph', description: 'Cancel active Ralph Loop', path: 'builtin' },
];

const FALLBACK_CLAUDECODE_COMMANDS: CommandSummary[] = [
  { name: 'plan', description: 'Enter plan mode to design an implementation approach', path: 'builtin-claude' },
  { name: 'compact', description: 'Compact conversation history to save context', path: 'builtin-claude' },
  { name: 'clear', description: 'Clear conversation history and start fresh', path: 'builtin-claude' },
];

const FALLBACK_CODEX_COMMANDS: CommandSummary[] = [
  {
    name: 'goal',
    description: 'Loop until the objective is achieved (codex 0.128.0+)',
    path: 'builtin-codex',
    params: [
      { name: 'objective', required: true, description: 'What the agent should keep iterating on until done' },
    ],
  },
];

export interface SubmitPayload {
  content: string;
  agent?: string;
}

export interface EnhancedInputHandle {
  submit: () => void;
  canSubmit: () => boolean;
  clear: () => void;
  /** Restore a previously-submitted draft with its locked-agent badge
   *  intact (used when the parent's send path fails after clear()). */
  restoreDraft: (content: string, agent?: string | null) => void;
}

export interface FilePasteContext {
  selectionStart: number;
  selectionEnd: number;
}

interface EnhancedInputProps {
  value: string;
  onChange: (value: string) => void;
  onSubmit: (payload: SubmitPayload) => void;
  onCanSubmitChange?: (canSubmit: boolean) => void;
  /** Called when files are pasted (e.g., images from clipboard) */
  onFilePaste?: (files: File[], context: FilePasteContext) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  /** Backend type for the current mission ("opencode" or "claudecode") */
  backend?: string;
}

interface AutocompleteItem {
  type: 'command' | 'agent';
  name: string;
  description: string | null;
  source?: string;
  params?: CommandParam[];
}

const parseAgentNames = (payload: unknown): string[] => {
  const normalizeEntry = (entry: unknown): string | null => {
    if (typeof entry === 'string') return entry;
    if (entry && typeof entry === 'object') {
      const name = (entry as { name?: unknown }).name;
      if (typeof name === 'string') return name;
      const id = (entry as { id?: unknown }).id;
      if (typeof id === 'string') return id;
    }
    return null;
  };

  const raw = Array.isArray(payload)
    ? payload
    : (payload as { agents?: unknown })?.agents;
  if (!Array.isArray(raw)) return [];

  const names = raw
    .map(normalizeEntry)
    .filter((name): name is string => Boolean(name));
  return Array.from(new Set(names));
};

const getAgentDescription = (name: string): string => {
  const descriptions: Record<string, string> = {
    'Sisyphus': 'Main orchestrator with parallel execution',
    'oracle': 'Architecture, code review, strategy (GPT)',
    'explore': 'Fast codebase exploration and search',
    'librarian': 'Documentation lookup and research',
    'plan': 'Prometheus planner for structured work',
    'frontend-ui-ux-engineer': 'UI/UX development specialist',
    'document-writer': 'Technical documentation expert',
    'multimodal-looker': 'Visual content analysis',
  };
  return descriptions[name] || 'Specialized agent';
};

export const EnhancedInput = memo(forwardRef<EnhancedInputHandle, EnhancedInputProps>(function EnhancedInput({
  value: externalValue,
  onChange: externalOnChange,
  onSubmit,
  onCanSubmitChange,
  onFilePaste,
  placeholder = "Message the root agent...",
  disabled = false,
  className,
  backend,
}, ref) {
  // Internal state for the input value — immune to parent re-renders.
  // Syncs FROM parent only when the parent explicitly pushes a new value
  // (e.g., clearing after submit, inserting upload notes).
  const [value, setValueState] = useState(externalValue);
  const lastExternalValueRef = useRef(externalValue);

  // Sync external → internal only when the parent pushes a genuinely new value
  useEffect(() => {
    if (externalValue !== lastExternalValueRef.current) {
      lastExternalValueRef.current = externalValue;
      // Parent-driven changes are explicit actions such as clearing after a send
      // or inserting upload notes; mirror them into the local draft buffer.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setValueState(externalValue);
    }
  }, [externalValue]);

  // Wrapper that updates both internal state and notifies parent
  const onChange = useCallback((newValue: string) => {
    setValueState(newValue);
    lastExternalValueRef.current = newValue;
    externalOnChange(newValue);
  }, [externalOnChange]);

  const [commands, setCommands] = useState<CommandSummary[]>([]);
  const [agents, setAgents] = useState<string[]>([]);
  const [showAutocomplete, setShowAutocomplete] = useState(false);
  const [autocompleteItems, setAutocompleteItems] = useState<AutocompleteItem[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [triggerPosition, setTriggerPosition] = useState(0);
  const [ghostText, setGhostText] = useState<string>('');

  // Track locked agent badge separately for cleaner UX
  const [lockedAgent, setLockedAgent] = useState<string | null>(null);

  // Message history for undo / arrow-key recall
  const HISTORY_KEY = 'enhanced-input-message-history';
  const MAX_HISTORY = 10;
  const sentHistoryRef = useRef<string[]>([]);
  const historyIndexRef = useRef(-1); // -1 = not browsing history
  const savedDraftRef = useRef(''); // saves current draft when entering history
  const savedLockedAgentRef = useRef<string | null>(null); // saves lockedAgent when entering history

  // Load history from localStorage on mount
  useEffect(() => {
    try {
      const stored = localStorage.getItem(HISTORY_KEY);
      if (stored) sentHistoryRef.current = JSON.parse(stored);
    } catch { /* ignore */ }
  }, []);

  const pushToHistory = useCallback((message: string) => {
    const trimmed = message.trim();
    if (!trimmed) return;
    const history = sentHistoryRef.current;
    // Deduplicate: remove if already most recent
    if (history.length > 0 && history[history.length - 1] === trimmed) return;
    history.push(trimmed);
    if (history.length > MAX_HISTORY) history.splice(0, history.length - MAX_HISTORY);
    try { localStorage.setItem(HISTORY_KEY, JSON.stringify(history)); } catch { /* ignore */ }
    historyIndexRef.current = -1;
  }, []);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const autocompleteRef = useRef<HTMLDivElement>(null);
  const resizeTextarea = useCallback((textarea: HTMLTextAreaElement) => {
    textarea.style.height = "auto";
    const lineHeight = 20;
    const maxLines = 10;
    const maxHeight = lineHeight * maxLines;
    const newHeight = Math.min(textarea.scrollHeight, maxHeight);
    textarea.style.height = `${newHeight}px`;
  }, []);

  // Load commands and agents on mount or when backend changes
  useEffect(() => {
    let isMounted = true;

    async function loadData() {
      // Fetch builtin commands from backend API
      let builtinCommands: CommandSummary[] = [];
      try {
        const builtinResponse = await fetchBuiltinCommands();
        const builtinByBackend: Record<string, CommandSummary[]> = {
          claudecode: builtinResponse.claudecode,
          opencode: builtinResponse.opencode,
          // Codex got native slash commands in 0.128.0 (currently just /goal).
          // The backend returns an empty array for older binaries; the field
          // is optional in the response type.
          codex: builtinResponse.codex ?? [],
        };
        if (backend) {
          builtinCommands = builtinByBackend[backend] ?? [];
        } else {
          // No backend selected yet, show every known builtin set.
          builtinCommands = [
            ...builtinResponse.opencode,
            ...builtinResponse.claudecode,
            ...(builtinResponse.codex ?? []),
          ];
        }
      } catch {
        // Use fallback commands if API fails
        const fallbackByBackend: Record<string, CommandSummary[]> = {
          claudecode: FALLBACK_CLAUDECODE_COMMANDS,
          opencode: FALLBACK_OPENCODE_COMMANDS,
          codex: FALLBACK_CODEX_COMMANDS,
        };
        if (backend) {
          builtinCommands = fallbackByBackend[backend] ?? [];
        } else {
          builtinCommands = [
            ...FALLBACK_OPENCODE_COMMANDS,
            ...FALLBACK_CLAUDECODE_COMMANDS,
            ...FALLBACK_CODEX_COMMANDS,
          ];
        }
      }

      // Fetch library commands
      try {
        const libraryCommands = await listLibraryCommands();
        if (isMounted) {
          setCommands([...builtinCommands, ...libraryCommands]);
        }
      } catch {
        if (isMounted) {
          setCommands(builtinCommands);
        }
      }

      // Fetch agents
      try {
        const agentsData = await getVisibleAgents();
        const agentNames = parseAgentNames(agentsData);
        if (isMounted) {
          setAgents(agentNames);
        }
      } catch {
        // Use empty array on failure - backend validates agents anyway
        // This prevents suggesting non-existent agents from stale fallbacks
        if (isMounted) {
          setAgents([]);
        }
      }
    }
    loadData();

    return () => {
      isMounted = false;
    };
  }, [backend]);

  // Check if an agent name is valid
  const isValidAgent = useCallback((name: string) => {
    return agents.some(a => a.toLowerCase() === name.toLowerCase());
  }, [agents]);

  // Parse the current value for agent mention (when not using locked badge)
  const parsedAgentFromValue = useMemo(() => {
    if (lockedAgent) return null; // Badge is locked, don't parse from value
    const match = value.match(/^@([\w-]+)(\s|$)/);
    if (match) {
      return {
        name: match[1],
        isValid: isValidAgent(match[1]),
        hasSpace: match[2] === ' ',
      };
    }
    return null;
  }, [value, lockedAgent, isValidAgent]);

  // The actual content to show in textarea (excludes locked agent prefix)
  const displayValue = useMemo(() => {
    if (lockedAgent) {
      return value; // Value is already without the @agent prefix
    }
    return value;
  }, [value, lockedAgent]);

  // Auto-resize textarea
  const adjustTextareaHeight = useCallback(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;
    resizeTextarea(textarea);
  }, [resizeTextarea]);

  useEffect(() => {
    adjustTextareaHeight();
  }, [displayValue, adjustTextareaHeight]);

  // Detect triggers (/ or @) and update autocomplete
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;

    const cursorPos = textarea.selectionStart;
    const textBeforeCursor = displayValue.substring(0, cursorPos);

    // Check for / command trigger — only at the very start of the input.
    // A `/` mid-message (e.g. inside a URL like `https://...`) shouldn't pop
    // the autocomplete; users only ever invoke a slash command as the first
    // token of the message.
    const commandMatch = textBeforeCursor.match(/^(\/[\w-]*)$/);
    if (commandMatch) {
      const searchTerm = commandMatch[1].substring(1).toLowerCase();
      const filtered = commands.filter(cmd =>
        cmd.name.toLowerCase().includes(searchTerm)
      );
      setAutocompleteItems(filtered.map(cmd => ({
        type: 'command',
        name: cmd.name,
        description: cmd.description,
        source: cmd.path === 'builtin' ? 'oh-my-opencode' : cmd.path === 'builtin-claude' ? 'claude-code' : 'library',
        params: cmd.params,
      })));
      setTriggerPosition(0);
      setShowAutocomplete(filtered.length > 0);
      setSelectedIndex(0);

      // Compute ghost text for the best matching command
      if (filtered.length > 0 && searchTerm.length > 0) {
        // Find commands that start with the search term (prefix match)
        const prefixMatches = filtered.filter(cmd =>
          cmd.name.toLowerCase().startsWith(searchTerm)
        );
        if (prefixMatches.length > 0) {
          const bestMatch = prefixMatches[0];
          const remaining = bestMatch.name.substring(searchTerm.length);
          // Show remaining command name + short hint from description
          const firstSentence = bestMatch.description?.split('.')[0] ?? '';
          const truncated = firstSentence.substring(0, 40);
          const hint = bestMatch.description
            ? ` — ${truncated}${firstSentence.length > 40 ? '…' : ''}`
            : '';
          setGhostText(remaining + hint);
        } else {
          setGhostText('');
        }
      } else {
        setGhostText('');
      }
      return;
    }

    // Check for @ agent trigger - only at start and only if no locked agent
    if (!lockedAgent) {
      const agentMatch = textBeforeCursor.match(/^@([\w-]*)$/);
      if (agentMatch) {
        const searchTerm = agentMatch[1].toLowerCase();
        const filtered = agents.filter(agent =>
          agent.toLowerCase().includes(searchTerm)
        );
        setAutocompleteItems(filtered.map(agent => ({
          type: 'agent',
          name: agent,
          description: getAgentDescription(agent),
        })));
        setTriggerPosition(0);
        setShowAutocomplete(filtered.length > 0);
        setSelectedIndex(0);

        // Compute ghost text for the best matching agent
        if (filtered.length > 0 && searchTerm.length > 0) {
          const prefixMatches = filtered.filter(agent =>
            agent.toLowerCase().startsWith(searchTerm)
          );
          if (prefixMatches.length > 0) {
            const bestMatch = prefixMatches[0];
            const remaining = bestMatch.substring(searchTerm.length);
            const desc = getAgentDescription(bestMatch);
            const hint = desc ? ` — ${desc.substring(0, 30)}${desc.length > 30 ? '…' : ''}` : '';
            setGhostText(remaining + hint);
          } else {
            setGhostText('');
          }
        } else {
          setGhostText('');
        }
        return;
      }
    }

    setShowAutocomplete(false);
    setGhostText('');
  }, [displayValue, commands, agents, lockedAgent]);

  const navigateHistory = useCallback((direction: 'back' | 'forward') => {
    const history = sentHistoryRef.current;
    if (history.length === 0) return false;

    if (direction === 'back') {
      if (historyIndexRef.current === -1) {
        // Entering history mode — save current draft and locked agent
        savedDraftRef.current = value;
        savedLockedAgentRef.current = lockedAgent;
        historyIndexRef.current = history.length - 1;
      } else if (historyIndexRef.current > 0) {
        historyIndexRef.current--;
      } else {
        return false; // Already at oldest
      }
      const msg = history[historyIndexRef.current];
      setLockedAgent(null);
      onChange(msg);
      return true;
    } else {
      if (historyIndexRef.current === -1) return false; // Not in history mode
      if (historyIndexRef.current < history.length - 1) {
        historyIndexRef.current++;
        const msg = history[historyIndexRef.current];
        setLockedAgent(null);
        onChange(msg);
      } else {
        // Past newest → restore draft and locked agent
        historyIndexRef.current = -1;
        setLockedAgent(savedLockedAgentRef.current);
        onChange(savedDraftRef.current);
      }
      return true;
    }
  }, [value, lockedAgent, onChange]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Cmd/Ctrl+Z: navigate history back (empty input or already browsing)
    if (e.key.toLowerCase() === 'z' && (e.metaKey || e.ctrlKey) && !e.shiftKey) {
      const isEmpty = displayValue.trim() === '';
      const isBrowsing = historyIndexRef.current !== -1;
      if ((isEmpty || isBrowsing) && sentHistoryRef.current.length > 0) {
        if (navigateHistory('back')) {
          e.preventDefault();
          return;
        }
      }
    }

    // Cmd/Ctrl+Shift+Z: navigate history forward (while browsing)
    if (e.key.toLowerCase() === 'z' && (e.metaKey || e.ctrlKey) && e.shiftKey) {
      if (historyIndexRef.current !== -1) {
        if (navigateHistory('forward')) {
          e.preventDefault();
          return;
        }
      }
    }

    // Handle backspace on locked agent badge
    if (e.key === 'Backspace' && lockedAgent && displayValue === '') {
      e.preventDefault();
      setLockedAgent(null);
      onChange(`@${lockedAgent}`); // Put back the @agent text for editing
      return;
    }

    if (showAutocomplete) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setSelectedIndex(prev =>
          prev < autocompleteItems.length - 1 ? prev + 1 : 0
        );
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setSelectedIndex(prev =>
          prev > 0 ? prev - 1 : autocompleteItems.length - 1
        );
        return;
      }
      if (e.key === 'Tab' || e.key === 'Enter') {
        if (autocompleteItems.length > 0) {
          e.preventDefault();
          selectItem(autocompleteItems[selectedIndex]);
          return;
        }
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        setShowAutocomplete(false);
        return;
      }
    }

    // Up arrow on empty input or at position 0: navigate history back
    if (e.key === 'ArrowUp' && !showAutocomplete) {
      const textarea = textareaRef.current;
      const isEmpty = displayValue.trim() === '';
      const atStart = textarea && textarea.selectionStart === 0 && textarea.selectionEnd === 0;
      if (isEmpty || (atStart && !displayValue.includes('\n'))) {
        if (navigateHistory('back')) {
          e.preventDefault();
          return;
        }
      }
    }

    // Down arrow while browsing history: navigate forward
    if (e.key === 'ArrowDown' && !showAutocomplete && historyIndexRef.current !== -1) {
      e.preventDefault();
      navigateHistory('forward');
      return;
    }

    // Normal Enter to submit (without Shift)
    if (e.key === 'Enter' && !e.shiftKey && !showAutocomplete) {
      e.preventDefault();
      handleSubmit();
    }
  };

  const selectItem = (item: AutocompleteItem) => {
    if (item.type === 'command') {
      const before = displayValue.substring(0, triggerPosition);
      const after = displayValue.substring(textareaRef.current?.selectionStart || displayValue.length);
      const newValue = `${before}/${item.name} ${after}`.trim();
      onChange(newValue);
    } else if (item.type === 'agent') {
      // Lock the agent as a badge and clear the text
      setLockedAgent(item.name);
      onChange(''); // Clear the @partial text, agent is now in badge
    }
    setShowAutocomplete(false);
    setGhostText('');
    textareaRef.current?.focus();
  };

  const handleSubmit = useCallback(() => {
    const trimmedValue = displayValue.trim();
    if (!trimmedValue && !lockedAgent) return;
    if (disabled) return;

    // Save to history before clearing (include agent prefix for full recall)
    const fullMessage = lockedAgent
      ? (trimmedValue ? `@${lockedAgent} ${trimmedValue}` : `@${lockedAgent}`)
      : value.trim();
    pushToHistory(fullMessage);

    if (lockedAgent) {
      // Locked agent badge mode
      if (trimmedValue) {
        onSubmit({ content: trimmedValue, agent: lockedAgent });
      } else {
        // Just @agent with no content - send as-is
        onSubmit({ content: `@${lockedAgent}` });
      }
    } else if (parsedAgentFromValue) {
      // Agent typed but not locked (user typed @agent and space)
      const content = value.substring(parsedAgentFromValue.name.length + 1).trim();
      if (content) {
        onSubmit({ content, agent: parsedAgentFromValue.name });
      } else {
        onSubmit({ content: value });
      }
    } else {
      onSubmit({ content: value });
    }

    // Intentionally does NOT clear here. Clearing is driven by the
    // parent via the imperative `clear()` handle so a submission that
    // fails upstream (mission sync error, post rejection) can leave the
    // user's draft intact. See enhanced-input.test.tsx — "does not
    // clear text synchronously when submitted".
  }, [displayValue, lockedAgent, disabled, onSubmit, parsedAgentFromValue, value, pushToHistory]);

  // Check if submission is valid (has content or locked agent)
  const canSubmit = useCallback(() => {
    if (disabled) return false;
    const trimmedValue = displayValue.trim();
    return !!(trimmedValue || lockedAgent);
  }, [disabled, displayValue, lockedAgent]);

  // Notify parent when submission validity changes
  useEffect(() => {
    onCanSubmitChange?.(canSubmit());
  }, [canSubmit, onCanSubmitChange]);

  // Expose submit method via ref so parent can trigger submit (e.g., from Send button)
  useImperativeHandle(ref, () => ({
    submit: handleSubmit,
    canSubmit,
    clear: () => {
      setLockedAgent(null);
      onChange('');
    },
    restoreDraft: (content: string, agent?: string | null) => {
      setLockedAgent(agent ?? null);
      onChange(content);
    },
  }), [handleSubmit, canSubmit, onChange]);

  // Handle paste events for file uploads (e.g., pasting screenshots)
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea || !onFilePaste) return;

    const handlePaste = (event: ClipboardEvent) => {
      const items = event.clipboardData?.items;
      if (!items) return;

      const files: File[] = [];
      for (const item of items) {
        if (item.kind === "file") {
          const file = item.getAsFile();
          if (file) files.push(file);
        }
      }

      if (files.length === 0) return;

      // If there's text being pasted too, let the default behavior handle it
      const textData = event.clipboardData?.getData("text/plain") ?? "";
      if (textData.trim().length > 0) {
        return;
      }

      // Prevent default paste and handle file upload
      event.preventDefault();
      onFilePaste(files, {
        selectionStart: textarea.selectionStart ?? 0,
        selectionEnd: textarea.selectionEnd ?? 0,
      });
    };

    textarea.addEventListener("paste", handlePaste);
    return () => textarea.removeEventListener("paste", handlePaste);
  }, [onFilePaste]);

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const newValue = e.target.value;
    const currentValue = lockedAgent ? displayValue : value;

    resizeTextarea(e.currentTarget);

    // Skip if no actual change (prevents infinite render loops)
    if (newValue === currentValue) return;

    // If user types space after @agent pattern, lock it as badge
    if (!lockedAgent) {
      const match = newValue.match(/^@([\w-]+)\s$/);
      if (match) {
        const agentName = match[1];
        setLockedAgent(agentName);
        onCanSubmitChange?.(true);
        onChange(''); // Agent is now in badge, clear text
        return;
      }
    }

    // User is typing normally — exit history browsing mode
    historyIndexRef.current = -1;
    onCanSubmitChange?.(!!(newValue.trim() || lockedAgent));
    onChange(newValue);
  };

  const removeBadge = () => {
    if (lockedAgent) {
      onChange(`@${lockedAgent}${displayValue}`);
      setLockedAgent(null);
      textareaRef.current?.focus();
    }
  };

  // Determine badge state for display - only show when locked
  const badgeState = useMemo(() => {
    if (lockedAgent) {
      return {
        show: true,
        text: `@${lockedAgent}`,
        isValid: isValidAgent(lockedAgent),
      };
    }
    return { show: false, text: '', isValid: false };
  }, [lockedAgent, isValidAgent]);

  return (
    <div className="relative flex-1">
      <div
        className={cn(
          "flex items-center gap-2 w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 transition-[border-color] duration-150 ease-out focus-within:border-indigo-500/50",
          className
        )}
        style={{ minHeight: "46px" }}
      >
        {/* Badge (locked agent) */}
        {badgeState.show && (
          <button
            type="button"
            onClick={removeBadge}
            className={cn(
              "inline-flex items-center rounded px-1.5 py-0.5 text-sm font-medium border shrink-0 transition-colors cursor-pointer",
              badgeState.isValid
                ? "bg-emerald-500/20 text-emerald-300 border-emerald-500/30 hover:bg-emerald-500/30"
                : "bg-orange-500/20 text-orange-300 border-orange-500/30 hover:bg-orange-500/30"
            )}
            title="Click to remove"
          >
            {badgeState.text}
            <span className="ml-1 opacity-60">×</span>
          </button>
        )}

        {/* Textarea with ghost text overlay */}
        <div className="relative flex-1 flex items-center">
          <textarea
            ref={textareaRef}
            value={lockedAgent ? displayValue : value}
            onChange={handleChange}
            onKeyDown={handleKeyDown}
            placeholder={lockedAgent ? "Type your message..." : placeholder}
            disabled={disabled}
            rows={1}
            className="w-full bg-transparent text-sm text-white placeholder-white/30 focus:outline-none resize-none overflow-y-auto leading-[1.4]"
            style={{
              minHeight: "20px",
              maxHeight: "200px",
            }}
          />
          {/* Ghost text overlay - positioned to appear after input text */}
          {ghostText && (
            <div
              className="absolute top-0 left-0 pointer-events-none text-sm leading-[1.4] whitespace-pre-wrap overflow-hidden"
              style={{
                minHeight: "20px",
                maxHeight: "200px",
              }}
              aria-hidden="true"
            >
              {/* Invisible text matching the input to position ghost text correctly */}
              <span className="invisible">{lockedAgent ? displayValue : value}</span>
              {/* Visible ghost text with reduced opacity */}
              <span className="text-white/30">{ghostText}</span>
            </div>
          )}
        </div>
      </div>

      {/* Autocomplete dropdown */}
      {showAutocomplete && autocompleteItems.length > 0 && (
        <div
          ref={autocompleteRef}
          className="absolute bottom-full left-0 right-0 mb-2 max-h-64 overflow-y-auto rounded-lg border border-white/[0.08] bg-[#1a1a1a] shadow-xl z-50"
        >
          {autocompleteItems.map((item, index) => (
            <button
              key={`${item.type}-${item.name}`}
              type="button"
              onClick={() => selectItem(item)}
              className={cn(
                "w-full px-3 py-2.5 text-left flex items-start gap-3 transition-colors",
                index === selectedIndex
                  ? "bg-white/[0.08]"
                  : "hover:bg-white/[0.04]"
              )}
            >
              <span className="text-white/40 font-mono text-sm shrink-0">
                {item.type === 'command' ? '/' : '@'}
              </span>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="font-medium text-white text-sm">
                    {item.name}
                  </span>
                  {item.params && item.params.length > 0 && (
                    <span className="text-xs text-white/40 font-mono">
                      {item.params.map(p => p.required ? `<${p.name}>` : `[${p.name}]`).join(' ')}
                    </span>
                  )}
                  {item.source && (
                    <span className="text-xs text-white/30 px-1.5 py-0.5 rounded bg-white/[0.05]">
                      {item.source}
                    </span>
                  )}
                </div>
                {item.description && (
                  <p className="text-xs text-white/50 mt-0.5 truncate">
                    {item.description}
                  </p>
                )}
              </div>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}));
