import { createContext, use, useEffect, useRef, useState } from 'react';

/**
 * The page being shown: the host provides its route's path. A dialog or
 * sheet that is open when it changes closes, so an overlay never covers the
 * page it led to (a "sign in" button taking the user to the sign-in form).
 * Without a provider nothing closes.
 */
export const PageContext = createContext<string | null>(null);

interface OverlayRootProps {
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
}

/**
 * The `open` and `onOpenChange` of an overlay root that closes when the page
 * changes, whether its owner controls it or not.
 */
export function usePageScopedOpen({
  open,
  defaultOpen = false,
  onOpenChange,
}: OverlayRootProps): Required<
  Pick<OverlayRootProps, 'open' | 'onOpenChange'>
> {
  const page = use(PageContext);
  const [uncontrolled, setUncontrolled] = useState(defaultOpen);
  const isOpen = open ?? uncontrolled;
  const setOpen = (next: boolean) => {
    if (open === undefined) setUncontrolled(next);
    onOpenChange?.(next);
  };

  const close = useRef(() => {});
  useEffect(() => {
    close.current = () => {
      if (isOpen) setOpen(false);
    };
  });
  const shownOn = useRef(page);
  useEffect(() => {
    if (shownOn.current === page) return;
    shownOn.current = page;
    close.current();
  }, [page]);

  return { open: isOpen, onOpenChange: setOpen };
}
