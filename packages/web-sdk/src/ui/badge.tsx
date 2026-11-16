import { cva, type VariantProps } from 'class-variance-authority';
import * as React from 'react';

import { cn } from '@/utils';

const badgeVariants = cva(
  'inline-flex items-center rounded-md border px-2.5 py-0.5 text-xs font-semibold transition-colors focus:outline-hidden focus:ring-2 focus:ring-ring focus:ring-offset-2',
  {
    variants: {
      // Verdict colors are dark enough for white text at 4.5:1.
      variant: {
        accepted:
          'border-transparent bg-green-700 text-white shadow-sm hover:bg-green-700/90',
        wronganswer:
          'border-transparent bg-red-700 text-white shadow-sm hover:bg-red-700/90',
        timelimitexceeded:
          'border-transparent bg-orange-700 text-white shadow-sm hover:bg-orange-700/90',
        memorylimitexceeded:
          'border-transparent bg-orange-700 text-white shadow-sm hover:bg-orange-700/90',
        runtimeerror:
          'border-transparent bg-purple-700 text-white shadow-sm hover:bg-purple-700/90',
        default:
          'border-transparent bg-primary text-primary-foreground shadow-sm hover:bg-primary/80',
        secondary:
          'border-transparent bg-secondary text-secondary-foreground hover:bg-secondary/80',
        destructive:
          'border-transparent bg-destructive text-destructive-foreground shadow-sm hover:bg-destructive/80',
        outline: 'text-foreground',
      },
    },
    defaultVariants: {
      variant: 'default',
    },
  },
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof badgeVariants> {}

function Badge({ className, variant, ...props }: BadgeProps) {
  return (
    <div className={cn(badgeVariants({ variant }), className)} {...props} />
  );
}

export { Badge, badgeVariants };
