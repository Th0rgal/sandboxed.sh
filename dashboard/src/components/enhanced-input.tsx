'use client';

import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { listLibraryCommands, getVisibleAgents, type CommandSummary } from '@/lib/api';
import { cn } from '@/lib/utils';

// Built-in oh-my-opencode commands
const BUILTIN_COMMANDS: CommandSummary[] = [
  { name: 'ralph-loop', description: 'Start self-referential development loop until completion', path: 'builtin' },
  { name: 'cancel-ralph', description: 'Cancel active Ralph Loop', path: 'builtin' },
  { name: 'start-work', description: 'Start Sisyphus work session from Prometheus plan', path: 'builtin' },
  { name: 'refactor', description: 'Intelligent refactoring with LSP, AST-grep, and TDD verification', path: 'builtin' },
  { name: 'init-deep', description: 'Initialize hierarchical AGENTS.md knowledge base', path: 'builtin' },
];

export interface SubmitPayload {
  content: string;
  agent?: string;
}

interface EnhancedInputProps {
  value: string;
  onChange: (value: string) => void;
  onSubmit: (payload: SubmitPayload) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
}

interface AutocompleteItem {
  type: 'command' | 'agent';
  name: string;
  description: string | null;
  source?: string;
}

export function EnhancedInput({
  value,
  onChange,
  onSubmit,
  placeholder = "Message the root agent...",
  disabled = false,
  className,
}: EnhancedInputProps) {
  const [commands, setCommands] = useState<CommandSummary[]>([]);
  const [agents, setAgents] = useState<string[]>([]);
  const [showAutocomplete, setShowAutocomplete] = useState(false);
  const [autocompleteItems, setAutocompleteItems] = useState<AutocompleteItem[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [autocompleteType, setAutocompleteType] = useState<'command' | 'agent' | null>(null);
  const [triggerPosition, setTriggerPosition] = useState(0);

  // Track locked agent badge separately for cleaner UX
  const [lockedAgent, setLockedAgent] = useState<string | null>(null);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const autocompleteRef = useRef<HTMLDivElement>(null);

  // Load commands and agents on mount
  useEffect(() => {
    async function loadData() {
      try {
        const libraryCommands = await listLibraryCommands();
        setCommands([...BUILTIN_COMMANDS, ...libraryCommands]);
      } catch {
        setCommands(BUILTIN_COMMANDS);
      }

      try {
        const agentsData = await getVisibleAgents();
        const agentNames = parseAgentNames(agentsData);
        setAgents(agentNames);
      } catch {
        // Use empty array on failure - backend validates agents anyway
        // This prevents suggesting non-existent agents from stale fallbacks
        setAgents([]);
      }
    }
    loadData();
  }, []);

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

    textarea.style.height = "auto";
    const lineHeight = 20;
    const maxLines = 10;
    const maxHeight = lineHeight * maxLines;
    const newHeight = Math.min(textarea.scrollHeight, maxHeight);
    textarea.style.height = `${newHeight}px`;
  }, []);

  useEffect(() => {
    adjustTextareaHeight();
  }, [displayValue, adjustTextareaHeight]);

  // Detect triggers (/ or @) and update autocomplete
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;

    const cursorPos = textarea.selectionStart;
    const textBeforeCursor = displayValue.substring(0, cursorPos);

    // Check for / command trigger at start of line or after whitespace
    const commandMatch = textBeforeCursor.match(/(?:^|\s)(\/[\w-]*)$/);
    if (commandMatch) {
      const searchTerm = commandMatch[1].substring(1).toLowerCase();
      const filtered = commands.filter(cmd =>
        cmd.name.toLowerCase().includes(searchTerm)
      );
      setAutocompleteItems(filtered.map(cmd => ({
        type: 'command',
        name: cmd.name,
        description: cmd.description,
        source: cmd.path === 'builtin' ? 'oh-my-opencode' : 'library',
      })));
      setAutocompleteType('command');
      setTriggerPosition(cursorPos - commandMatch[1].length);
      setShowAutocomplete(filtered.length > 0);
      setSelectedIndex(0);
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
        setAutocompleteType('agent');
        setTriggerPosition(0);
        setShowAutocomplete(filtered.length > 0);
        setSelectedIndex(0);
        return;
      }
    }

    setShowAutocomplete(false);
    setAutocompleteType(null);
  }, [displayValue, commands, agents, lockedAgent]);

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

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
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
    textareaRef.current?.focus();
  };

  const handleSubmit = () => {
    const trimmedValue = displayValue.trim();
    if (!trimmedValue && !lockedAgent) return;
    if (disabled) return;

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

    // Clear state after submit
    setLockedAgent(null);
    onChange('');
  };

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const newValue = e.target.value;

    // If user types space after @agent pattern, lock it as badge
    if (!lockedAgent) {
      const match = newValue.match(/^@([\w-]+)\s$/);
      if (match) {
        const agentName = match[1];
        setLockedAgent(agentName);
        onChange(''); // Agent is now in badge, clear text
        return;
      }
    }

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
          "flex items-start gap-2 w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 transition-[border-color] duration-150 ease-out focus-within:border-indigo-500/50",
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

        {/* Textarea - shows full value when no locked badge, or just the rest when locked */}
        <textarea
          ref={textareaRef}
          value={lockedAgent ? displayValue : value}
          onChange={handleChange}
          onKeyDown={handleKeyDown}
          placeholder={lockedAgent ? "Type your message..." : placeholder}
          disabled={disabled}
          rows={1}
          className="flex-1 bg-transparent text-sm text-white placeholder-white/30 focus:outline-none resize-none overflow-y-auto leading-5"
          style={{
            minHeight: "20px",
            maxHeight: "200px",
          }}
        />
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
                <div className="flex items-center gap-2">
                  <span className="font-medium text-white text-sm">
                    {item.name}
                  </span>
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
}
