import { getErrorMessage } from '@broccoli/web-sdk/api';
/**
 * Visual subtask definition editor for IOI-style contest problems.
 */
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { TestCaseSummary } from '@broccoli/web-sdk/problem';
import type { ConfigFieldSlotProps } from '@broccoli/web-sdk/slot';
import { InheritedBadge } from '@broccoli/web-sdk/slot';

type TestCaseListItem = TestCaseSummary;
import {
  Button,
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import type React from 'react';
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { createPortal } from 'react-dom';

import { useTestCases } from './hooks/useTestCases';

/**
 * Find a portal target inside the Radix Dialog tree.
 * Elements portaled to document.body become inert (Radix Dialog marks siblings
 * as non-interactive). By portaling into the Radix portal container instead,
 * our overlays stay interactive. Falls back to document.body if no dialog found.
 */
function findPortalTarget(el: HTMLElement): HTMLElement {
  const radixPortal = el.closest('[data-radix-portal]') as HTMLElement | null;
  if (radixPortal) return radixPortal;
  const dialog = el.closest('[role="dialog"]') as HTMLElement | null;
  if (dialog?.parentElement) return dialog.parentElement;
  return document.body;
}

/**
 * Compute the offset needed to convert viewport-relative coordinates to
 * coordinates relative to a portal target that may have a CSS transform.
 */
function getPortalOffset(portalTarget: HTMLElement): {
  dx: number;
  dy: number;
} {
  if (portalTarget === document.body) return { dx: 0, dy: 0 };
  // Probe with a fixed element to measure any CSS transform offset on the portal target.
  const probe = document.createElement('div');
  probe.style.cssText =
    'position:fixed;top:0;left:0;width:0;height:0;pointer-events:none';
  portalTarget.appendChild(probe);
  const probeRect = probe.getBoundingClientRect();
  portalTarget.removeChild(probe);
  return { dx: probeRect.left, dy: probeRect.top };
}

interface SubtaskValue {
  name: string;
  scoring_method: string;
  max_score: number;
  test_cases: string[];
}

interface EditorStatusMessage {
  tone: 'success' | 'error';
  text: string;
}

/** Canonical string label for a test case: prefer label, fall back to stringified id. */
function tcLabel(tc: TestCaseSummary): string {
  return tc.label || String(tc.id);
}

const SCORING_METHODS = [
  {
    key: 'group_min',
    labelKey: 'ioi.subtask.method.groupMin.label',
    shortKey: 'ioi.subtask.method.groupMin.short',
    hintKey: 'ioi.subtask.method.groupMin.hint',
    color: '#ef4444',
    bg: 'rgba(239,68,68,0.08)',
  },
  {
    key: 'sum',
    labelKey: 'ioi.subtask.method.sum.label',
    shortKey: 'ioi.subtask.method.sum.short',
    hintKey: 'ioi.subtask.method.sum.hint',
    color: '#10b981',
    bg: 'rgba(16,185,129,0.08)',
  },
  {
    key: 'group_mul',
    labelKey: 'ioi.subtask.method.groupMul.label',
    shortKey: 'ioi.subtask.method.groupMul.short',
    hintKey: 'ioi.subtask.method.groupMul.hint',
    color: '#f59e0b',
    bg: 'rgba(245,158,11,0.08)',
  },
] as const;

const SCORING_METHOD_KEYS: Set<string> = new Set(
  SCORING_METHODS.map((method) => method.key),
);

function getMethodInfo(key: string) {
  return SCORING_METHODS.find((m) => m.key === key) ?? SCORING_METHODS[0];
}

function defaultSubtask(
  index: number,
  t: (key: string, params?: Record<string, string | number>) => string,
): SubtaskValue {
  return {
    name: t('ioi.subtask.defaultName', { index: index + 1 }),
    scoring_method: 'group_min',
    max_score: 100,
    test_cases: [],
  };
}

function normalizeImportedSubtasks(value: unknown): SubtaskValue[] | null {
  const rawSubtasks = Array.isArray(value)
    ? value
    : value &&
        typeof value === 'object' &&
        !Array.isArray(value) &&
        Array.isArray((value as { subtasks?: unknown }).subtasks)
      ? (value as { subtasks: unknown[] }).subtasks
      : null;

  if (!rawSubtasks) return null;

  const normalized: SubtaskValue[] = [];
  for (const item of rawSubtasks) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) return null;
    const raw = item as Record<string, unknown>;
    const scoringMethod =
      raw.scoring_method ?? raw.scoringMethod ?? 'group_min';
    if (
      typeof scoringMethod !== 'string' ||
      !SCORING_METHOD_KEYS.has(scoringMethod)
    ) {
      return null;
    }

    const maxScoreRaw = raw.max_score ?? raw.maxScore ?? 100;
    const maxScore =
      typeof maxScoreRaw === 'number'
        ? maxScoreRaw
        : typeof maxScoreRaw === 'string'
          ? maxScoreRaw.trim()
            ? Number(maxScoreRaw)
            : NaN
          : NaN;
    if (!Number.isFinite(maxScore)) return null;

    const rawTestCases = raw.test_cases ?? raw.testCases ?? [];
    if (!Array.isArray(rawTestCases)) return null;
    const testCases = rawTestCases.map((entry) => {
      if (typeof entry === 'string' || typeof entry === 'number') {
        return String(entry);
      }
      return null;
    });
    if (testCases.some((entry) => entry === null)) return null;

    const name = raw.name;
    if (name !== undefined && typeof name !== 'string') return null;

    normalized.push({
      name: typeof name === 'string' ? name : '',
      scoring_method: scoringMethod,
      max_score: maxScore,
      test_cases: testCases as string[],
    });
  }

  return normalized;
}

const MAX_RANGE_SPAN = 10000;

/** Parse "1-5, 8, 10-12" into a sorted array of unique positions. */
function parseRanges(input: string): number[] {
  const result = new Set<number>();
  for (const part of input.split(/[,;\s]+/)) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const rangeMatch = trimmed.match(/^(\d+)\s*-\s*(\d+)$/);
    if (rangeMatch) {
      const lo = parseInt(rangeMatch[1]);
      const hi = parseInt(rangeMatch[2]);
      if (!isNaN(lo) && !isNaN(hi) && Math.abs(hi - lo) <= MAX_RANGE_SPAN) {
        for (let i = Math.min(lo, hi); i <= Math.max(lo, hi); i++)
          result.add(i);
      }
    } else {
      const n = parseInt(trimmed);
      if (!isNaN(n)) result.add(n);
    }
  }
  return [...result].sort((a, b) => a - b);
}

/** Monospace font class shorthand. */
const monoClass = 'font-mono tabular-nums';

/** Glyph on the buttons that remove a test case or subtask. */
const REMOVE_GLYPH = '\u2715';

