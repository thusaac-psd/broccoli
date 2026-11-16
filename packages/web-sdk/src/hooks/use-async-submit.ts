import { useCallback, useRef, useState } from 'react';

/**
 * Where a form submission stands. The user's input is kept in every state:
 * it is only cleared, or its dialog closed, by the caller once `submit`
 * reports success.
 */
export type SubmitState =
  | { status: 'editing' }
  | { status: 'submitting' }
  | { status: 'failed'; error: unknown };

const EDITING: SubmitState = { status: 'editing' };

/**
 * Run a form submission at most once at a time.
 *
 * `submit(action)` runs `action` and resolves to whether it succeeded. A
 * rejection is a failure: the state becomes `failed` with the error and the
 * caller keeps what the user typed, so they can retry. `action` reports its
 * own failure (a toast, or the `failed` state shown inline). A `submit` while
 * another is running is ignored and resolves to false.
 */
export function useAsyncSubmit() {
  const [state, setState] = useState<SubmitState>(EDITING);
  const running = useRef(false);

  const submit = useCallback(
    async (action: () => Promise<unknown>): Promise<boolean> => {
      if (running.current) return false;
      running.current = true;
      setState({ status: 'submitting' });
      try {
        await action();
        setState(EDITING);
        return true;
      } catch (error) {
        setState({ status: 'failed', error });
        return false;
      } finally {
        running.current = false;
      }
    },
    [],
  );

  const reset = useCallback(() => {
    if (!running.current) setState(EDITING);
  }, []);

  return {
    state,
    submitting: state.status === 'submitting',
    submit,
    reset,
  };
}

/**
 * A dialog whose form submits asynchronously: it closes only once the
 * submission succeeds, keeps the user's input when it fails, and cannot be
 * dismissed while a submission is running.
 *
 * Pass `open`/`onOpenChange` to control it from outside; otherwise it keeps
 * its own open state. Spread `open` and `onOpenChange` onto `<Dialog>`.
 */
export function useAsyncDialog(controlled?: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [localOpen, setLocalOpen] = useState(false);
  const open = controlled ? controlled.open : localOpen;
  const setOpen = controlled ? controlled.onOpenChange : setLocalOpen;
  const { state, submitting, submit: run, reset } = useAsyncSubmit();

  const onOpenChange = useCallback(
    (next: boolean) => {
      if (!next) {
        if (submitting) return;
        reset();
      }
      setOpen(next);
    },
    [reset, setOpen, submitting],
  );

  const submit = useCallback(
    async (action: () => Promise<unknown>): Promise<boolean> => {
      const succeeded = await run(action);
      if (succeeded) setOpen(false);
      return succeeded;
    },
    [run, setOpen],
  );

  return { open, onOpenChange, state, submitting, submit };
}
