import { CalendarIcon, X } from 'lucide-react';
import * as React from 'react';

import { serverNow } from '@/api/server-clock';
import { formatDateTime, useTranslation } from '@/i18n';
import { Button } from '@/ui/button';
import { Calendar } from '@/ui/calendar';
import { Input } from '@/ui/input';
import { Popover, PopoverContent, PopoverTrigger } from '@/ui/popover';
import { cn } from '@/utils';

export interface DateTimePickerProps {
  value: Date | undefined;
  onChange: (date: Date | undefined) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
}

function DateTimePicker({
  value,
  onChange,
  placeholder,
  disabled = false,
  className,
}: DateTimePickerProps) {
  const { t, locale } = useTranslation();
  const [open, setOpen] = React.useState(false);

  const hours = value ? String(value.getHours()).padStart(2, '0') : '00';
  const minutes = value ? String(value.getMinutes()).padStart(2, '0') : '00';

  function handleDateSelect(date: Date | undefined) {
    if (!date) {
      onChange(undefined);
      return;
    }
    const next = new Date(date);
    if (value) {
      next.setHours(value.getHours(), value.getMinutes(), 0, 0);
    } else {
      next.setHours(0, 0, 0, 0);
    }
    onChange(next);
  }

  function handleTimeChange(type: 'hours' | 'minutes', raw: string) {
    const num = parseInt(raw, 10);
    if (isNaN(num)) return;

    const base = value ? new Date(value) : new Date(serverNow());
    if (!value) {
      base.setSeconds(0, 0);
    }

    if (type === 'hours') {
      const clamped = Math.max(0, Math.min(23, num));
      base.setHours(clamped);
    } else {
      const clamped = Math.max(0, Math.min(59, num));
      base.setMinutes(clamped);
    }
    onChange(new Date(base));
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          disabled={disabled}
          className={cn(
            'w-full justify-start text-left font-normal',
            !value && 'text-muted-foreground',
            className,
          )}
        >
          <CalendarIcon className="mr-2 h-4 w-4" />
          {value ? (
            formatDateTime(value, locale, { timeZoneName: true })
          ) : (
            <span>{placeholder ?? t('dateTime.placeholder')}</span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-auto p-0" align="start">
        <Calendar
          mode="single"
          selected={value}
          onSelect={handleDateSelect}
          defaultMonth={value}
        />
        <div className="border-t px-3 py-2 flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">
              {t('dateTime.time')}
            </span>
            <Input
              type="number"
              min={0}
              max={23}
              value={hours}
              onChange={(e) => handleTimeChange('hours', e.target.value)}
              className="w-12 h-7 text-center text-xs px-1"
              aria-label={t('dateTime.hours')}
            />
            <span className="text-xs font-medium">:</span>
            <Input
              type="number"
              min={0}
              max={59}
              value={minutes}
              onChange={(e) => handleTimeChange('minutes', e.target.value)}
              className="w-12 h-7 text-center text-xs px-1"
              aria-label={t('dateTime.minutes')}
            />
          </div>

          {value && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                onChange(undefined);
                setOpen(false);
              }}
              className="h-7 px-2 text-xs text-destructive hover:text-destructive hover:bg-destructive/10"
            >
              <X className="h-3 w-3 mr-1" />
              {t('dateTime.clear')}
            </Button>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
DateTimePicker.displayName = 'DateTimePicker';

export { DateTimePicker };
