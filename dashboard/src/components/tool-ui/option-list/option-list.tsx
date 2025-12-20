"use client";

import {
  useMemo,
  useState,
  useCallback,
  useEffect,
  useRef,
  Fragment,
} from "react";
import type { KeyboardEvent } from "react";
import type {
  OptionListProps,
  OptionListSelection,
  OptionListOption,
} from "./schema";
import { normalizeActionsConfig } from "../shared";
import type { Action } from "../shared";
import { cn, Button, Separator } from "./_adapter";
import { Check } from "lucide-react";

function parseSelectionToIdSet(
  value: OptionListSelection | undefined,
  mode: "multi" | "single",
  maxSelections?: number
): Set<string> {
  if (mode === "single") {
    const single =
      typeof value === "string"
        ? value
        : Array.isArray(value)
        ? value[0]
        : null;
    return single ? new Set([single]) : new Set();
  }

  const arr =
    typeof value === "string" ? [value] : Array.isArray(value) ? value : [];

  return new Set(maxSelections ? arr.slice(0, maxSelections) : arr);
}

function convertIdSetToSelection(
  selected: Set<string>,
  mode: "multi" | "single"
): OptionListSelection {
  if (mode === "single") {
    const [first] = selected;
    return first ?? null;
  }
  return Array.from(selected);
}

function areSetsEqual(a: Set<string>, b: Set<string>) {
  if (a.size !== b.size) return false;
  for (const val of a) {
    if (!b.has(val)) return false;
  }
  return true;
}

interface SelectionIndicatorProps {
  mode: "multi" | "single";
  isSelected: boolean;
  disabled?: boolean;
}

function SelectionIndicator({
  mode,
  isSelected,
  disabled,
}: SelectionIndicatorProps) {
  const shape = mode === "single" ? "rounded-full" : "rounded-sm";

  return (
    <div
      className={cn(
        "flex size-3.5 shrink-0 items-center justify-center border transition-all duration-150",
        shape,
        isSelected && "border-indigo-500 bg-indigo-500 text-white",
        !isSelected && "border-white/20 bg-white/[0.02]",
        disabled && "opacity-40"
      )}
    >
      {mode === "multi" && isSelected && <Check className="size-2.5 stroke-[3]" />}
      {mode === "single" && isSelected && (
        <span className="size-1.5 rounded-full bg-current" />
      )}
    </div>
  );
}

interface OptionItemProps {
  option: OptionListOption;
  isSelected: boolean;
  isDisabled: boolean;
  selectionMode: "multi" | "single";
  isFirst: boolean;
  isLast: boolean;
  onToggle: () => void;
  tabIndex?: number;
  onFocus?: () => void;
  buttonRef?: (el: HTMLButtonElement | null) => void;
}

function OptionItem({
  option,
  isSelected,
  isDisabled,
  selectionMode,
  isFirst,
  isLast,
  onToggle,
  tabIndex,
  onFocus,
  buttonRef,
}: OptionItemProps) {
  return (
    <button
      ref={buttonRef}
      data-id={option.id}
      role="option"
      type="button"
      aria-selected={isSelected}
      onClick={onToggle}
      onFocus={onFocus}
      tabIndex={tabIndex}
      disabled={isDisabled}
      className={cn(
        "peer group relative flex w-full items-start gap-2.5 text-left transition-colors",
        "py-2 px-0.5 -mx-0.5 rounded-lg",
        "hover:bg-white/[0.03] focus-visible:bg-white/[0.03] focus-visible:outline-none",
        isDisabled && "opacity-40 pointer-events-none",
        isFirst && "pt-1",
        isLast && "pb-1"
      )}
    >
      <span className="flex h-5 items-center shrink-0">
        <SelectionIndicator
          mode={selectionMode}
          isSelected={isSelected}
          disabled={option.disabled}
        />
      </span>
      {option.icon && (
        <span className="flex h-5 items-center shrink-0 text-white/50">{option.icon}</span>
      )}
      <div className="flex flex-col gap-0.5 min-w-0">
        <span className={cn(
          "text-[13px] leading-5 text-pretty",
          isSelected ? "text-white" : "text-white/80"
        )}>
          {option.label}
        </span>
        {option.description && (
          <span className="text-[11px] leading-4 text-white/40 text-pretty">
            {option.description}
          </span>
        )}
      </div>
    </button>
  );
}