/** Shared preview content for test case hover cards. */
function PreviewContent({ tc }: { tc: TestCaseListItem }) {
  const { t } = useTranslation();
  return (
    <>
      <div className="flex items-center gap-2 mb-2">
        <span className={cn(monoClass, 'text-xs font-bold opacity-60')}>
          #{tc.position + 1}
        </span>
        {tc.label && (
          <span className={cn(monoClass, 'text-[11px] opacity-50')}>
            {tc.label}
          </span>
        )}
        {tc.is_sample && (
          <span className="text-[9px] font-bold uppercase tracking-wide px-1.5 py-px rounded bg-indigo-500/10 text-indigo-500">
            {t('ioi.subtask.preview.sample')}
          </span>
        )}
        <span className={cn(monoClass, 'text-[11px] opacity-50 ml-auto')}>
          {t('ioi.subtask.points', { score: tc.score })}
        </span>
      </div>
      {tc.description && (
        <div className="text-[11px] opacity-60 mb-2 leading-snug">
          {tc.description.length > 200
            ? tc.description.slice(0, 200) + '\u2026'
            : tc.description}
        </div>
      )}
      <div className="grid grid-cols-2 gap-2">
        <div>
          <div className="text-[9px] font-bold uppercase tracking-widest opacity-40 block mb-0.5">
            {t('ioi.subtask.preview.input')}
          </div>
          <pre
            className={cn(
              monoClass,
              'text-[10px] leading-snug m-0 p-1.5 rounded max-h-20 overflow-auto overscroll-contain bg-muted whitespace-pre-wrap break-all',
            )}
          >
            {tc.input_preview || t('ioi.subtask.preview.empty')}
          </pre>
        </div>
        <div>
          <div className="text-[9px] font-bold uppercase tracking-widest opacity-40 block mb-0.5">
            {t('ioi.subtask.preview.output')}
          </div>
          <pre
            className={cn(
              monoClass,
              'text-[10px] leading-snug m-0 p-1.5 rounded max-h-20 overflow-auto overscroll-contain bg-muted whitespace-pre-wrap break-all',
            )}
          >
            {tc.output_preview || t('ioi.subtask.preview.empty')}
          </pre>
        </div>
      </div>
    </>
  );
}

function TcChip({
  tc,
  onRemove,
  onDragStart,
  isDuplicate,
}: {
  tc: TestCaseListItem;
  onRemove: () => void;
  onDragStart?: (e: React.DragEvent) => void;
  isDuplicate?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <HoverCard openDelay={0} closeDelay={300}>
      <HoverCardTrigger asChild>
        <span
          draggable={!!onDragStart}
          onDragStart={onDragStart}
          className={cn(
            monoClass,
            'inline-flex items-center gap-1 px-2 py-[3px] rounded-md text-[11px] select-none transition-[border-color,box-shadow,background] duration-150',
            isDuplicate
              ? 'border-[1.5px] border-dashed border-red-500/50 bg-red-500/[0.06]'
              : 'border border-border bg-card hover:border-primary hover:bg-accent',
            onDragStart ? 'cursor-grab' : 'cursor-default',
            isDuplicate
              ? 'hover:shadow-[0_0_0_1px_rgba(239,68,68,0.4)]'
              : 'shadow-[0_1px_0_0_rgba(0,0,0,0.06)] hover:shadow-[0_0_0_1px_hsl(var(--primary))]',
          )}
        >
          {tc.label ? (
            <>
              <span className="text-[11px]">{tc.label}</span>
              <span className="opacity-35 text-[9px]">#{tc.position + 1}</span>
            </>
          ) : (
            <>
              <span className="opacity-50 text-[9px]">#</span>
              {tc.position + 1}
            </>
          )}
          {tc.is_sample && (
            <span className="size-1 rounded-full bg-indigo-500 shrink-0" />
          )}
          {isDuplicate && (
            <span className="size-1.5 rounded-full shrink-0 bg-red-500 shadow-[0_0_0_2px_rgba(239,68,68,0.2)]" />
          )}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onRemove();
            }}
            aria-label={t('ioi.subtask.removeTestCase', { label: tcLabel(tc) })}
            className="bg-transparent border-none cursor-pointer text-[10px] opacity-30 px-px py-0 text-inherit leading-none transition-opacity duration-150 hover:opacity-80"
          >
            {REMOVE_GLYPH}
          </button>
        </span>
      </HoverCardTrigger>
      <HoverCardContent side="bottom" align="center" className="w-80 p-3">
        <PreviewContent tc={tc} />
      </HoverCardContent>
    </HoverCard>
  );
}

function PoolCard({
  tc,
  state,
  ownerLabel,
  onClick,
  onDragStart,
  isDuplicate,
}: {
  tc: TestCaseListItem;
  state: 'unassigned' | 'this' | 'other';
  ownerLabel?: string;
  onClick: (e: React.MouseEvent) => void;
  onDragStart: (e: React.DragEvent) => void;
  isDuplicate?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <HoverCard openDelay={0} closeDelay={300}>
      <HoverCardTrigger asChild>
        <div
          draggable
          onDragStart={onDragStart}
          onClick={onClick}
          className={cn(
            'px-2.5 py-2 rounded-lg border-[1.5px] transition-all duration-150 select-none relative min-w-0',
            isDuplicate
              ? 'border-red-500/50 bg-red-500/[0.04]'
              : state === 'this'
                ? 'border-emerald-500/50 bg-emerald-500/[0.04]'
                : state === 'other'
                  ? 'border-border bg-muted'
                  : 'border-border bg-card',
            state === 'other'
              ? 'cursor-not-allowed opacity-45'
              : 'cursor-pointer opacity-100',
          )}
        >
          <div className="flex items-center gap-1.5">
            {tc.label ? (
              <>
                <span
                  className={cn(
                    monoClass,
                    'text-[11px] font-bold opacity-80 overflow-hidden text-ellipsis whitespace-nowrap',
                  )}
                >
                  {tc.label}
                </span>
                <span className={cn(monoClass, 'text-[9px] opacity-35')}>
                  #{tc.position + 1}
                </span>
              </>
            ) : (
              <span className={cn(monoClass, 'text-xs font-bold opacity-70')}>
                {tc.position + 1}
              </span>
            )}
            {tc.is_sample && (
              <span className="text-[8px] font-bold uppercase tracking-tight px-[5px] py-px rounded-[3px] bg-indigo-500/10 text-indigo-500">
                {t('ioi.subtask.sampleAbbr')}
              </span>
            )}
            <span className={cn(monoClass, 'text-[10px] opacity-40 ml-auto')}>
              {tc.score}
            </span>
          </div>
          {tc.description && (
            <div className="text-[10px] opacity-50 mt-[3px] whitespace-nowrap overflow-hidden text-ellipsis">
              {tc.description}
            </div>
          )}
          {state === 'this' && (
            <div className="absolute top-[3px] right-[3px] size-1.5 rounded-full bg-emerald-500" />
          )}
          {state === 'other' && ownerLabel && (
            <div className="text-[9px] opacity-60 mt-[3px] italic">
              {'\u2192'} {ownerLabel}
            </div>
          )}
        </div>
      </HoverCardTrigger>
      <HoverCardContent side="bottom" align="center" className="w-80 p-3">
        <PreviewContent tc={tc} />
      </HoverCardContent>
    </HoverCard>
  );
}

// Position mode requires explicit # prefix to avoid confusion with numeric labels.
// Examples: #1-10, #15, #1-5,8,10-12
const POS_RANGE_RE = /^#[\d\s,;-]+$/;

