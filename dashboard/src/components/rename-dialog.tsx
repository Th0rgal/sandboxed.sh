"use client";

import { useState, useCallback, useEffect } from "react";
import { Loader, AlertCircle, FileEdit, ArrowRight, X } from "lucide-react";
import {
  renameLibraryItem,
  LibraryItemType,
  RenameResult,
  RenameChange,
} from "@/lib/api";
import { cn } from "@/lib/utils";

interface RenameDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  itemType: LibraryItemType;
  currentName: string;
  onSuccess?: () => void;
}

const ITEM_TYPE_LABELS: Record<LibraryItemType, string> = {
  skill: "Skill",
  command: "Command",
  rule: "Rule",
  agent: "Agent",
  tool: "Tool",
  "workspace-template": "Workspace Template",
};

function ChangePreview({ changes }: { changes: RenameChange[] }) {
  if (changes.length === 0) return null;

  return (
    <div className="mt-4 space-y-2">
      <label className="text-xs text-white/60">
        Changes to apply ({changes.length}):
      </label>
      <div className="max-h-48 overflow-y-auto rounded-lg border border-white/[0.08] bg-white/[0.02] p-2 text-xs font-mono space-y-1">
        {changes.map((change, i) => (
          <div key={i} className="flex items-center gap-2 text-white/80">
            {change.type === "rename_file" && (
              <>
                <FileEdit className="h-3 w-3 text-blue-400 flex-shrink-0" />
                <span className="text-white/50">{change.from}</span>
                <ArrowRight className="h-3 w-3 flex-shrink-0" />
                <span className="text-white">{change.to}</span>
              </>
            )}
            {change.type === "update_reference" && (
              <>
                <FileEdit className="h-3 w-3 text-amber-400 flex-shrink-0" />
                <span className="text-white/50">{change.file}</span>
                <span className="text-white/40">({change.field})</span>
              </>
            )}
            {change.type === "update_workspace" && (
              <>
                <FileEdit className="h-3 w-3 text-green-400 flex-shrink-0" />
                <span className="text-white">
                  Workspace: {change.workspace_name}
                </span>
                <span className="text-white/40">({change.field})</span>
              </>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

export function RenameDialog({
  open,
  onOpenChange,
  itemType,
  currentName,
  onSuccess,
}: RenameDialogProps) {
  const [newName, setNewName] = useState(currentName);
  const [loading, setLoading] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [preview, setPreview] = useState<RenameResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const label = ITEM_TYPE_LABELS[itemType];

  // Reset state when dialog opens/closes
  useEffect(() => {
    if (open) {
      setNewName(currentName);
      setPreview(null);
      setError(null);
    }
  }, [open, currentName]);

  // Handle escape key
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && open) {
        onOpenChange(false);
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [open, onOpenChange]);

  const handlePreview = useCallback(async () => {
    if (!newName.trim() || newName === currentName) return;

    setPreviewing(true);
    setError(null);
    setPreview(null);

    try {
      const result = await renameLibraryItem(
        itemType,
        currentName,
        newName.trim(),
        true // dry_run
      );
      setPreview(result);
      if (!result.success) {
        setError(result.error || "Preview failed");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to preview rename");
    } finally {
      setPreviewing(false);
    }
  }, [itemType, currentName, newName]);

  const handleRename = useCallback(async () => {
    if (!newName.trim() || newName === currentName) return;

    setLoading(true);
    setError(null);

    try {
      const result = await renameLibraryItem(
        itemType,
        currentName,
        newName.trim(),
        false // execute
      );

      if (result.success) {
        onOpenChange(false);
        onSuccess?.();
      } else {
        setError(result.error || "Rename failed");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to rename");
    } finally {
      setLoading(false);
    }
  }, [itemType, currentName, newName, onOpenChange, onSuccess]);

  const isValid = newName.trim() && newName !== currentName;

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-lg font-medium text-white">Rename {label}</h3>
          <button
            onClick={() => onOpenChange(false)}
            className="p-1 rounded hover:bg-white/[0.06] transition-colors"
          >
            <X className="h-4 w-4 text-white/60" />
          </button>
        </div>

        <p className="text-sm text-white/60 mb-4">
          Enter a new name for this {label.toLowerCase()}. All references will
          be automatically updated.
        </p>

        <div className="space-y-4">
          <div>
            <label className="block text-sm text-white/60 mb-1.5">
              Current name
            </label>
            <input
              type="text"
              value={currentName}
              disabled
              className="w-full px-4 py-2 rounded-lg bg-white/[0.02] border border-white/[0.06] text-white/50"
            />
          </div>

          <div>
            <label className="block text-sm text-white/60 mb-1.5">
              New name
            </label>
            <input
              type="text"
              value={newName}
              onChange={(e) => {
                setNewName(e.target.value);
                setPreview(null);
                setError(null);
              }}
              placeholder={`Enter new ${label.toLowerCase()} name`}
              autoFocus
              onKeyDown={(e) => {
                if (e.key === "Enter" && isValid && !loading && !previewing) {
                  if (preview?.success) {
                    handleRename();
                  } else {
                    handlePreview();
                  }
                }
              }}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
            />
          </div>

          {error && (
            <div className="flex items-start gap-2 p-3 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 text-sm">
              <AlertCircle className="h-4 w-4 mt-0.5 flex-shrink-0" />
              <span>{error}</span>
            </div>
          )}

          {preview?.success && <ChangePreview changes={preview.changes} />}

          {preview?.warnings && preview.warnings.length > 0 && (
            <div className="flex items-start gap-2 p-3 rounded-lg bg-amber-500/10 border border-amber-500/20 text-amber-400 text-sm">
              <AlertCircle className="h-4 w-4 mt-0.5 flex-shrink-0" />
              <span>{preview.warnings.join(", ")}</span>
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 mt-6">
          <button
            onClick={() => onOpenChange(false)}
            className="px-4 py-2 text-sm text-white/60 hover:text-white transition-colors"
          >
            Cancel
          </button>

          {!preview?.success ? (
            <button
              onClick={handlePreview}
              disabled={!isValid || previewing}
              className={cn(
                "flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg transition-colors",
                isValid && !previewing
                  ? "text-white bg-indigo-500 hover:bg-indigo-600"
                  : "text-white/40 bg-white/[0.04] cursor-not-allowed"
              )}
            >
              {previewing ? (
                <>
                  <Loader className="h-4 w-4 animate-spin" />
                  Previewing...
                </>
              ) : (
                "Preview Changes"
              )}
            </button>
          ) : (
            <button
              onClick={handleRename}
              disabled={!isValid || loading}
              className={cn(
                "flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg transition-colors",
                isValid && !loading
                  ? "text-white bg-indigo-500 hover:bg-indigo-600"
                  : "text-white/40 bg-white/[0.04] cursor-not-allowed"
              )}
            >
              {loading ? (
                <>
                  <Loader className="h-4 w-4 animate-spin" />
                  Renaming...
                </>
              ) : (
                "Apply Rename"
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