interface OptionListConfirmationProps {
  id: string;
  options: OptionListOption[];
  selectedIds: Set<string>;
  className?: string;
}

function OptionListConfirmation({
  id,
  options,
  selectedIds,
  className,
}: OptionListConfirmationProps) {
  const confirmedOptions = options.filter((opt) => selectedIds.has(opt.id));

  return (
    <div
      className={cn(
        "@container/option-list flex w-full max-w-sm flex-col",
        "text-foreground",
        className
      )}
      data-slot="option-list"
      data-tool-ui-id={id}
      data-receipt="true"
      role="status"
      aria-label="Confirmed selection"
    >
      <div
        className={cn(
          "bg-white/[0.02] flex w-full flex-col overflow-hidden rounded-xl border border-white/[0.04] px-3 py-2"
        )}
      >
        {confirmedOptions.map((option, index) => (
          <Fragment key={option.id}>
            {index > 0 && <Separator className="my-1.5 opacity-30" orientation="horizontal" />}
            <div className="flex items-start gap-2 py-0.5">
              <span className="flex h-5 items-center shrink-0">
                <Check className="text-emerald-400 size-3.5 stroke-[2.5]" />
              </span>
              {option.icon && (
                <span className="flex h-5 items-center shrink-0 text-white/50">{option.icon}</span>
              )}
              <div className="flex flex-col min-w-0">
                <span className="text-[13px] leading-5 text-white/90 text-pretty">
                  {option.label}
                </span>
                {option.description && (
                  <span className="text-[11px] leading-4 text-white/40 text-pretty">
                    {option.description}
                  </span>
                )}
              </div>
            </div>
          </Fragment>
        ))}
      </div>
    </div>
  );
}