function UnifiedSearch({
  testCases,
  subtask,
  subtaskIdx,
  updateSubtask,
  tcByPosition,
  assignmentMap,
  duplicateSet,
  subtaskNames,
  portalTarget,
  portalOffset,
}: {
  testCases: TestCaseListItem[];
  subtask: SubtaskValue;
  subtaskIdx: number;
  updateSubtask: (index: number, patch: Partial<SubtaskValue>) => void;
  tcByPosition: Map<number, TestCaseListItem>;
  assignmentMap: Map<string, number[]>;
  duplicateSet: Set<string>;
  subtaskNames: string[];
  portalTarget: HTMLElement;
  portalOffset: { dx: number; dy: number };
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState('');
  const [showDropdown, setShowDropdown] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const [inputRect, setInputRect] = useState<{
    top: number;
    bottom: number;
    left: number;
    width: number;
  } | null>(null);
  const blurTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listboxRef = useRef<HTMLDivElement>(null);
  const listboxId = `unified-search-${subtaskIdx}`;

  const cancelBlur = useCallback(() => {
    if (blurTimeoutRef.current) {
      clearTimeout(blurTimeoutRef.current);
      blurTimeoutRef.current = null;
    }
  }, []);

  useEffect(() => () => cancelBlur(), [cancelBlur]);

  // Reposition dropdown on scroll/resize
  const measureInput = useCallback(() => {
    if (!inputRef.current) return;
    const r = inputRef.current.getBoundingClientRect();
    if (r.bottom < 0 || r.top > window.innerHeight) {
      setShowDropdown(false);
      return;
    }
    setInputRect({
      top: r.top,
      bottom: r.bottom,
      left: r.left,
      width: r.width,
    });
  }, []);

  useEffect(() => {
    if (!showDropdown) return;
    measureInput();
    window.addEventListener('scroll', measureInput, true);
    window.addEventListener('resize', measureInput);
    return () => {
      window.removeEventListener('scroll', measureInput, true);
      window.removeEventListener('resize', measureInput);
    };
  }, [showDropdown, measureInput]);

  const q = query.trim();
  const isPositionMode = q.length > 0 && POS_RANGE_RE.test(q);

  const posResults = useMemo(() => {
    if (!isPositionMode || !q) return null;
    const rangeStr = q.slice(1);
    const positions = parseRanges(rangeStr);
    const resolved: TestCaseListItem[] = [];
    const invalid: number[] = [];
    for (const pos of positions) {
      const tc = tcByPosition.get(pos);
      if (tc) resolved.push(tc);
      else invalid.push(pos);
    }
    return { resolved, invalid };
  }, [isPositionMode, q, tcByPosition]);

  const labelMatches = useMemo(() => {
    if (isPositionMode || !q) return [];
    let re: RegExp | null = null;
    try {
      re = new RegExp(q, 'i');
    } catch {
      /* invalid regex */
    }
    return testCases.filter((tc) => {
      const l = tcLabel(tc);
      return re ? re.test(l) : l.toLowerCase().includes(q.toLowerCase());
    });
  }, [q, testCases, isPositionMode]);

  // Build dropdown items - only show addable (non-added, non-duplicate) items
  const items = useMemo(() => {
    const result: Array<{
      type: 'batch' | 'item';
      tc?: TestCaseListItem;
      labels?: string[];
      count?: number;
    }> = [];
    const source = isPositionMode ? (posResults?.resolved ?? []) : labelMatches;
    // Filter to only addable items: not already in this subtask
    const addable = source.filter(
      (tc) => !subtask.test_cases.includes(tcLabel(tc)),
    );
    if (addable.length > 1) {
      result.push({
        type: 'batch',
        labels: addable.map((tc) => tcLabel(tc)),
        count: addable.length,
      });
    }
    for (const tc of addable.slice(0, 50)) {
      result.push({ type: 'item', tc });
    }
    return { items: result, addableTotal: addable.length };
  }, [isPositionMode, posResults, labelMatches, subtask.test_cases]);

  const addLabels = useCallback(
    (labels: string[]) => {
      const existing = new Set(subtask.test_cases);
      const toAdd = labels.filter((l) => !existing.has(l));
      if (toAdd.length > 0)
        updateSubtask(subtaskIdx, {
          test_cases: [...subtask.test_cases, ...toAdd],
        });
      setQuery('');
      setShowDropdown(false);
      setActiveIndex(-1);
    },
    [subtask, subtaskIdx, updateSubtask],
  );

  const scrollActiveIntoView = useCallback(
    (index: number) => {
      const el = listboxRef.current?.querySelector(
        `[id="${listboxId}-opt-${index}"]`,
      ) as HTMLElement | null;
      if (el) el.scrollIntoView({ block: 'nearest' });
    },
    [listboxId],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const hasItems = items.items.length > 0;
      const isOpen = showDropdown && q.length > 0 && hasItems;

      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (!isOpen) {
          if (q) setShowDropdown(true);
          return;
        }
        setActiveIndex((prev) => {
          const n = prev < items.items.length - 1 ? prev + 1 : 0;
          requestAnimationFrame(() => scrollActiveIntoView(n));
          return n;
        });
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (!isOpen) return;
        setActiveIndex((prev) => {
          const n = prev > 0 ? prev - 1 : items.items.length - 1;
          requestAnimationFrame(() => scrollActiveIntoView(n));
          return n;
        });
      } else if (e.key === 'Enter') {
        e.preventDefault();
        if (isOpen && activeIndex >= 0 && activeIndex < items.items.length) {
          const item = items.items[activeIndex];
          if (item.type === 'batch') addLabels(item.labels!);
          else if (item.tc) addLabels([tcLabel(item.tc)]);
        } else if (isPositionMode && posResults) {
          const labels = posResults.resolved.map((tc) => tcLabel(tc));
          addLabels(labels);
        } else if (!isPositionMode && labelMatches.length === 1) {
          addLabels([tcLabel(labelMatches[0])]);
        } else if (!isPositionMode && labelMatches.length > 1) {
          const exact = labelMatches.find(
            (tc) => tcLabel(tc).toLowerCase() === q.toLowerCase(),
          );
          if (exact) addLabels([tcLabel(exact)]);
        }
      } else if (e.key === 'Escape') {
        setShowDropdown(false);
        setActiveIndex(-1);
        inputRef.current?.blur();
      } else if (e.key === 'Tab') {
        setShowDropdown(false);
        setActiveIndex(-1);
      }
    },
    [
      showDropdown,
      q,
      items,
      activeIndex,
      isPositionMode,
      posResults,
      labelMatches,
      addLabels,
      scrollActiveIntoView,
    ],
  );

  const isOpen = showDropdown && q.length > 0;
  const hasResults = items.items.length > 0;
  const activeDescendant =
    isOpen && hasResults && activeIndex >= 0
      ? `${listboxId}-opt-${activeIndex}`
      : undefined;

  // Dropdown positioning. Portal into dialog tree so Radix doesn't block events
  const MAX_DD = 280;
  const spaceBelow = inputRect ? window.innerHeight - inputRect.bottom : MAX_DD;
  const spaceAbove = inputRect?.top ?? MAX_DD;
  const flipAbove = spaceBelow < 150 && spaceAbove > spaceBelow;
  const ddTop = flipAbove
    ? undefined
    : (inputRect?.bottom ?? 0) - portalOffset.dy + 2;
  const ddBottom = flipAbove
    ? window.innerHeight - (inputRect?.top ?? 0) - portalOffset.dy + 2
    : undefined;
  const ddMaxH = Math.min(MAX_DD, (flipAbove ? spaceAbove : spaceBelow) - 8);

  const dropdownPositionStyle: React.CSSProperties = {
    top: ddTop,
    bottom: ddBottom,
    left: inputRect ? inputRect.left - portalOffset.dx : undefined,
    width: inputRect?.width,
    maxHeight: ddMaxH,
  };

  const renderDropdown = () => {
    if (!isOpen || !inputRect) return null;

    const ddClass =
      'fixed z-[100000] bg-popover text-popover-foreground border border-border rounded-lg shadow-[0_8px_24px_rgba(0,0,0,0.15),0_2px_6px_rgba(0,0,0,0.08)] overflow-y-auto overscroll-contain box-border';

    if (!hasResults) {
      const noMatchMsg = isPositionMode
        ? t('ioi.subtask.noMatchPosition')
        : t('ioi.subtask.noMatchLabel');
      return createPortal(
        <div
          className={cn(ddClass, 'px-2.5 py-2')}
          style={dropdownPositionStyle}
          onMouseDown={(e) => {
            e.preventDefault();
            cancelBlur();
          }}
        >
          <span className="text-[11px] opacity-50 italic">{noMatchMsg}</span>
        </div>,
        portalTarget,
      );
    }

    return createPortal(
      <div
        ref={listboxRef}
        id={listboxId}
        role="listbox"
        aria-label={t('ioi.subtask.searchResults')}
        className={ddClass}
        style={dropdownPositionStyle}
        onMouseDown={(e) => {
          e.preventDefault();
          cancelBlur();
        }}
      >
        {/* Mode badge */}
        <div className="px-2.5 py-1 text-[9px] font-bold uppercase tracking-wide opacity-40 border-b border-border">
          {isPositionMode
            ? t('ioi.subtask.positions')
            : t('ioi.subtask.labels')}
          {isPositionMode && posResults?.invalid.length ? (
            <span className="text-red-600 ml-2 font-semibold opacity-100">
              {posResults.invalid.length > 3
                ? `${posResults.invalid.length} ${t('ioi.subtask.notFound')}`
                : `#${posResults.invalid.join(', #')} ${t('ioi.subtask.notFound')}`}
            </span>
          ) : null}
        </div>

        {items.items.map((item, idx) => {
          const isActive = activeIndex === idx;
          if (item.type === 'batch') {
            return (
              <div
                key="batch"
                id={`${listboxId}-opt-${idx}`}
                role="option"
                aria-selected={isActive}
                onClick={() => addLabels(item.labels!)}
                onMouseEnter={() => setActiveIndex(idx)}
                className={cn(
                  'px-2.5 py-1.5 cursor-pointer text-[10px] font-bold text-indigo-600 uppercase tracking-tight border-b border-border box-border',
                  isActive ? 'bg-indigo-600/[0.12]' : 'bg-indigo-600/[0.04]',
                )}
              >
                {t('ioi.subtask.addCount', {
                  count: item.count ?? 0,
                  mode: isPositionMode
                    ? t('ioi.subtask.positions').toLowerCase()
                    : 'matches',
                })}
              </div>
            );
          }
          const tc = item.tc!;
          const label = tcLabel(tc);
          const owners = assignmentMap.get(label) ?? [];
          const otherOwner =
            owners.length > 0 && !owners.includes(subtaskIdx)
              ? owners[0]
              : null;
          const isDup = duplicateSet.has(label);
          return (
            <div
              key={tc.id}
              id={`${listboxId}-opt-${idx}`}
              role="option"
              aria-selected={isActive}
              onClick={() => addLabels([label])}
              onMouseEnter={() => setActiveIndex(idx)}
              className={cn(
                'px-2.5 py-[5px] text-[11px] flex items-center gap-2 border-b border-border cursor-pointer box-border',
                isActive ? 'bg-indigo-600/[0.08]' : 'bg-transparent',
              )}
            >
              <span className={cn(monoClass, 'font-semibold')}>
                {tc.label || `#${tc.position + 1}`}
              </span>
              <span className={cn(monoClass, 'text-[9px] opacity-40')}>
                {tc.label ? `#${tc.position + 1}` : ''}
              </span>
              {tc.is_sample && (
                <span className="text-[8px] font-bold uppercase px-1 rounded-[3px] bg-indigo-500/10 text-indigo-500">
                  {t('ioi.subtask.sampleAbbr')}
                </span>
              )}
              <span className={cn(monoClass, 'text-[9px] opacity-35 ml-auto')}>
                {t('ioi.subtask.points', { score: tc.score })}
              </span>
              {otherOwner !== null && (
                <span className="text-[9px] italic opacity-60">
                  {'\u2192'}{' '}
                  {subtaskNames[otherOwner] ||
                    t('ioi.subtask.defaultName', { index: otherOwner + 1 })}
                </span>
              )}
              {isDup && (
                <span className="size-1.5 rounded-full shrink-0 bg-red-500 shadow-[0_0_0_2px_rgba(239,68,68,0.2)]" />
              )}
            </div>
          );
        })}

        {items.addableTotal > 50 && (
          <div className="px-2.5 py-[5px] text-[10px] opacity-40 text-center">
            {t('ioi.subtask.andMore', { count: items.addableTotal - 50 })}
          </div>
        )}
      </div>,
      portalTarget,
    );
  };

  return (
    <div>
      <Input
        ref={inputRef}
        type="text"
        role="combobox"
        aria-expanded={isOpen && hasResults}
        aria-controls={isOpen && hasResults ? listboxId : undefined}
        aria-activedescendant={activeDescendant}
        aria-autocomplete="list"
        aria-haspopup="listbox"
        placeholder={t('ioi.subtask.searchPlaceholder')}
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          setShowDropdown(true);
          setActiveIndex(-1);
          measureInput();
        }}
        onFocus={() => {
          cancelBlur();
          measureInput();
          if (q) setShowDropdown(true);
        }}
        onBlur={() => {
          cancelBlur();
          blurTimeoutRef.current = setTimeout(() => {
            setShowDropdown(false);
            setActiveIndex(-1);
          }, 200);
        }}
        onKeyDown={handleKeyDown}
        className={cn(monoClass, 'w-full text-xs bg-card')}
      />
      {renderDropdown()}
    </div>
  );
}

