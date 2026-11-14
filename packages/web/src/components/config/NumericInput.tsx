import { useTranslation } from '@broccoli/web-sdk/i18n';
import { ChevronDown, ChevronUp } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';

export function NumericInput({
  value,
  onChange,
  min,
  max,
  integer,
  step: stepProp,
  precision,
  unit,
  id,
  inheritedPlaceholder,
}: Readonly<{
  value: number | undefined;
  onChange: (v: number | undefined) => void;
  min?: number;
  max?: number;
  integer?: boolean;
  step?: number;
  precision?: number;
  unit?: string;
  id?: string;
  inheritedPlaceholder?: string;
}>) {
  const { t } = useTranslation();
  const [text, setText] = useState(value !== undefined ? String(value) : '');
  const [focused, setFocused] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  // Sync external value -> text when not focused, applying precision formatting
  useEffect(() => {
    if (!focused) {
      if (value !== undefined) {
        setText(
          precision !== undefined ? value.toFixed(precision) : String(value),
        );
      } else {
        setText('');
      }
    }
  }, [value, focused, precision]);

  const step = stepProp ?? (integer ? 1 : 0.1);

  function clamp(n: number): number {
    let v = n;
    if (min !== undefined) v = Math.max(min, v);
    if (max !== undefined) v = Math.min(max, v);
    return v;
  }

  function commit(raw: string) {
    const trimmed = raw.trim();
    if (trimmed === '' || trimmed === '-') {
      onChange(undefined);
      setText('');
      return;
    }
    const parsed = integer
      ? Number.parseInt(trimmed, 10)
      : Number.parseFloat(trimmed);
    if (Number.isNaN(parsed)) {
      // Reset to previous valid value
      setText(value !== undefined ? String(value) : '');
      return;
    }
    const clamped = clamp(parsed);
    const final = integer ? Math.round(clamped) : clamped;
    onChange(final);
    setText(precision !== undefined ? final.toFixed(precision) : String(final));
  }

  function increment(delta: number) {
    const base = value ?? 0;
    const next = clamp(
      integer
        ? Math.round(base + delta)
        : Math.round((base + delta) * 1e10) / 1e10,
    );
    onChange(next);
    setText(precision !== undefined ? next.toFixed(precision) : String(next));
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      increment(step);
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      increment(-step);
    }
  }

  // Allow digits, minus, decimal point (for floats), and 'e' for scientific notation
  function handleChange(e: React.ChangeEvent<HTMLInputElement>) {
    const raw = e.target.value;
    if (integer) {
      if (raw === '' || raw === '-' || /^-?\d*$/.test(raw)) {
        setText(raw);
      }
    } else {
      if (
        raw === '' ||
        raw === '-' ||
        raw === '.' ||
        /^-?\d*\.?\d*(?:[eE][-+]?\d*)?$/.test(raw)
      ) {
        setText(raw);
      }
    }
  }

  const rangeHint =
    min !== undefined && max !== undefined
      ? `${String(min)} – ${String(max)}`
      : min !== undefined
        ? `≥ ${String(min)}`
        : max !== undefined
          ? `≤ ${String(max)}`
          : null;

  return (
    <div className="flex items-center gap-0">
      <div className="relative flex-1">
        <input
          ref={inputRef}
          id={id}
          type="text"
          inputMode={integer ? 'numeric' : 'decimal'}
          value={text}
          onChange={handleChange}
          onBlur={(e) => {
            setFocused(false);
            commit(e.target.value);
          }}
          onFocus={() => setFocused(true)}
          onKeyDown={handleKeyDown}
          className="flex h-9 w-full rounded-l-md border border-r-0 border-input bg-transparent px-3 py-1 text-sm tabular-nums shadow-xs transition-colors placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring focus-visible:z-10 disabled:cursor-not-allowed disabled:opacity-50"
          placeholder={inheritedPlaceholder ?? (integer ? '0' : '0.0')}
        />
        {(rangeHint || unit) && (
          <span className="absolute right-2 top-1/2 -translate-y-1/2 text-[10px] text-muted-foreground pointer-events-none select-none tabular-nums">
            {unit && rangeHint
              ? `${rangeHint} ${unit}`
              : unit
                ? unit
                : rangeHint}
          </span>
        )}
      </div>
      <div className="flex flex-col">
        <button
          aria-label={t('common.increase')}
          type="button"
          tabIndex={-1}
          onClick={() => increment(step)}
          className="flex h-[18px] w-7 items-center justify-center rounded-tr-md border border-input bg-muted/50 text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground active:bg-accent/80"
        >
          <ChevronUp className="h-3 w-3" />
        </button>
        <button
          aria-label={t('common.decrease')}
          type="button"
          tabIndex={-1}
          onClick={() => increment(-step)}
          className="flex h-[18px] w-7 items-center justify-center rounded-br-md border border-t-0 border-input bg-muted/50 text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground active:bg-accent/80"
        >
          <ChevronDown className="h-3 w-3" />
        </button>
      </div>
    </div>
  );
}
