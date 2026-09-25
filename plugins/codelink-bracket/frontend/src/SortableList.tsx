import { useTranslation } from '@broccoli/web-sdk/i18n';
import { cn } from '@broccoli/web-sdk/utils';
import { ArrowDown, ArrowUp, GripVertical } from 'lucide-react';
import { type ReactNode, useState } from 'react';

interface SortableListProps<T> {
  items: T[];
  getKey: (item: T) => string | number;
  renderItem: (item: T, index: number) => ReactNode;
  onChange: (items: T[]) => void;
  disabled?: boolean;
}

function move<T>(items: T[], from: number, to: number): T[] {
  const next = [...items];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item as T);
  return next;
}

/**
 * A vertical list reordered by dragging a row, or with the arrow buttons for
 * keyboard and touch users. Rows reorder live while dragging so the drop
 * position is always visible.
 */
export function SortableList<T>({
  items,
  getKey,
  renderItem,
  onChange,
  disabled,
}: SortableListProps<T>) {
  const { t } = useTranslation();
  const [dragging, setDragging] = useState<number | null>(null);

  return (
    <ol className="flex flex-col gap-1.5">
      {items.map((item, index) => (
        <li
          key={getKey(item)}
          draggable={!disabled}
          onDragStart={(e) => {
            e.dataTransfer.effectAllowed = 'move';
            setDragging(index);
          }}
          onDragOver={(e) => {
            e.preventDefault();
            if (dragging === null || dragging === index) return;
            onChange(move(items, dragging, index));
            setDragging(index);
          }}
          onDragEnd={() => setDragging(null)}
          className={cn(
            'group flex items-center gap-3 rounded-lg border border-border bg-card px-3 py-2 text-sm shadow-xs transition-colors',
            !disabled &&
              'cursor-grab active:cursor-grabbing hover:border-primary/40',
            dragging === index && 'border-primary bg-primary/5 opacity-80',
          )}
        >
          <GripVertical
            aria-hidden
            className="h-4 w-4 shrink-0 text-muted-foreground/60 group-hover:text-muted-foreground"
          />
          <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-muted font-mono text-xs font-semibold tabular-nums">
            {index + 1}
          </span>
          <div className="min-w-0 flex-1">{renderItem(item, index)}</div>
          <div className="flex shrink-0 gap-0.5">
            <button
              type="button"
              disabled={disabled || index === 0}
              onClick={() => onChange(move(items, index, index - 1))}
              aria-label={t('codelink-bracket.sortable.up')}
              className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
            >
              <ArrowUp className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              disabled={disabled || index === items.length - 1}
              onClick={() => onChange(move(items, index, index + 1))}
              aria-label={t('codelink-bracket.sortable.down')}
              className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
            >
              <ArrowDown className="h-3.5 w-3.5" />
            </button>
          </div>
        </li>
      ))}
    </ol>
  );
}
