import * as SwitchPrimitives from '@radix-ui/react-switch';
import * as React from 'react';

import { cn } from '@/utils';

/**
 * A switch shows no text of its own, so it takes its name from an
 * `aria-label`, from the element `aria-labelledby` points at, or (given an
 * `id`) from a `<Label htmlFor>`. A `<Label>` merely wrapping it does not
 * name it for every screen reader.
 */
export type SwitchProps = React.ComponentPropsWithoutRef<
  typeof SwitchPrimitives.Root
> &
  ({ 'aria-label': string } | { 'aria-labelledby': string } | { id: string });

const Switch = ({
  ref,
  className,
  ...props
}: SwitchProps & {
  ref?: React.RefObject<React.ElementRef<typeof SwitchPrimitives.Root> | null>;
}) => (
  <SwitchPrimitives.Root
    className={cn(
      'peer inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border-2 border-transparent shadow-xs transition-colors focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:cursor-not-allowed disabled:opacity-50 data-[state=checked]:bg-primary data-[state=unchecked]:bg-input',
      className,
    )}
    {...props}
    ref={ref}
  >
    <SwitchPrimitives.Thumb
      className={cn(
        'pointer-events-none block h-4 w-4 rounded-full bg-background shadow-lg ring-0 transition-transform data-[state=checked]:translate-x-4 data-[state=unchecked]:translate-x-0',
      )}
    />
  </SwitchPrimitives.Root>
);
Switch.displayName = SwitchPrimitives.Root.displayName;

export { Switch };
