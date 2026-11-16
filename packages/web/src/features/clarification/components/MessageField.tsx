import { Textarea } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';

import { MESSAGE_MAX_LENGTH } from '@/features/clarification/message';

/** A message textarea with its length counter and an optional hint. */
export function MessageField({
  value,
  onChange,
  placeholder,
  hint,
  disabled,
  className,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  hint?: string;
  disabled?: boolean;
  className?: string;
}) {
  const length = value.trim().length;
  return (
    <div className="space-y-2">
      <Textarea
        placeholder={placeholder}
        className={cn('min-h-[150px]', className)}
        maxLength={MESSAGE_MAX_LENGTH}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
      <div className="flex justify-between gap-4">
        <p className="text-xs text-muted-foreground">{hint}</p>
        <span
          className={cn(
            'shrink-0 text-xs tabular-nums',
            length > MESSAGE_MAX_LENGTH
              ? 'text-destructive'
              : 'text-muted-foreground',
          )}
        >
          {`${length}/${MESSAGE_MAX_LENGTH}`}
        </span>
      </div>
    </div>
  );
}