export function SubtaskEditor({
  value,
  schema,
  onChange,
  scope,
  showAsPlaceholder,
  inheritedSource,
}: ConfigFieldSlotProps) {
  const { t } = useTranslation();
  const subtasks: SubtaskValue[] = Array.isArray(value)
    ? (value as Record<string, unknown>[]).map((raw) => ({
        name: (raw.name as string) ?? '',
        scoring_method: (raw.scoring_method as string) ?? 'group_min',
        max_score: (raw.max_score as number) ?? 100,
        test_cases: Array.isArray(raw.test_cases)
          ? (raw.test_cases as string[])
          : [],
      }))
    : [];

  // Extract problemId from scope
  const problemId =
    scope && 'problemId' in scope ? (scope.problemId as number) : undefined;
  const {
    testCases,
    loading: tcLoading,
    error: tcError,
    refetch,
  } = useTestCases(problemId);

  // Build lookup maps
  const tcByLabel = useMemo(() => {
    const map = new Map<string, TestCaseListItem>();
    for (const tc of testCases) map.set(tcLabel(tc), tc);
    return map;
  }, [testCases]);

  const tcByPosition = useMemo(() => {
    const map = new Map<number, TestCaseListItem>();
    for (const tc of testCases) map.set(tc.position + 1, tc); // 1-indexed for user display
    return map;
  }, [testCases]);

  // Assignment map: label -> all subtask indices that contain it
  const assignmentMap = useMemo(() => {
    const map = new Map<string, number[]>();
    subtasks.forEach((s, i) => {
      for (const label of s.test_cases) {
        const arr = map.get(label);
        if (arr) arr.push(i);
        else map.set(label, [i]);
      }
    });
    return map;
  }, [subtasks]);

  // Validation
  const unassignedLabels = useMemo(
    () =>
      testCases
        .filter((tc) => !assignmentMap.has(tcLabel(tc)))
        .map((tc) => tcLabel(tc)),
    [testCases, assignmentMap],
  );

  const duplicateLabels = useMemo(() => {
    const seen = new Map<string, number[]>();
    subtasks.forEach((s, i) => {
      for (const label of s.test_cases) {
        if (!seen.has(label)) seen.set(label, []);
        seen.get(label)!.push(i);
      }
    });
    return [...seen.entries()].filter(([, indices]) => indices.length > 1);
  }, [subtasks]);

  // O(1) lookup for duplicate labels
  const duplicateSet = useMemo(() => {
    const set = new Set<string>();
    for (const [label] of duplicateLabels) set.add(label);
    return set;
  }, [duplicateLabels]);

  // Subtask names for "-> SubtaskName" labels in dropdown
  const subtaskNames = useMemo(() => subtasks.map((s) => s.name), [subtasks]);

  const duplicateNameIndices = useMemo(() => {
    const nameCount = new Map<string, number[]>();
    subtasks.forEach((s, i) => {
      if (!s.name) return;
      const key = s.name.trim().toLowerCase();
      if (!key) return;
      if (!nameCount.has(key)) nameCount.set(key, []);
      nameCount.get(key)!.push(i);
    });
    const dupes = new Set<number>();
    for (const [, indices] of nameCount) {
      if (indices.length > 1) indices.forEach((i) => dupes.add(i));
    }
    return dupes;
  }, [subtasks]);

  // Stable keys for subtask cards (monotonic counter)
  const nextKeyRef = useRef(subtasks.length);
  const [subtaskKeys, setSubtaskKeys] = useState<number[]>(() =>
    subtasks.map((_, i) => i),
  );
  // Keep keys in sync when subtasks grow or shrink externally
  useEffect(() => {
    setSubtaskKeys((prev) => {
      if (prev.length === subtasks.length) return prev;
      if (prev.length < subtasks.length) {
        const newKeys = [...prev];
        while (newKeys.length < subtasks.length)
          newKeys.push(nextKeyRef.current++);
        return newKeys;
      }
      // External shrinkage
      return prev.slice(0, subtasks.length);
    });
  }, [subtasks.length]);

  // Portal target: find the Radix Dialog portal container so our overlays
  // stay inside the dialog's interactive zone (not marked inert by Radix).
  const rootRef = useRef<HTMLDivElement>(null);
  const [portalInfo, setPortalInfo] = useState<{
    target: HTMLElement;
    offset: { dx: number; dy: number };
  }>({
    target: document.body,
    offset: { dx: 0, dy: 0 },
  });
  useLayoutEffect(() => {
    if (!rootRef.current) return;
    const target = findPortalTarget(rootRef.current);
    const offset = getPortalOffset(target);
    setPortalInfo({ target, offset });
  }, []);

  // State
  const [openPools, setOpenPools] = useState<Record<number, boolean>>({});
  const [confirm, confirmDialog] = useConfirmDialog();
  const [warningsExpanded, setWarningsExpanded] = useState(true);
  const [lastClicked, setLastClicked] = useState<string | null>(null);
  const [statusMessage, setStatusMessage] =
    useState<EditorStatusMessage | null>(null);
  const [pendingImport, setPendingImport] = useState<SubtaskValue[] | null>(
    null,
  );
  const dragDataRef = useRef<{
    tcLabels: string[];
    fromSubtask: number | null;
  } | null>(null);
  const [dropTarget, setDropTarget] = useState<number | null>(null);
  const dragCountersRef = useRef<Record<number, number>>({});

  useEffect(() => {
    if (!statusMessage) return;
    const id = window.setTimeout(() => {
      setStatusMessage((current) =>
        current?.text === statusMessage.text ? null : current,
      );
    }, 2400);
    return () => window.clearTimeout(id);
  }, [statusMessage]);

  // Mutations
  const updateSubtask = useCallback(
    (index: number, patch: Partial<SubtaskValue>) => {
      const updated = subtasks.map((s, i) =>
        i === index ? { ...s, ...patch } : s,
      );
      onChange(updated);
    },
    [subtasks, onChange],
  );

  const resetInteractiveState = useCallback(() => {
    setOpenPools({});
    setDropTarget(null);
    setLastClicked(null);
    dragDataRef.current = null;
    dragCountersRef.current = {};
  }, []);

  const removeSubtask = useCallback(
    (index: number) => {
      const name =
        subtasks[index]?.name ||
        t('ioi.subtask.defaultName', { index: index + 1 });
      const count = subtasks[index]?.test_cases.length ?? 0;
      const msg =
        count > 0
          ? t('ioi.subtask.removeConfirm', { name, count })
          : t('ioi.subtask.removeConfirmEmpty', { name });
      confirm({
        title: msg,
        confirmLabel: t('ioi.subtask.remove'),
        destructive: true,
        onConfirm: () => {
          onChange(subtasks.filter((_, i) => i !== index));
          setSubtaskKeys((prev) => prev.filter((_, i) => i !== index));
          // Shift index-keyed state maps so entries above the removed index move down by 1
          setOpenPools((prev) => {
            const shifted: Record<number, boolean> = {};
            for (const [k, v] of Object.entries(prev)) {
              const ki = Number(k);
              if (ki < index) shifted[ki] = v;
              else if (ki > index) shifted[ki - 1] = v;
            }
            return shifted;
          });
        },
      });
    },
    [subtasks, onChange, confirm, t],
  );

  const addSubtask = useCallback(() => {
    onChange([...subtasks, defaultSubtask(subtasks.length, t)]);
    setSubtaskKeys((prev) => [...prev, nextKeyRef.current++]);
  }, [subtasks, onChange, t]);

  const applyImportedSubtasks = useCallback(
    (imported: SubtaskValue[], mode: 'replace' | 'merge') => {
      const startIndex = mode === 'merge' ? subtasks.length : 0;
      const materialized = imported.map((subtask, index) => ({
        ...subtask,
        name: subtask.name.trim() || defaultSubtask(startIndex + index, t).name,
        test_cases: [...subtask.test_cases],
      }));
      const nextSubtasks =
        mode === 'merge' ? [...subtasks, ...materialized] : materialized;

      onChange(nextSubtasks);
      if (mode === 'merge') {
        setSubtaskKeys((prev) => [
          ...prev,
          ...materialized.map(() => nextKeyRef.current++),
        ]);
      } else {
        setSubtaskKeys(materialized.map(() => nextKeyRef.current++));
      }
      setPendingImport(null);
      setStatusMessage(null);
      resetInteractiveState();
    },
    [onChange, resetInteractiveState, subtasks, t],
  );

  const handleCopyJson = useCallback(async () => {
    if (!navigator.clipboard?.writeText) {
      setStatusMessage({
        tone: 'error',
        text: t('ioi.subtask.pasteClipboardError'),
      });
      return;
    }
    try {
      await navigator.clipboard.writeText(JSON.stringify(subtasks, null, 2));
      setPendingImport(null);
      setStatusMessage({
        tone: 'success',
        text: t('ioi.subtask.copySuccess'),
      });
    } catch {
      setStatusMessage({
        tone: 'error',
        text: t('ioi.subtask.pasteClipboardError'),
      });
    }
  }, [subtasks, t]);

  const handlePasteJson = useCallback(async () => {
    if (!navigator.clipboard?.readText) {
      setStatusMessage({
        tone: 'error',
        text: t('ioi.subtask.pasteClipboardError'),
      });
      return;
    }

    try {
      const clipboardText = await navigator.clipboard.readText();
      let parsed: unknown;
      try {
        parsed = JSON.parse(clipboardText);
      } catch {
        setPendingImport(null);
        setStatusMessage({
          tone: 'error',
          text: t('ioi.subtask.pasteInvalidJson'),
        });
        return;
      }

      const normalized = normalizeImportedSubtasks(parsed);
      if (!normalized) {
        setPendingImport(null);
        setStatusMessage({
          tone: 'error',
          text: t('ioi.subtask.pasteInvalidShape'),
        });
        return;
      }

      if (subtasks.length === 0) {
        applyImportedSubtasks(normalized, 'replace');
        return;
      }

      setPendingImport(normalized);
      setStatusMessage(null);
      resetInteractiveState();
    } catch {
      setPendingImport(null);
      setStatusMessage({
        tone: 'error',
        text: t('ioi.subtask.pasteClipboardError'),
      });
    }
  }, [applyImportedSubtasks, resetInteractiveState, subtasks.length, t]);

  // Pool click handler with shift/ctrl support
  const handlePoolClick = useCallback(
    (tc: TestCaseListItem, subtaskIdx: number, e: React.MouseEvent) => {
      const label = tcLabel(tc);
      const owners = assignmentMap.get(label) ?? [];
      // Block click if assigned exclusively to other subtasks (not this one)
      if (owners.length > 0 && !owners.includes(subtaskIdx)) return;

      const isAssigned = subtasks[subtaskIdx].test_cases.includes(label);

      if (e.shiftKey && lastClicked !== null) {
        // Range select from last clicked to this one
        const labels = testCases.map((t) => tcLabel(t));
        const lastIdx = labels.indexOf(lastClicked);
        const thisIdx = labels.indexOf(label);
        if (lastIdx !== -1 && thisIdx !== -1) {
          const lo = Math.min(lastIdx, thisIdx);
          const hi = Math.max(lastIdx, thisIdx);
          const rangeLabels = labels.slice(lo, hi + 1).filter((l) => {
            const o = assignmentMap.get(l) ?? [];
            return o.length === 0 || o.includes(subtaskIdx);
          });

          if (isAssigned) {
            // Deselect range
            updateSubtask(subtaskIdx, {
              test_cases: subtasks[subtaskIdx].test_cases.filter(
                (l) => !rangeLabels.includes(l),
              ),
            });
          } else {
            // Select range
            const existing = new Set(subtasks[subtaskIdx].test_cases);
            const merged = [
              ...subtasks[subtaskIdx].test_cases,
              ...rangeLabels.filter((l) => !existing.has(l)),
            ];
            updateSubtask(subtaskIdx, { test_cases: merged });
          }
        }
      } else {
        // Toggle single
        if (isAssigned) {
          updateSubtask(subtaskIdx, {
            test_cases: subtasks[subtaskIdx].test_cases.filter(
              (l) => l !== label,
            ),
          });
        } else {
          updateSubtask(subtaskIdx, {
            test_cases: [...subtasks[subtaskIdx].test_cases, label],
          });
        }
      }
      setLastClicked(label);
    },
    [assignmentMap, subtasks, testCases, lastClicked, updateSubtask],
  );

  // Drag & drop
  const handleDragStart = useCallback(
    (e: React.DragEvent, tcLabels: string[], fromSubtask: number | null) => {
      e.dataTransfer.setData('text/plain', '');
      e.dataTransfer.effectAllowed = 'move';
      dragDataRef.current = { tcLabels, fromSubtask };
    },
    [],
  );

  // Clean up drag state when any drag ends (drop or cancel).
  // dragend fires on the source element and bubbles to root.
  const handleDragEnd = useCallback(() => {
    dragDataRef.current = null;
    setDropTarget(null);
    dragCountersRef.current = {};
  }, []);

  const handleDrop = useCallback(
    (toSubtask: number) => {
      const data = dragDataRef.current;
      if (!data) return;
      const { tcLabels, fromSubtask } = data;

      // No-op if dropping on the same subtask
      if (fromSubtask === toSubtask) {
        dragDataRef.current = null;
        setDropTarget(null);
        return;
      }

      const updated = subtasks.map((s, i) => {
        if (i === fromSubtask) {
          return {
            ...s,
            test_cases: s.test_cases.filter((l) => !tcLabels.includes(l)),
          };
        }
        if (i === toSubtask) {
          const existing = new Set(s.test_cases);
          return {
            ...s,
            test_cases: [
              ...s.test_cases,
              ...tcLabels.filter((l) => !existing.has(l)),
            ],
          };
        }
        return s;
      });
      onChange(updated);
      dragDataRef.current = null;
      setDropTarget(null);
    },
    [subtasks, onChange],
  );

  const totalMax = subtasks.reduce((sum, s) => sum + (s.max_score || 0), 0);

  return (
    <div
      ref={rootRef}
      onDragEnd={handleDragEnd}
      className="flex flex-col gap-3 col-span-2"
    >
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <div className="flex items-center gap-2">
          <span className="text-[11px] font-bold uppercase tracking-widest opacity-45">
            {schema.title ?? t('ioi.subtask.title')}
          </span>
          {showAsPlaceholder && inheritedSource && (
            <InheritedBadge source={inheritedSource} />
          )}
          {problemId && !tcLoading && testCases.length > 0 && (
            <span className={cn(monoClass, 'text-[10px] opacity-30 ml-2.5')}>
              {testCases.length !== 1
                ? t('ioi.subtask.testCasesAvailable', {
                    count: testCases.length,
                  })
                : t('ioi.subtask.testCaseAvailable', {
                    count: testCases.length,
                  })}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 flex-wrap justify-end">
          {subtasks.length > 0 && (
            <>
              <span className={cn(monoClass, 'text-[11px] opacity-40')}>
                {subtasks.length !== 1
                  ? t('ioi.subtask.subtaskCount', { count: subtasks.length })
                  : t('ioi.subtask.subtaskCountSingular', {
                      count: subtasks.length,
                    })}
              </span>
              <span
                className={cn(
                  monoClass,
                  'text-[11px] font-bold px-2.5 py-0.5 rounded-[10px]',
                  totalMax === 100
                    ? 'bg-emerald-500/10 text-emerald-700'
                    : 'bg-amber-500/10 text-amber-600',
                )}
              >
                {t('ioi.subtask.points', { score: totalMax })}
              </span>
            </>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={handleCopyJson}
            className="text-[11px] font-semibold opacity-75"
          >
            {t('ioi.subtask.copyJson')}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={handlePasteJson}
            className="text-[11px] font-semibold opacity-75"
          >
            {t('ioi.subtask.pasteJson')}
          </Button>
        </div>
      </div>

      {statusMessage && (
        <div
          className={cn(
            'px-3 py-2.5 rounded-lg text-xs border',
            statusMessage.tone === 'success'
              ? 'border-emerald-500/25 bg-emerald-500/[0.08] text-emerald-800'
              : 'border-red-500/25 bg-red-500/[0.08] text-red-600',
          )}
        >
          {statusMessage.text}
        </div>
      )}

      {pendingImport && (
        <div className="p-3 rounded-[10px] border border-border bg-muted/55 flex items-center justify-between gap-3 flex-wrap">
          <div className="text-xs font-semibold text-foreground">
            {t('ioi.subtask.importReady', { count: pendingImport.length })}
          </div>
          <div className="flex gap-2 flex-wrap">
            <Button
              variant="outline"
              size="sm"
              onClick={() => applyImportedSubtasks(pendingImport, 'replace')}
              className="border-primary text-primary"
            >
              {t('ioi.subtask.importReplace')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => applyImportedSubtasks(pendingImport, 'merge')}
              className="opacity-75"
            >
              {t('ioi.subtask.importMerge')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPendingImport(null)}
              className="opacity-75"
            >
              {t('ioi.subtask.importCancel')}
            </Button>
          </div>
        </div>
      )}

      {schema.description && (
        <p className="text-xs opacity-45 m-0 leading-normal">
          {schema.description}
        </p>
      )}

      {problemId && tcLoading && (
        <div className="p-4 text-center text-xs opacity-50 rounded-lg border border-border">
          {t('ioi.subtask.loading')}
        </div>
      )}

      {problemId && tcError != null && (
        <div className="px-4 py-3 rounded-lg bg-red-500/[0.06] border border-red-500/20 flex items-center justify-between text-xs text-red-600">
          <span>{getErrorMessage(tcError, t('ioi.subtask.loadFailed'))}</span>
          <Button
            variant="outline"
            size="sm"
            onClick={refetch}
            className="border-red-500/30 text-inherit text-[11px]"
          >
            {t('ioi.subtask.retry')}
          </Button>
        </div>
      )}

      {!tcLoading && problemId && testCases.length === 0 && (
        <div className="text-center p-4 text-xs text-muted-foreground rounded-lg border border-dashed border-border">
          {t('ioi.subtask.noTestCasesOnProblem')}
        </div>
      )}

      {subtasks.length > 0 &&
        testCases.length > 0 &&
        (unassignedLabels.length > 0 || duplicateLabels.length > 0) && (
          <div
            className={cn(
              'rounded-lg border overflow-hidden',
              duplicateLabels.length > 0
                ? 'border-red-500/40 shadow-[0_0_0_1px_rgba(239,68,68,0.15)]'
                : 'border-amber-500/30',
            )}
          >
            <button
              type="button"
              onClick={() => setWarningsExpanded((p) => !p)}
              className={cn(
                'w-full px-3 py-[7px] border-none cursor-pointer flex items-center gap-1.5 text-[11px] font-semibold text-left',
                duplicateLabels.length > 0
                  ? 'bg-red-500/[0.06] text-red-600'
                  : 'bg-amber-500/[0.06] text-amber-800',
              )}
            >
              <span className="text-[13px]">
                {warningsExpanded ? '\u25BE' : '\u25B8'}
              </span>
              <span>
                {(() => {
                  const wc =
                    (unassignedLabels.length > 0 ? 1 : 0) +
                    (duplicateLabels.length > 0 ? 1 : 0);
                  return wc !== 1
                    ? t('ioi.subtask.warningCount', { count: wc })
                    : t('ioi.subtask.warningCountSingular', { count: wc });
                })()}
              </span>
              {unassignedLabels.length > 0 && (
                <span className="font-normal opacity-70">
                  {t('ioi.subtask.unassignedCount', {
                    count: unassignedLabels.length,
                  })}
                </span>
              )}
              {duplicateLabels.length > 0 && (
                <span className="font-normal opacity-70">
                  {duplicateLabels.length !== 1
                    ? t('ioi.subtask.duplicateCount', {
                        count: duplicateLabels.length,
                      })
                    : t('ioi.subtask.duplicateCountSingular', {
                        count: duplicateLabels.length,
                      })}
                </span>
              )}
            </button>
            {warningsExpanded && (
              <div className="flex flex-col gap-px max-h-[200px] overflow-y-auto">
                {unassignedLabels.length > 0 && (
                  <div className="px-3 py-2 text-[11px] bg-amber-500/[0.04] text-amber-800 flex items-center gap-1.5">
                    <span>
                      {unassignedLabels.length !== 1
                        ? t('ioi.subtask.unassignedMessage', {
                            count: unassignedLabels.length,
                          })
                        : t('ioi.subtask.unassignedMessageSingular', {
                            count: unassignedLabels.length,
                          })}
                    </span>
                  </div>
                )}
                {duplicateLabels.length > 0 && (
                  <div className="px-3 py-2 text-[11px] bg-red-500/[0.04] text-red-600">
                    <div className="font-semibold mb-1">
                      {duplicateLabels.length !== 1
                        ? t('ioi.subtask.duplicateMessage', {
                            count: duplicateLabels.length,
                          })
                        : t('ioi.subtask.duplicateMessageSingular', {
                            count: duplicateLabels.length,
                          })}
                    </div>
                    <div className="opacity-80">
                      {duplicateLabels.slice(0, 12).map(([label, indices]) => {
                        const tc = tcByLabel.get(label);
                        const display = tc
                          ? tc.label
                            ? tc.label
                            : `#${tc.position + 1}`
                          : label;
                        const names = indices
                          .map(
                            (i) =>
                              subtasks[i]?.name ||
                              t('ioi.subtask.defaultName', { index: i + 1 }),
                          )
                          .join(', ');
                        return (
                          <div
                            key={label}
                            className={cn(
                              monoClass,
                              'text-[10px] leading-relaxed',
                            )}
                          >
                            {display} {'\u2192'} {names}
                          </div>
                        );
                      })}
                      {duplicateLabels.length > 12 && (
                        <div className="text-[10px] opacity-60">
                          {t('ioi.subtask.andMore', {
                            count: duplicateLabels.length - 12,
                          })}
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            )}
          </div>
        )}

      {subtasks.length === 0 && (
        <div className="p-7 rounded-[10px] border-[1.5px] border-dashed border-border text-center bg-muted/40">
          <div className="text-[13px] opacity-45 mb-1">
            {t('ioi.subtask.noSubtasks')}
          </div>
          <div className="text-[11px] opacity-30">
            {t('ioi.subtask.noSubtasksHint')}
          </div>
        </div>
      )}

      {subtasks.map((subtask, idx) => {
        const methodInfo = getMethodInfo(subtask.scoring_method);
        const isPoolOpen = openPools[idx] ?? false;
        return (
          <div
            key={subtaskKeys[idx] ?? idx}
            onDragEnter={(e) => {
              e.preventDefault();
              dragCountersRef.current[idx] =
                (dragCountersRef.current[idx] ?? 0) + 1;
              setDropTarget(idx);
            }}
            onDragOver={(e) => {
              e.preventDefault();
            }}
            onDragLeave={() => {
              dragCountersRef.current[idx] =
                (dragCountersRef.current[idx] ?? 0) - 1;
              if (dragCountersRef.current[idx] <= 0) {
                dragCountersRef.current[idx] = 0;
                setDropTarget((prev) => (prev === idx ? null : prev));
              }
            }}
            onDrop={(e) => {
              e.preventDefault();
              dragCountersRef.current[idx] = 0;
              handleDrop(idx);
            }}
            className={cn(
              'rounded-[10px] transition-[border-color,background] duration-150',
              dropTarget === idx
                ? 'border-[1.5px] border-primary bg-indigo-600/[0.02]'
                : 'border border-border bg-card',
            )}
          >
            {/* Color bar — uses per-method dynamic color, must stay inline */}
            <div
              className="h-[3px] rounded-t-[10px]"
              style={{
                background: `linear-gradient(90deg, ${methodInfo.color}, color-mix(in srgb, ${methodInfo.color} 30%, transparent))`,
              }}
            />

            <div className="p-3.5 flex flex-col gap-2.5">
              <div className="grid grid-cols-[1fr_auto_auto_auto] gap-2.5 items-end">
                <div>
                  <div className="text-[9px] font-bold uppercase tracking-widest opacity-40 block mb-1">
                    {t('ioi.subtask.fieldName')}
                  </div>
                  <Input
                    type="text"
                    value={subtask.name}
                    onChange={(e) =>
                      updateSubtask(idx, { name: e.target.value })
                    }
                    className={cn(
                      'w-full',
                      duplicateNameIndices.has(idx) && 'border-amber-500/50',
                    )}
                    maxLength={50}
                  />
                  {duplicateNameIndices.has(idx) && (
                    <div className="text-[10px] text-amber-800 mt-0.5">
                      {t('ioi.subtask.duplicateName')}
                    </div>
                  )}
                </div>
                <div>
                  <div className="text-[9px] font-bold uppercase tracking-widest opacity-40 block mb-1">
                    {t('ioi.subtask.fieldMethod')}
                  </div>
                  <Select
                    value={subtask.scoring_method}
                    onValueChange={(v) =>
                      updateSubtask(idx, { scoring_method: v })
                    }
                  >
                    <SelectTrigger className="min-w-[130px]">
                      <div className="flex items-center gap-1.5">
                        <span
                          className={cn(
                            monoClass,
                            'text-[9px] font-extrabold px-[5px] py-px rounded-[3px] tracking-tight',
                          )}
                          style={{
                            background: methodInfo.bg,
                            color: methodInfo.color,
                          }}
                        >
                          {t(methodInfo.shortKey)}
                        </span>
                        <SelectValue />
                      </div>
                    </SelectTrigger>
                    <SelectContent>
                      {SCORING_METHODS.map((m) => (
                        <SelectItem key={m.key} value={m.key}>
                          {t(m.labelKey)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div>
                  <div className="text-[9px] font-bold uppercase tracking-widest opacity-40 block mb-1">
                    {t('ioi.subtask.fieldPoints')}
                  </div>
                  <Input
                    type="number"
                    min={0}
                    max={10000}
                    step={1}
                    value={subtask.max_score}
                    onChange={(e) =>
                      updateSubtask(idx, {
                        max_score: parseFloat(e.target.value) || 0,
                      })
                    }
                    className={cn(monoClass, 'w-20')}
                  />
                </div>
                <Button
                  aria-label={t('ioi.subtask.removeSubtask')}
                  variant="ghost"
                  size="icon"
                  onClick={() => removeSubtask(idx)}
                  title={t('ioi.subtask.removeSubtask')}
                  className="size-[30px] opacity-30 hover:opacity-100 hover:text-destructive hover:border-destructive hover:bg-red-500/[0.06] transition-all duration-150"
                >
                  {REMOVE_GLYPH}
                </Button>
              </div>

              <div className="text-[11px] opacity-35 italic pl-0.5">
                {t(methodInfo.hintKey)}
              </div>

              <div className="p-3 rounded-lg bg-muted">
                <div className="flex items-center justify-between mb-2">
                  <span className="text-[9px] font-bold uppercase tracking-widest opacity-40 flex items-center gap-2">
                    <span>{t('ioi.subtask.testCases')}</span>
                    <span className={cn('font-normal', monoClass)}>
                      {t('ioi.subtask.assigned', {
                        count: subtask.test_cases.length,
                      })}
                    </span>
                  </span>
                  {problemId && testCases.length > 0 && (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setOpenPools((p) => ({ ...p, [idx]: !p[idx] }))
                      }
                      className={cn(
                        'text-[10px] font-semibold uppercase tracking-tight transition-all duration-150 h-auto py-[3px] px-2.5',
                        isPoolOpen
                          ? 'bg-indigo-600/[0.08] border-indigo-600/30 text-indigo-600'
                          : 'opacity-50',
                      )}
                    >
                      {isPoolOpen
                        ? t('ioi.subtask.hidePool')
                        : t('ioi.subtask.showPool')}
                    </Button>
                  )}
                </div>

                {testCases.length > 0 && (
                  <div className="mb-2">
                    <UnifiedSearch
                      testCases={testCases}
                      subtask={subtask}
                      subtaskIdx={idx}
                      updateSubtask={updateSubtask}
                      tcByPosition={tcByPosition}
                      assignmentMap={assignmentMap}
                      duplicateSet={duplicateSet}
                      subtaskNames={subtaskNames}
                      portalTarget={portalInfo.target}
                      portalOffset={portalInfo.offset}
                    />
                  </div>
                )}

                {/* Assigned chips */}
                <div
                  className="flex flex-wrap gap-1 min-h-7 max-h-40 overflow-y-auto items-center"
                  title={t('ioi.subtask.chipPreviewHint')}
                >
                  {subtask.test_cases.map((tcId) => {
                    const tc = tcByLabel.get(tcId);
                    if (!tc) {
                      // Orphaned label -- show as raw string
                      return (
                        <span
                          key={tcId}
                          className={cn(
                            monoClass,
                            'inline-flex items-center gap-[3px] px-2 py-[3px] rounded-md text-[11px] bg-red-500/[0.06] border border-red-500/20 text-red-600',
                          )}
                        >
                          {tcId}
                          <button
                            type="button"
                            onClick={() =>
                              updateSubtask(idx, {
                                test_cases: subtask.test_cases.filter(
                                  (l) => l !== tcId,
                                ),
                              })
                            }
                            aria-label={t('ioi.subtask.removeTestCase', {
                              label: tcId,
                            })}
                            className="bg-transparent border-none cursor-pointer text-[10px] opacity-50 px-px py-0 text-inherit"
                          >
                            {REMOVE_GLYPH}
                          </button>
                        </span>
                      );
                    }
                    return (
                      <TcChip
                        key={tcId}
                        tc={tc}
                        onRemove={() =>
                          updateSubtask(idx, {
                            test_cases: subtask.test_cases.filter(
                              (l) => l !== tcId,
                            ),
                          })
                        }
                        onDragStart={(e) => handleDragStart(e, [tcId], idx)}
                        isDuplicate={duplicateSet.has(tcId)}
                      />
                    );
                  })}
                  {subtask.test_cases.length === 0 && (
                    <span className="text-[11px] opacity-30 italic">
                      {t('ioi.subtask.noTestCases')}
                    </span>
                  )}
                </div>

                {/* Pool grid */}
                {isPoolOpen && (
                  <div className="mt-2.5">
                    <div className="flex items-center justify-between mb-2">
                      <span className="text-[10px] opacity-40 font-semibold">
                        {t('ioi.subtask.allTestCases')}
                      </span>
                      <div className="flex gap-1.5">
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            const unassignedForThis = testCases
                              .filter((tc) => !assignmentMap.has(tcLabel(tc)))
                              .map((tc) => tcLabel(tc));
                            updateSubtask(idx, {
                              test_cases: [
                                ...new Set([
                                  ...subtask.test_cases,
                                  ...unassignedForThis,
                                ]),
                              ],
                            });
                          }}
                          className="text-[9px] font-semibold opacity-50 uppercase tracking-tight h-auto py-0.5 px-2"
                        >
                          {t('ioi.subtask.addAllUnassigned')}
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            if (subtask.test_cases.length === 0) return;
                            const n = subtask.test_cases.length;
                            confirm({
                              title: t('ioi.subtask.clearConfirm', {
                                count: n,
                                name: subtask.name,
                              }),
                              confirmLabel: t('ioi.subtask.clear'),
                              destructive: true,
                              onConfirm: () =>
                                updateSubtask(idx, { test_cases: [] }),
                            });
                          }}
                          className="text-[9px] font-semibold opacity-50 uppercase tracking-tight h-auto py-0.5 px-2"
                        >
                          {t('ioi.subtask.clear')}
                        </Button>
                      </div>
                    </div>
                    <div className="grid grid-cols-[repeat(auto-fill,minmax(90px,1fr))] gap-1.5 max-h-[300px] overflow-y-auto">
                      {testCases.map((tc) => {
                        const label = tcLabel(tc);
                        const owners = assignmentMap.get(label) ?? [];
                        const state: 'unassigned' | 'this' | 'other' =
                          owners.length === 0
                            ? 'unassigned'
                            : owners.includes(idx)
                              ? 'this'
                              : 'other';
                        const ownerLabel =
                          state === 'other'
                            ? subtasks[owners[0]]?.name
                            : undefined;

                        return (
                          <PoolCard
                            key={tc.id}
                            tc={tc}
                            state={state}
                            ownerLabel={ownerLabel}
                            onClick={(e) => handlePoolClick(tc, idx, e)}
                            onDragStart={(e) =>
                              handleDragStart(
                                e,
                                [label],
                                state === 'this' ? idx : (owners[0] ?? null),
                              )
                            }
                            isDuplicate={duplicateSet.has(label)}
                          />
                        );
                      })}
                    </div>
                  </div>
                )}
              </div>
            </div>
          </div>
        );
      })}

      <Button
        variant="outline"
        onClick={addSubtask}
        className="w-full p-3.5 rounded-[10px] border-[1.5px] border-dashed border-border bg-transparent text-xs font-semibold opacity-40 tracking-tight transition-all duration-200 hover:opacity-80 hover:border-primary hover:text-primary"
      >
        {t('ioi.subtask.addSubtask')}
      </Button>
      {confirmDialog}
    </div>
  );
}
