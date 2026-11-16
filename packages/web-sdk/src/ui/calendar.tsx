import { ChevronLeft, ChevronRight } from 'lucide-react';
import * as React from 'react';
import { DayPicker } from 'react-day-picker';

import {
  formatLongDate,
  formatMonthYear,
  formatWeekday,
  useTranslation,
} from '@/i18n';
import { buttonVariants } from '@/ui/button';
import { cn } from '@/utils';

export type CalendarProps = React.ComponentProps<typeof DayPicker>;

function CalendarChevron({ orientation }: { orientation?: string }) {
  const Icon = orientation === 'left' ? ChevronLeft : ChevronRight;
  return <Icon className="h-4 w-4" />;
}

type WeekDay = 0 | 1 | 2 | 3 | 4 | 5 | 6;
type FirstDay = { firstDay: number };

/** The first day of the week in `locale` as DayPicker counts (Sunday is 0). */
function weekStart(locale: string): WeekDay | undefined {
  try {
    const info = new Intl.Locale(locale) as Intl.Locale & {
      getWeekInfo?: () => FirstDay;
      weekInfo?: FirstDay;
    };
    const firstDay = (info.getWeekInfo?.() ?? info.weekInfo)?.firstDay;
    return firstDay === undefined ? undefined : ((firstDay % 7) as WeekDay);
  } catch {
    return undefined;
  }
}

function Calendar({
  className,
  classNames,
  showOutsideDays = true,
  ...props
}: CalendarProps) {
  const { t, locale } = useTranslation();
  return (
    <DayPicker
      lang={locale}
      weekStartsOn={weekStart(locale)}
      formatters={{
        formatCaption: (month) => formatMonthYear(month, locale),
        formatWeekdayName: (day) => formatWeekday(day, locale),
      }}
      labels={{
        labelNext: () => t('dateTime.nextMonth'),
        labelPrevious: () => t('dateTime.previousMonth'),
        labelDayButton: (day) => formatLongDate(day, locale),
        labelGrid: (month) => formatMonthYear(month, locale),
      }}
      showOutsideDays={showOutsideDays}
      className={cn('p-3', className)}
      classNames={{
        months: 'flex flex-col sm:flex-row gap-2',
        month: 'flex flex-col gap-4',
        month_caption: 'flex justify-center pt-1 relative items-center w-full',
        caption_label: 'text-sm font-medium',
        nav: 'flex items-center gap-1',
        button_previous: cn(
          buttonVariants({ variant: 'outline' }),
          'absolute left-1 top-0 h-7 w-7 bg-transparent p-0 opacity-50 hover:opacity-100',
        ),
        button_next: cn(
          buttonVariants({ variant: 'outline' }),
          'absolute right-1 top-0 h-7 w-7 bg-transparent p-0 opacity-50 hover:opacity-100',
        ),
        month_grid: 'w-full border-collapse space-x-1',
        weekdays: 'flex',
        weekday:
          'text-muted-foreground rounded-md w-8 font-normal text-[0.8rem]',
        week: 'flex w-full mt-2',
        day: cn(
          'relative p-0 text-center text-sm focus-within:relative focus-within:z-20 [&:has([aria-selected])]:bg-accent [&:has([aria-selected].day-outside)]:bg-accent/50 [&:has([aria-selected].day-range-end)]:rounded-r-md',
          props.mode === 'range'
            ? '[&:has(>.day-range-end)]:rounded-r-md [&:has(>.day-range-start)]:rounded-l-md first:[&:has([aria-selected])]:rounded-l-md last:[&:has([aria-selected])]:rounded-r-md'
            : '[&:has([aria-selected])]:rounded-md',
        ),
        day_button: cn(
          buttonVariants({ variant: 'ghost' }),
          'h-8 w-8 p-0 font-normal aria-selected:opacity-100',
        ),
        range_end: 'day-range-end',
        range_start: 'day-range-start',
        selected:
          'bg-primary text-primary-foreground hover:bg-primary hover:text-primary-foreground focus:bg-primary focus:text-primary-foreground',
        today: 'bg-accent text-accent-foreground',
        outside:
          'day-outside text-muted-foreground aria-selected:text-muted-foreground',
        disabled: 'text-muted-foreground opacity-50',
        range_middle:
          'aria-selected:bg-accent aria-selected:text-accent-foreground',
        hidden: 'invisible',
        ...classNames,
      }}
      components={{
        Chevron: CalendarChevron,
      }}
      {...props}
    />
  );
}
Calendar.displayName = 'Calendar';

export { Calendar };
