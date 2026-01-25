'use client';

import { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import {
  getLibrarySkill,
  getSkillReference,
  saveSkillReference,
  deleteSkillReference,
  importSkill,
  validateSkillName,
  type Skill,
} from '@/lib/api';
import {
  GitBranch,
  RefreshCw,
  Upload,
  Check,
  AlertCircle,
  Loader,
  Plus,
  Save,
  Trash2,
  X,
  File,
  Folder,
  FolderOpen,
  ChevronRight,
  ChevronDown,
  Download,
  FileText,
  ExternalLink,
  Pencil,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { useLibrary } from '@/contexts/library-context';
import { ConfigCodeEditor } from '@/components/config-code-editor';
import { RenameDialog } from '@/components/rename-dialog';

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

interface FileNode {
  name: string;
  path: string;
  type: 'file' | 'folder';
  children?: FileNode[];
}

interface Frontmatter {
  description?: string;
  name?: string;
  license?: string;
  compatibility?: string;
  [key: string]: string | undefined;
}

// ─────────────────────────────────────────────────────────────────────────────
// Utility Functions
// ─────────────────────────────────────────────────────────────────────────────

function buildFileTree(references: string[]): FileNode[] {
  const root: FileNode[] = [];

  // Always include SKILL.md at the top
  root.push({ name: 'SKILL.md', path: 'SKILL.md', type: 'file' });

  // Build tree from references
  for (const ref of references) {
    const parts = ref.split('/');
    let current = root;

    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      const isLast = i === parts.length - 1;
      const currentPath = parts.slice(0, i + 1).join('/');

      let existing = current.find(n => n.name === part);

      if (!existing) {
        existing = {
          name: part,
          path: currentPath,
          type: isLast ? 'file' : 'folder',
          children: isLast ? undefined : [],
        };
        current.push(existing);
      }

      if (!isLast && existing.children) {
        current = existing.children;
      }
    }
  }

  // Sort: folders first, then alphabetically
  const sortNodes = (nodes: FileNode[]): FileNode[] => {
    return nodes.sort((a, b) => {
      if (a.name === 'SKILL.md') return -1;
      if (b.name === 'SKILL.md') return 1;
      if (a.type !== b.type) return a.type === 'folder' ? -1 : 1;
      return a.name.localeCompare(b.name);
    }).map(node => ({
      ...node,
      children: node.children ? sortNodes(node.children) : undefined,
    }));
  };

  return sortNodes(root);
}

function parseFrontmatter(content: string): { frontmatter: Frontmatter; body: string } {
  if (!content.startsWith('---')) {
    return { frontmatter: {}, body: content };
  }

  const endIndex = content.indexOf('\n---', 3);
  if (endIndex === -1) {
    return { frontmatter: {}, body: content };
  }

  const yamlStr = content.substring(4, endIndex);
  const body = content.substring(endIndex + 4).trimStart();

  // Simple YAML parsing for key: value pairs
  const frontmatter: Frontmatter = {};
  for (const line of yamlStr.split('\n')) {
    const match = line.match(/^(\w+):\s*(.*)$/);
    if (match) {
      frontmatter[match[1]] = match[2].replace(/^["']|["']$/g, '');
    }
  }

  return { frontmatter, body };
}

// YAML special characters that require quoting
const YAML_SPECIAL_CHARS = [':', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', "'", '"', '%', '@', '`'];

/**
 * Format a YAML value, quoting if it contains special characters.
 * This prevents YAML parsing errors when descriptions contain colons (e.g., "Triggers: foo, bar").
 */
function formatYamlValue(value: string): string {
  // Check if value needs quoting
  const needsQuoting = YAML_SPECIAL_CHARS.some(char => value.includes(char));
  
  if (needsQuoting) {
    // Escape backslashes and double quotes, then wrap in quotes
    const escaped = value.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
    return `"${escaped}"`;
  }
  
  return value;
}

/**
 * Check if a frontmatter value contains YAML special characters that need quoting.
 * Returns an array of field names that have problematic values.
 */
function checkFrontmatterForYamlIssues(frontmatter: Frontmatter): string[] {
  const problematicFields: string[] = [];
  
  for (const [key, value] of Object.entries(frontmatter)) {
    if (value && YAML_SPECIAL_CHARS.some(char => value.includes(char))) {
      problematicFields.push(key);
    }
  }
  
  return problematicFields;
}

function buildContent(frontmatter: Frontmatter, body: string): string {
  const entries = Object.entries(frontmatter).filter(([_, v]) => v !== undefined && v !== '');
  if (entries.length === 0) {
    return body;
  }

  // Quote values that contain YAML special characters
  const yaml = entries.map(([k, v]) => `${k}: ${formatYamlValue(v!)}`).join('\n');
  return `---\n${yaml}\n---\n\n${body}`;
}

// ─────────────────────────────────────────────────────────────────────────────
// File Tree Component
// ─────────────────────────────────────────────────────────────────────────────

interface FileTreeProps {
  nodes: FileNode[];
  selectedPath: string | null;
  onSelect: (path: string) => void;
  onDelete: (path: string) => void;
  expandedFolders: Set<string>;
  onToggleFolder: (path: string) => void;
  level?: number;
}

function FileTree({
  nodes,
  selectedPath,
  onSelect,
  onDelete,
  expandedFolders,
  onToggleFolder,
  level = 0,
}: FileTreeProps) {
  return (
    <div className="space-y-0.5">
      {nodes.map((node) => (
        <div key={node.path}>
          <div
            className={cn(
              'flex items-center gap-1.5 px-2 py-1.5 rounded-lg cursor-pointer transition-colors group',
              selectedPath === node.path
                ? 'bg-white/[0.08] text-white'
                : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
            )}
            style={{ paddingLeft: `${level * 12 + 8}px` }}
            onClick={() => {
              if (node.type === 'folder') {
                onToggleFolder(node.path);
              } else {
                onSelect(node.path);
              }
            }}
          >
            {node.type === 'folder' ? (
              <>
                {expandedFolders.has(node.path) ? (
                  <ChevronDown className="h-3 w-3 text-white/40" />
                ) : (
                  <ChevronRight className="h-3 w-3 text-white/40" />
                )}
                {expandedFolders.has(node.path) ? (
                  <FolderOpen className="h-3.5 w-3.5 text-amber-400" />
                ) : (
                  <Folder className="h-3.5 w-3.5 text-amber-400" />
                )}
              </>
            ) : (
              <>
                <span className="w-3" />
                {node.name === 'SKILL.md' ? (
                  <FileText className="h-3.5 w-3.5 text-indigo-400" />
                ) : (
                  <File className="h-3.5 w-3.5 text-white/40" />
                )}
              </>
            )}
            <span className="text-xs truncate flex-1">{node.name}</span>
            {node.name !== 'SKILL.md' && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onDelete(node.path);
                }}
                className="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-red-500/20 transition-all"
              >
                <Trash2 className="h-3 w-3 text-red-400" />
              </button>
            )}
          </div>
          {node.type === 'folder' && node.children && expandedFolders.has(node.path) && (
            <FileTree
              nodes={node.children}
              selectedPath={selectedPath}
              onSelect={onSelect}
              onDelete={onDelete}
              expandedFolders={expandedFolders}
              onToggleFolder={onToggleFolder}
              level={level + 1}
            />
          )}
        </div>
      ))}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Frontmatter Editor Component
// ─────────────────────────────────────────────────────────────────────────────

interface FrontmatterEditorProps {
  frontmatter: Frontmatter;
  onChange: (frontmatter: Frontmatter) => void;
  disabled?: boolean;
}

function FrontmatterEditor({ frontmatter, onChange, disabled }: FrontmatterEditorProps) {
  const updateField = (key: string, value: string) => {
    onChange({ ...frontmatter, [key]: value || undefined });
  };

  // Check if description contains special YAML characters
  const descriptionHasSpecialChars = frontmatter.description && 
    YAML_SPECIAL_CHARS.some(char => frontmatter.description?.includes(char));

  return (
    <div className="space-y-3 p-3 rounded-lg bg-white/[0.02] border border-white/[0.06]">
      <div className="text-xs font-medium text-white/60 uppercase tracking-wide">Frontmatter</div>

      <div className="space-y-2">
        <div>
          <label className="block text-xs text-white/40 mb-1">Description *</label>
          <input
            type="text"
            value={frontmatter.description || ''}
            onChange={(e) => updateField('description', e.target.value)}
            placeholder="Brief description of what this skill does"
            className="w-full px-3 py-1.5 text-xs rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
            disabled={disabled}
          />
          {descriptionHasSpecialChars && (
            <p className="text-xs text-blue-400/80 mt-1 flex items-center gap-1">
              <Check className="h-3 w-3" />
              Contains special characters — will be auto-quoted for valid YAML
            </p>
          )}
        </div>

        <div className="grid grid-cols-2 gap-2">
          <div>
            <label className="block text-xs text-white/40 mb-1">License</label>
            <input
              type="text"
              value={frontmatter.license || ''}
              onChange={(e) => updateField('license', e.target.value)}
              placeholder="MIT"
              className="w-full px-3 py-1.5 text-xs rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
              disabled={disabled}
            />
          </div>
          <div>
            <label className="block text-xs text-white/40 mb-1">Compatibility</label>
            <input
              type="text"
              value={frontmatter.compatibility || ''}
              onChange={(e) => updateField('compatibility', e.target.value)}
              placeholder="opencode >=1.0"
              className="w-full px-3 py-1.5 text-xs rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
              disabled={disabled}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Import Dialog Component
// ─────────────────────────────────────────────────────────────────────────────

interface ImportDialogProps {
  open: boolean;
  onClose: () => void;
  onImport: (skill: Skill) => void;
}

function ImportDialog({ open, onClose, onImport }: ImportDialogProps) {
  const [url, setUrl] = useState('');
  const [path, setPath] = useState('');
  const [name, setName] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();

    if (!url.trim()) {
      setError('Please enter a Git URL');
      return;
    }

    // Validate name if provided
    if (name.trim()) {
      const validation = validateSkillName(name.trim());
      if (!validation.valid) {
        setError(validation.error || 'Invalid skill name');
        return;
      }
    }

    setLoading(true);
    setError(null);

    try {
      const skill = await importSkill({
        url: url.trim(),
        path: path.trim() || undefined,
        name: name.trim() || undefined,
      });
      onImport(skill);
      setUrl('');
      setPath('');
      setName('');
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to import skill');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    if (open) {
      document.addEventListener('keydown', handleKeyDown);
      return () => document.removeEventListener('keydown', handleKeyDown);
    }
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="w-full max-w-lg p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-lg font-medium text-white">Import Skill from Git</h3>
          <button onClick={onClose} className="p-1 rounded hover:bg-white/[0.06]">
            <X className="h-4 w-4 text-white/60" />
          </button>
        </div>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div>
            <label className="block text-sm text-white/60 mb-1.5">Git Repository URL *</label>
            <input
              type="text"
              value={url}
              onChange={(e) => {
                setUrl(e.target.value);
                setError(null);
              }}
              placeholder="https://github.com/owner/repo.git"
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
              disabled={loading}
            />
            <p className="text-xs text-white/40 mt-1">
              Public repositories only. For private repos, use SSH URLs.
            </p>
          </div>

          <div>
            <label className="block text-sm text-white/60 mb-1.5">Path within Repository</label>
            <input
              type="text"
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="path/to/skill (optional)"
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
              disabled={loading}
            />
            <p className="text-xs text-white/40 mt-1">
              For monorepos, specify the path to the skill folder containing SKILL.md
            </p>
          </div>

          <div>
            <label className="block text-sm text-white/60 mb-1.5">Skill Name</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
              placeholder="my-skill (auto-detected if empty)"
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
              disabled={loading}
            />
            <p className="text-xs text-white/40 mt-1">
              Lowercase alphanumeric with hyphens. Auto-detected from URL if empty.
            </p>
          </div>

          {error && (
            <div className="p-3 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 text-sm">
              {error}
            </div>
          )}

          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 text-sm text-white/60 hover:text-white"
              disabled={loading}
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={loading || !url.trim()}
              className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {loading ? (
                <>
                  <Loader className="h-4 w-4 animate-spin" />
                  Importing...
                </>
              ) : (
                <>
                  <Download className="h-4 w-4" />
                  Import
                </>
              )}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// New File Dialog Component
// ─────────────────────────────────────────────────────────────────────────────

interface NewFileDialogProps {
  open: boolean;
  onClose: () => void;
  onCreate: (path: string, isFolder: boolean) => void;
  skillName: string;
}

function NewFileDialog({ open, onClose, onCreate, skillName }: NewFileDialogProps) {
  const [fileName, setFileName] = useState('');
  const [isFolder, setIsFolder] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    const trimmed = fileName.trim();
    if (!trimmed) {
      setError('Please enter a name');
      return;
    }

    if (trimmed.includes('..') || trimmed.startsWith('/')) {
      setError('Invalid path');
      return;
    }

    onCreate(trimmed, isFolder);
    setFileName('');
    setIsFolder(false);
    setError(null);
    onClose();
  };

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    if (open) {
      document.addEventListener('keydown', handleKeyDown);
      return () => document.removeEventListener('keydown', handleKeyDown);
    }
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
        <h3 className="text-lg font-medium text-white mb-4">
          New {isFolder ? 'Folder' : 'File'}
        </h3>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="flex gap-2 mb-4">
            <button
              type="button"
              onClick={() => setIsFolder(false)}
              className={cn(
                'flex-1 flex items-center justify-center gap-2 px-3 py-2 rounded-lg border transition-colors',
                !isFolder
                  ? 'border-indigo-500/50 bg-indigo-500/10 text-white'
                  : 'border-white/[0.08] text-white/60 hover:bg-white/[0.04]'
              )}
            >
              <File className="h-4 w-4" />
              File
            </button>
            <button
              type="button"
              onClick={() => setIsFolder(true)}
              className={cn(
                'flex-1 flex items-center justify-center gap-2 px-3 py-2 rounded-lg border transition-colors',
                isFolder
                  ? 'border-indigo-500/50 bg-indigo-500/10 text-white'
                  : 'border-white/[0.08] text-white/60 hover:bg-white/[0.04]'
              )}
            >
              <Folder className="h-4 w-4" />
              Folder
            </button>
          </div>

          <div>
            <label className="block text-sm text-white/60 mb-1.5">
              {isFolder ? 'Folder' : 'File'} Path
            </label>
            <input
              type="text"
              value={fileName}
              onChange={(e) => {
                setFileName(e.target.value);
                setError(null);
              }}
              placeholder={isFolder ? 'references' : 'references/example.md'}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
            />
            <p className="text-xs text-white/40 mt-1">
              Relative to skills/{skillName}/
            </p>
          </div>

          {error && (
            <p className="text-sm text-red-400">{error}</p>
          )}

          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 text-sm text-white/60 hover:text-white"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={!fileName.trim()}
              className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
            >
              Create
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Main Skills Page
// ─────────────────────────────────────────────────────────────────────────────

export default function SkillsPage() {
  const {
    status,
    skills,
    loading,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
    sync,
    commit,
    push,
    saveSkill,
    removeSkill,
    syncing,
    committing,
    pushing,
  } = useLibrary();

  // Skill selection state
  const [selectedSkill, setSelectedSkill] = useState<Skill | null>(null);
  const [loadingSkill, setLoadingSkill] = useState(false);

  // File tree state
  const [fileTree, setFileTree] = useState<FileNode[]>([]);
  const [expandedFolders, setExpandedFolders] = useState<Set<string>>(new Set());
  const [selectedFile, setSelectedFile] = useState<string | null>(null);

  // Editor state
  const [fileContent, setFileContent] = useState('');
  const [frontmatter, setFrontmatter] = useState<Frontmatter>({});
  const [bodyContent, setBodyContent] = useState('');
  const [isDirty, setIsDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loadingFile, setLoadingFile] = useState(false);

  // Dialog state
  const [showNewSkillDialog, setShowNewSkillDialog] = useState(false);
  const [showImportDialog, setShowImportDialog] = useState(false);
  const [showNewFileDialog, setShowNewFileDialog] = useState(false);
  const [showCommitDialog, setShowCommitDialog] = useState(false);
  const [showRenameDialog, setShowRenameDialog] = useState(false);
  const [newSkillName, setNewSkillName] = useState('');
  const [newSkillError, setNewSkillError] = useState<string | null>(null);
  const [commitMessage, setCommitMessage] = useState('');

  // Ref to track content changes
  const contentRef = useRef({ frontmatter, bodyContent });
  contentRef.current = { frontmatter, bodyContent };

  // Build file tree when skill changes
  useEffect(() => {
    if (selectedSkill) {
      setFileTree(buildFileTree(selectedSkill.references));
      // Expand all folders by default
      const folders = new Set<string>();
      const collectFolders = (nodes: FileNode[]) => {
        for (const node of nodes) {
          if (node.type === 'folder') {
            folders.add(node.path);
            if (node.children) collectFolders(node.children);
          }
        }
      };
      collectFolders(buildFileTree(selectedSkill.references));
      setExpandedFolders(folders);
    }
  }, [selectedSkill]);

  // Handle keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 's') {
        e.preventDefault();
        if (isDirty && selectedSkill && selectedFile) {
          handleSave();
        }
      }
      if (e.key === 'Escape') {
        if (showNewSkillDialog) setShowNewSkillDialog(false);
        if (showImportDialog) setShowImportDialog(false);
        if (showNewFileDialog) setShowNewFileDialog(false);
        if (showCommitDialog) setShowCommitDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [isDirty, selectedSkill, selectedFile, showNewSkillDialog, showImportDialog, showNewFileDialog, showCommitDialog]);

  const loadSkill = async (name: string) => {
    try {
      setLoadingSkill(true);
      const skill = await getLibrarySkill(name);
      setSelectedSkill(skill);
      setSelectedFile('SKILL.md');

      // Parse and set SKILL.md content
      const { frontmatter: fm, body } = parseFrontmatter(skill.content);
      setFrontmatter(fm);
      setBodyContent(body);
      setFileContent(skill.content);
      setIsDirty(false);
    } catch (err) {
      console.error('Failed to load skill:', err);
    } finally {
      setLoadingSkill(false);
    }
  };

  const loadFile = async (path: string) => {
    if (!selectedSkill) return;

    if (path === 'SKILL.md') {
      const { frontmatter: fm, body } = parseFrontmatter(selectedSkill.content);
      setFrontmatter(fm);
      setBodyContent(body);
      setFileContent(selectedSkill.content);
      setSelectedFile(path);
      setIsDirty(false);
      return;
    }

    try {
      setLoadingFile(true);
      const content = await getSkillReference(selectedSkill.name, path);
      setFileContent(content);
      setBodyContent(content);
      setFrontmatter({});
      setSelectedFile(path);
      setIsDirty(false);
    } catch (err) {
      console.error('Failed to load file:', err);
    } finally {
      setLoadingFile(false);
    }
  };

  const handleSave = async () => {
    if (!selectedSkill || !selectedFile) return;

    setSaving(true);
    try {
      if (selectedFile === 'SKILL.md') {
        const content = buildContent(frontmatter, bodyContent);
        await saveSkill(selectedSkill.name, content);
        // Reload skill to get updated references
        const updated = await getLibrarySkill(selectedSkill.name);
        setSelectedSkill(updated);
        setFileContent(content);
      } else {
        await saveSkillReference(selectedSkill.name, selectedFile, fileContent);
      }
      setIsDirty(false);
    } catch (err) {
      console.error('Failed to save:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleCreateSkill = async () => {
    const name = newSkillName.trim();
    const validation = validateSkillName(name);

    if (!validation.valid) {
      setNewSkillError(validation.error || 'Invalid skill name');
      return;
    }

    const template = `---
description: A new skill
---

# ${name}

Describe what this skill does.
`;

    try {
      setSaving(true);
      await saveSkill(name, template);
      setShowNewSkillDialog(false);
      setNewSkillName('');
      setNewSkillError(null);
      await loadSkill(name);
    } catch (err) {
      setNewSkillError(err instanceof Error ? err.message : 'Failed to create skill');
    } finally {
      setSaving(false);
    }
  };

  const handleDeleteSkill = async () => {
    if (!selectedSkill) return;
    if (!confirm(`Delete skill "${selectedSkill.name}" and all its files?`)) return;

    try {
      await removeSkill(selectedSkill.name);
      setSelectedSkill(null);
      setSelectedFile(null);
      setFileContent('');
      setFrontmatter({});
      setBodyContent('');
    } catch (err) {
      console.error('Failed to delete skill:', err);
    }
  };

  const handleDeleteFile = async (path: string) => {
    if (!selectedSkill) return;
    if (path === 'SKILL.md') return; // Can't delete SKILL.md

    if (!confirm(`Delete "${path}"?`)) return;

    try {
      await deleteSkillReference(selectedSkill.name, path);
      // Reload skill to get updated references
      const updated = await getLibrarySkill(selectedSkill.name);
      setSelectedSkill(updated);

      if (selectedFile === path) {
        setSelectedFile('SKILL.md');
        const { frontmatter: fm, body } = parseFrontmatter(updated.content);
        setFrontmatter(fm);
        setBodyContent(body);
        setFileContent(updated.content);
        setIsDirty(false);
      }
    } catch (err) {
      console.error('Failed to delete file:', err);
    }
  };

  const handleCreateFile = async (path: string, isFolder: boolean) => {
    if (!selectedSkill) return;

    try {
      if (isFolder) {
        // Create a placeholder file to create the folder
        await saveSkillReference(selectedSkill.name, `${path}/.gitkeep`, '');
      } else {
        // Create empty file with default content based on extension
        const ext = path.split('.').pop()?.toLowerCase();
        let content = '';
        if (ext === 'md') {
          content = `# ${path.split('/').pop()?.replace('.md', '')}\n\nContent here.\n`;
        }
        await saveSkillReference(selectedSkill.name, path, content);
      }

      // Reload skill to get updated references
      const updated = await getLibrarySkill(selectedSkill.name);
      setSelectedSkill(updated);

      if (!isFolder) {
        setSelectedFile(path);
        setFileContent('');
        setBodyContent('');
        setFrontmatter({});
        setIsDirty(false);
      }
    } catch (err) {
      console.error('Failed to create file:', err);
    }
  };

  const handleToggleFolder = (path: string) => {
    setExpandedFolders((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  };

  const handleFrontmatterChange = (newFm: Frontmatter) => {
    setFrontmatter(newFm);
    setIsDirty(true);
  };

  const handleBodyChange = (value: string) => {
    setBodyContent(value);
    if (selectedFile !== 'SKILL.md') {
      setFileContent(value);
    }
    setIsDirty(true);
  };

  const handleSync = async () => {
    try {
      await sync();
    } catch {
      // Error handled by context
    }
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    try {
      await commit(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
    } catch {
      // Error handled by context
    }
  };

  const handlePush = async () => {
    try {
      await push();
    } catch {
      // Error handled by context
    }
  };

  const handleImportSuccess = async (skill: Skill) => {
    await refresh();
    await loadSkill(skill.name);
  };

  const handleRenameSuccess = async () => {
    await refresh();
    // The skill was renamed, so we need to clear selection
    // since the old name no longer exists
    setSelectedSkill(null);
    setSelectedFile(null);
    setFileContent('');
    setFrontmatter({});
    setBodyContent('');
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  if (libraryUnavailable) {
    return <LibraryUnavailable message={libraryUnavailableMessage} onConfigured={refresh} />;
  }

  return (
    <div className="h-screen flex flex-col p-6 gap-4 overflow-hidden">
      {/* Git Status Bar */}
      {status && (
        <div className="p-4 rounded-xl bg-white/[0.02] border border-white/[0.06]">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-4">
              <div className="flex items-center gap-2">
                <GitBranch className="h-4 w-4 text-white/40" />
                <span className="text-sm font-medium text-white">{status.branch}</span>
              </div>
              <div className="flex items-center gap-2">
                {status.clean ? (
                  <span className="flex items-center gap-1 text-xs text-emerald-400">
                    <Check className="h-3 w-3" />
                    Clean
                  </span>
                ) : (
                  <span className="flex items-center gap-1 text-xs text-amber-400">
                    <AlertCircle className="h-3 w-3" />
                    {status.modified_files.length} modified
                  </span>
                )}
              </div>
              {(status.ahead > 0 || status.behind > 0) && (
                <div className="text-xs text-white/40">
                  {status.ahead > 0 && <span className="text-emerald-400">+{status.ahead}</span>}
                  {status.ahead > 0 && status.behind > 0 && ' / '}
                  {status.behind > 0 && <span className="text-amber-400">-{status.behind}</span>}
                </div>
              )}
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={handleSync}
                disabled={syncing}
                className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
              >
                <RefreshCw className={cn('h-3 w-3', syncing && 'animate-spin')} />
                Sync
              </button>
              {!status.clean && (
                <button
                  onClick={() => setShowCommitDialog(true)}
                  disabled={committing}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
                >
                  <Check className="h-3 w-3" />
                  Commit
                </button>
              )}
              {status.ahead > 0 && (
                <button
                  onClick={handlePush}
                  disabled={pushing}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-emerald-400 hover:text-emerald-300 bg-emerald-500/10 hover:bg-emerald-500/20 rounded-lg transition-colors disabled:opacity-50"
                >
                  <Upload className={cn('h-3 w-3', pushing && 'animate-pulse')} />
                  Push
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {/* Main Content */}
      <div className="flex-1 min-h-0 rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden flex">
        {/* Skills List */}
        <div className="w-56 border-r border-white/[0.06] flex flex-col min-h-0">
          <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
            <span className="text-xs font-medium text-white/60">
              Skills{skills.length ? ` (${skills.length})` : ''}
            </span>
            <div className="flex items-center gap-1">
              <button
                onClick={() => setShowImportDialog(true)}
                className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
                title="Import from Git"
              >
                <Download className="h-3.5 w-3.5 text-white/60" />
              </button>
              <button
                onClick={() => setShowNewSkillDialog(true)}
                className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
                title="New Skill"
              >
                <Plus className="h-3.5 w-3.5 text-white/60" />
              </button>
            </div>
          </div>
          <div className="flex-1 min-h-0 overflow-y-auto p-2">
            {skills.length === 0 ? (
              <div className="text-center py-8">
                <FileText className="h-8 w-8 text-white/20 mx-auto mb-2" />
                <p className="text-xs text-white/40 mb-3">No skills yet</p>
                <button
                  onClick={() => setShowNewSkillDialog(true)}
                  className="text-xs text-indigo-400 hover:text-indigo-300"
                >
                  Create your first skill
                </button>
              </div>
            ) : (
              skills.map((skill) => {
                // Use frontmatter description for selected skill as fallback
                // (backend YAML parser may fail on special chars, but frontend parser succeeds)
                const description = selectedSkill?.name === skill.name
                  ? (skill.description || frontmatter.description)
                  : skill.description;
                return (
                  <button
                    key={skill.name}
                    onClick={() => loadSkill(skill.name)}
                    className={cn(
                      'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                      selectedSkill?.name === skill.name
                        ? 'bg-white/[0.08] text-white'
                        : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                    )}
                  >
                    <p className="text-sm font-medium truncate">{skill.name}</p>
                    {description && (
                      <p className="text-xs text-white/40 truncate">{description}</p>
                    )}
                  </button>
                );
              })
            )}
          </div>
        </div>

        {/* File Tree */}
        {selectedSkill && (
          <div className="w-52 border-r border-white/[0.06] flex flex-col min-h-0">
            <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
              <span className="text-xs font-medium text-white/60 truncate">
                {selectedSkill.name}
              </span>
              <button
                onClick={() => setShowNewFileDialog(true)}
                className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
                title="New File"
              >
                <Plus className="h-3.5 w-3.5 text-white/60" />
              </button>
            </div>
            <div className="flex-1 min-h-0 overflow-y-auto p-2">
              {loadingSkill ? (
                <div className="flex items-center justify-center py-4">
                  <Loader className="h-4 w-4 animate-spin text-white/40" />
                </div>
              ) : (
                <FileTree
                  nodes={fileTree}
                  selectedPath={selectedFile}
                  onSelect={loadFile}
                  onDelete={handleDeleteFile}
                  expandedFolders={expandedFolders}
                  onToggleFolder={handleToggleFolder}
                />
              )}
            </div>
          </div>
        )}

        {/* Editor */}
        <div className="flex-1 min-h-0 min-w-0 flex flex-col">
          {selectedSkill && selectedFile ? (
            <>
              <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                <div className="min-w-0">
                  <p className="text-sm font-medium text-white truncate">{selectedFile}</p>
                  <p className="text-xs text-white/40">skills/{selectedSkill.name}/{selectedFile}</p>
                </div>
                <div className="flex items-center gap-2">
                  {isDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                  {selectedFile === 'SKILL.md' && (
                    <>
                      <button
                        onClick={() => setShowRenameDialog(true)}
                        className="p-1.5 rounded-lg text-white/60 hover:text-white hover:bg-white/[0.06] transition-colors"
                        title="Rename Skill"
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </button>
                      <button
                        onClick={handleDeleteSkill}
                        className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                        title="Delete Skill"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    </>
                  )}
                  <button
                    onClick={handleSave}
                    disabled={saving || !isDirty}
                    className={cn(
                      'flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                      isDirty
                        ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                        : 'text-white/40 bg-white/[0.04]'
                    )}
                  >
                    <Save className={cn('h-3 w-3', saving && 'animate-pulse')} />
                    Save
                  </button>
                </div>
              </div>

              <div className="flex-1 min-h-0 p-3 overflow-y-auto">
                {loadingFile ? (
                  <div className="flex items-center justify-center h-full">
                    <Loader className="h-5 w-5 animate-spin text-white/40" />
                  </div>
                ) : (
                  <div className="flex flex-col gap-3">
                    {selectedFile === 'SKILL.md' && (
                      <FrontmatterEditor
                        frontmatter={frontmatter}
                        onChange={handleFrontmatterChange}
                        disabled={saving}
                      />
                    )}
                    <div>
                      <label className="block text-xs text-white/40 mb-1.5">
                        {selectedFile === 'SKILL.md' ? 'Body Content' : 'Content'}
                      </label>
                      <ConfigCodeEditor
                        value={bodyContent}
                        onChange={handleBodyChange}
                        disabled={saving}
                        highlightEncrypted={
                          selectedFile === 'SKILL.md' ||
                          selectedFile?.toLowerCase().endsWith('.md') ||
                          selectedFile?.toLowerCase().endsWith('.mdx') ||
                          selectedFile?.toLowerCase().endsWith('.markdown')
                        }
                        language={
                          selectedFile?.toLowerCase().endsWith('.json')
                            ? 'json'
                            : selectedFile?.toLowerCase().endsWith('.sh') ||
                                selectedFile?.toLowerCase().endsWith('.bash')
                              ? 'bash'
                              : 'markdown'
                        }
                      />
                    </div>
                  </div>
                )}
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center text-white/40 text-sm">
              {selectedSkill ? 'Select a file to edit' : 'Select a skill to get started'}
            </div>
          )}
        </div>
      </div>

      {/* New Skill Dialog */}
      {showNewSkillDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Skill</h3>
            <div className="space-y-4">
              <div>
                <label className="block text-sm text-white/60 mb-1.5">Skill Name</label>
                <input
                  type="text"
                  placeholder="my-skill"
                  value={newSkillName}
                  onChange={(e) => {
                    setNewSkillName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'));
                    setNewSkillError(null);
                  }}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
                <p className="text-xs text-white/40 mt-1">
                  Lowercase alphanumeric with hyphens (e.g., my-skill)
                </p>
              </div>
              {newSkillError && (
                <p className="text-sm text-red-400">{newSkillError}</p>
              )}
              <div className="flex justify-end gap-2">
                <button
                  onClick={() => {
                    setShowNewSkillDialog(false);
                    setNewSkillName('');
                    setNewSkillError(null);
                  }}
                  className="px-4 py-2 text-sm text-white/60 hover:text-white"
                >
                  Cancel
                </button>
                <button
                  onClick={handleCreateSkill}
                  disabled={!newSkillName.trim() || saving}
                  className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
                >
                  {saving ? 'Creating...' : 'Create'}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Commit Dialog */}
      {showCommitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Commit Changes</h3>
            <input
              type="text"
              placeholder="Commit message..."
              value={commitMessage}
              onChange={(e) => setCommitMessage(e.target.value)}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-4"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowCommitDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCommit}
                disabled={!commitMessage.trim() || committing}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {committing ? 'Committing...' : 'Commit'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Import Dialog */}
      <ImportDialog
        open={showImportDialog}
        onClose={() => setShowImportDialog(false)}
        onImport={handleImportSuccess}
      />

      {/* New File Dialog */}
      {selectedSkill && (
        <NewFileDialog
          open={showNewFileDialog}
          onClose={() => setShowNewFileDialog(false)}
          onCreate={handleCreateFile}
          skillName={selectedSkill.name}
        />
      )}

      {/* Rename Dialog */}
      {selectedSkill && (
        <RenameDialog
          open={showRenameDialog}
          onOpenChange={setShowRenameDialog}
          itemType="skill"
          currentName={selectedSkill.name}
          onSuccess={handleRenameSuccess}
        />
      )}
    </div>
  );
}