export function OptionList({
  id,
  options,
  selectionMode = "multi",
  minSelections = 1,
  maxSelections,
  value,
  defaultValue,
  confirmed,
  onChange,
  onConfirm,
  onCancel,
  responseActions,
  onResponseAction,
  onBeforeResponseAction,
  className,
}: OptionListProps) {
  if (process.env["NODE_ENV"] !== "production") {
    if (value !== undefined && defaultValue !== undefined) {
      console.warn(
        "[OptionList] Both `value` (controlled) and `defaultValue` (uncontrolled) were provided. `defaultValue` is ignored when `value` is set."
      );
    }
    if (value !== undefined && !onChange) {
      console.warn(
        "[OptionList] `value` was provided without `onChange`. This makes OptionList controlled; selection will not update unless the parent updates `value`."
      );
    }
  }

  const effectiveMaxSelections = selectionMode === "single" ? 1 : maxSelections;

  const [uncontrolledSelected, setUncontrolledSelected] = useState<Set<string>>(
    () =>
      parseSelectionToIdSet(defaultValue, selectionMode, effectiveMaxSelections)
  );

  useEffect(() => {
    setUncontrolledSelected((prev) => {
      const normalized = parseSelectionToIdSet(
        Array.from(prev),
        selectionMode,
        effectiveMaxSelections
      );
      return areSetsEqual(prev, normalized) ? prev : normalized;
    });
  }, [selectionMode, effectiveMaxSelections]);

  const selectedIds = useMemo(
    () =>
      value !== undefined
        ? parseSelectionToIdSet(value, selectionMode, effectiveMaxSelections)
        : uncontrolledSelected,
    [value, uncontrolledSelected, selectionMode, effectiveMaxSelections]
  );

  const selectedCount = selectedIds.size;

  const optionStates = useMemo(() => {
    return options.map((option) => {
      const isSelected = selectedIds.has(option.id);
      const isSelectionLocked =
        selectionMode === "multi" &&
        effectiveMaxSelections !== undefined &&
        selectedCount >= effectiveMaxSelections &&
        !isSelected;
      const isDisabled = option.disabled || isSelectionLocked;

      return { option, isSelected, isDisabled };
    });
  }, [
    options,
    selectedIds,
    selectionMode,
    effectiveMaxSelections,
    selectedCount,
  ]);

  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [activeIndex, setActiveIndex] = useState(() => {
    const firstSelected = optionStates.findIndex(
      (s) => s.isSelected && !s.isDisabled
    );
    if (firstSelected >= 0) return firstSelected;
    const firstEnabled = optionStates.findIndex((s) => !s.isDisabled);
    return firstEnabled >= 0 ? firstEnabled : 0;
  });

  useEffect(() => {
    if (optionStates.length === 0) return;
    setActiveIndex((prev) => {
      if (
        prev < 0 ||
        prev >= optionStates.length ||
        optionStates[prev].isDisabled
      ) {
        const firstEnabled = optionStates.findIndex((s) => !s.isDisabled);
        return firstEnabled >= 0 ? firstEnabled : 0;
      }
      return prev;
    });
  }, [optionStates]);

  const updateSelection = useCallback(
    (next: Set<string>) => {
      const normalizedNext = parseSelectionToIdSet(
        Array.from(next),
        selectionMode,
        effectiveMaxSelections
      );

      if (value === undefined) {
        if (!areSetsEqual(uncontrolledSelected, normalizedNext)) {
          setUncontrolledSelected(normalizedNext);
        }
      }

      onChange?.(convertIdSetToSelection(normalizedNext, selectionMode));
    },
    [
      effectiveMaxSelections,
      selectionMode,
      uncontrolledSelected,
      value,
      onChange,
    ]
  );

  const toggleSelection = useCallback(
    (optionId: string) => {
      const next = new Set(selectedIds);
      const isSelected = next.has(optionId);

      if (selectionMode === "single") {
        if (isSelected) {
          next.delete(optionId);
        } else {
          next.clear();
          next.add(optionId);
        }
      } else {
        if (isSelected) {
          next.delete(optionId);
        } else {
          if (effectiveMaxSelections && next.size >= effectiveMaxSelections) {
            return;
          }
          next.add(optionId);
        }
      }

      updateSelection(next);
    },
    [effectiveMaxSelections, selectedIds, selectionMode, updateSelection]
  );

  const handleConfirm = useCallback(async () => {
    if (!onConfirm) return;
    if (selectedCount === 0 || selectedCount < minSelections) return;
    await onConfirm(convertIdSetToSelection(selectedIds, selectionMode));
  }, [minSelections, onConfirm, selectedCount, selectedIds, selectionMode]);

  const handleCancel = useCallback(() => {
    const empty = new Set<string>();
    updateSelection(empty);
    onCancel?.();
  }, [onCancel, updateSelection]);

  const hasCustomResponseActions = responseActions !== undefined;

  const handleFooterAction = useCallback(
    async (actionId: string) => {
      if (hasCustomResponseActions) {
        await onResponseAction?.(actionId);
        return;
      }
      if (actionId === "confirm") {
        await handleConfirm();
      } else if (actionId === "cancel") {
        handleCancel();
      }
    },
    [handleConfirm, handleCancel, hasCustomResponseActions, onResponseAction]
  );

  const normalizedFooterActions = useMemo(() => {
    const normalized = normalizeActionsConfig(responseActions);
    if (normalized) return normalized;
    return {
      items: [
        { id: "cancel", label: "Clear", variant: "ghost" as const },
        { id: "confirm", label: "Confirm", variant: "default" as const },
      ],
      align: "right" as const,
    } satisfies ReturnType<typeof normalizeActionsConfig>;
  }, [responseActions]);

  const isConfirmDisabled =
    selectedCount < minSelections || selectedCount === 0;
  const hasNothingToClear = selectedCount === 0;

  const focusOptionAt = useCallback((index: number) => {
    const el = optionRefs.current[index];
    if (el) el.focus();
    setActiveIndex(index);
  }, []);

  const findFirstEnabledIndex = useCallback(() => {
    const idx = optionStates.findIndex((s) => !s.isDisabled);
    return idx >= 0 ? idx : 0;
  }, [optionStates]);

  const findLastEnabledIndex = useCallback(() => {
    for (let i = optionStates.length - 1; i >= 0; i--) {
      if (!optionStates[i].isDisabled) return i;
    }
    return 0;
  }, [optionStates]);

  const findNextEnabledIndex = useCallback(
    (start: number, direction: 1 | -1) => {
      const len = optionStates.length;
      if (len === 0) return 0;
      for (let step = 1; step <= len; step++) {
        const idx = (start + direction * step + len) % len;
        if (!optionStates[idx].isDisabled) return idx;
      }
      return start;
    },
    [optionStates]
  );

  const handleListboxKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      if (optionStates.length === 0) return;

      const key = e.key;

      if (key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        focusOptionAt(findNextEnabledIndex(activeIndex, 1));
        return;
      }

      if (key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        focusOptionAt(findNextEnabledIndex(activeIndex, -1));
        return;
      }

      if (key === "Home") {
        e.preventDefault();
        e.stopPropagation();
        focusOptionAt(findFirstEnabledIndex());
        return;
      }

      if (key === "End") {
        e.preventDefault();
        e.stopPropagation();
        focusOptionAt(findLastEnabledIndex());
        return;
      }

      if (key === "Enter" || key === " ") {
        e.preventDefault();
        e.stopPropagation();
        const current = optionStates[activeIndex];
        if (!current || current.isDisabled) return;
        toggleSelection(current.option.id);
        return;
      }

      if (key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (!hasNothingToClear) {
          handleCancel();
        }
      }
    },
    [
      activeIndex,
      findFirstEnabledIndex,
      findLastEnabledIndex,
      findNextEnabledIndex,
      focusOptionAt,
      handleCancel,
      hasNothingToClear,
      optionStates,
      toggleSelection,
    ]
  );

  const actionsWithDisabledState = useMemo((): Action[] => {
    return normalizedFooterActions.items.map((action) => {
      const isDisabledByValidation =
        (action.id === "confirm" && isConfirmDisabled) ||
        (action.id === "cancel" && hasNothingToClear);
      return {
        ...action,
        disabled: action.disabled || isDisabledByValidation,
        label:
          action.id === "confirm" &&
          selectionMode === "multi" &&
          selectedCount > 0
            ? `${action.label} (${selectedCount})`
            : action.label,
      };
    });
  }, [
    normalizedFooterActions.items,
    isConfirmDisabled,
    hasNothingToClear,
    selectionMode,
    selectedCount,
  ]);

  if (confirmed !== undefined && confirmed !== null) {
    const selectedIds = parseSelectionToIdSet(confirmed, selectionMode);
    return (
      <OptionListConfirmation
        id={id}
        options={options}
        selectedIds={selectedIds}
        className={className}
      />
    );
  }

  return (
    <div
      className={cn(
        "@container/option-list flex w-full max-w-sm flex-col gap-2",
        "text-foreground",
        className
      )}
      data-slot="option-list"
      data-tool-ui-id={id}
      role="group"
      aria-label="Option list"
    >
      <div
        className={cn(
          "group/list bg-white/[0.02] backdrop-blur-sm flex w-full flex-col overflow-hidden rounded-xl border border-white/[0.04] px-3 py-1"
        )}
        role="listbox"
        aria-multiselectable={selectionMode === "multi"}
        onKeyDown={handleListboxKeyDown}
      >
        {optionStates.map(({ option, isSelected, isDisabled }, index) => {
          return (
            <Fragment key={option.id}>
              {index > 0 && (
                <Separator
                  className="opacity-30 [@media(hover:hover)]:[&:has(+_:hover)]:opacity-0 [@media(hover:hover)]:[.peer:hover+&]:opacity-0"
                  orientation="horizontal"
                />
              )}
              <OptionItem
                option={option}
                isSelected={isSelected}
                isDisabled={isDisabled}
                selectionMode={selectionMode}
                isFirst={index === 0}
                isLast={index === optionStates.length - 1}
                tabIndex={index === activeIndex ? 0 : -1}
                onFocus={() => setActiveIndex(index)}
                buttonRef={(el) => {
                  optionRefs.current[index] = el;
                }}
                onToggle={() => toggleSelection(option.id)}
              />
            </Fragment>
          );
        })}
      </div>

      <div className="flex items-center justify-end gap-2 pt-0.5">
        {actionsWithDisabledState.map((action) => (
          <Button
            key={action.id}
            variant={action.variant === "default" ? "default" : "ghost"}
            size="sm"
            onClick={() => handleFooterAction(action.id)}
            disabled={action.disabled}
            className={cn(
              "h-7 px-3 text-xs font-medium rounded-lg",
              action.variant === "default" 
                ? "bg-indigo-500 hover:bg-indigo-600 text-white" 
                : "text-white/50 hover:text-white/70 hover:bg-white/[0.04]"
            )}
          >
            {action.label}
          </Button>
        ))}
      </div>
    </div>
  );
}
