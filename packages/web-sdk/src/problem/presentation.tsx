/* eslint-disable react-refresh/only-export-components -- The SDK is built separately as a shared module for runtime plugins. */
import {
  createContext,
  type ReactNode,
  use,
  useCallback,
  useLayoutEffect,
  useMemo,
  useState,
} from 'react';

export interface ProblemPresentationPolicy {
  contestId: number;
  viewerId: number;
  hideLabels: boolean;
  /** null leaves access unchanged; an empty list hides all statements. */
  allowedProblemIds: readonly number[] | null;
}

type Policies = Record<string, ProblemPresentationPolicy>;
const Context = createContext<{
  policies: Policies;
  setPolicy: (key: string, value: ProblemPresentationPolicy | null) => void;
}>({ policies: {}, setPolicy: () => undefined });

/** Contest plugins publish presentation rules here; the server still enforces access. */
export function ProblemPresentationProvider({
  children,
}: {
  children: ReactNode;
}) {
  const [policies, setPolicies] = useState<Policies>({});
  const setPolicy = useCallback(
    (key: string, policy: ProblemPresentationPolicy | null) => {
      setPolicies((previous) => {
        if (JSON.stringify(previous[key] ?? null) === JSON.stringify(policy))
          return previous;
        const next = { ...previous };
        if (policy) next[key] = policy;
        else delete next[key];
        return next;
      });
    },
    [],
  );
  const value = useMemo(() => ({ policies, setPolicy }), [policies, setPolicy]);
  return <Context value={value}>{children}</Context>;
}

export function useProblemPresentationPolicy(
  key: string,
  policy: ProblemPresentationPolicy | null,
) {
  const { setPolicy } = use(Context);
  const serialized = JSON.stringify(policy);
  useLayoutEffect(() => {
    setPolicy(key, JSON.parse(serialized) as ProblemPresentationPolicy | null);
    return () => setPolicy(key, null);
  }, [key, serialized, setPolicy]);
}

export function useProblemPresentation(
  contestId: number | null | undefined,
  viewerId: number | null | undefined,
) {
  const { policies } = use(Context);
  return useMemo(() => {
    const active = Object.values(policies).filter(
      (policy) =>
        policy.contestId === contestId && policy.viewerId === viewerId,
    );
    const limits = active.flatMap((policy) =>
      policy.allowedProblemIds === null ? [] : [policy.allowedProblemIds],
    );
    return {
      hideLabels: active.some((policy) => policy.hideLabels),
      allowedProblemIds: limits.length
        ? limits[0].filter((id) =>
            limits.every((allowed) => allowed.includes(id)),
          )
        : null,
    };
  }, [policies, contestId, viewerId]);
}
