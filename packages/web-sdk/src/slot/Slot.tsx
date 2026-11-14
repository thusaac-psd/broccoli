import React, { type ReactNode } from 'react';

import { PluginErrorBoundary } from '@/plugin/PluginErrorBoundary';
import { usePluginRegistry } from '@/plugin/registry/use-plugin-registry';
import type { SlotConfig } from '@/plugin/types';
import { useContestSlotContext } from '@/slot/contest-slot-context';
import { useSlotPermissions } from '@/slot/slot-permissions-context';

interface SlotProps {
  name: string;
  /**
   * The element the slot's content renders in. `null` renders it without a
   * container, for a slot whose parent allows only certain children (a list
   * item in a `<ul>`, a row in a `<tbody>`).
   */
  as?: React.ElementType | null;
  className?: string;
  children?: ReactNode;
  /**
   * Additional props to pass to all slot components
   */
  slotProps?: Record<string, unknown>;
}

export function Slot({
  name,
  as = 'div',
  className,
  children,
  slotProps = {},
}: SlotProps) {
  const { getSlots, components } = usePluginRegistry();
  const slotPermissions = useSlotPermissions();
  const userPermissions = slotPermissions?.permissions ?? [];
  const contestSlotContext = useContestSlotContext();
  const currentContestType = contestSlotContext?.contestType ?? null;

  const allSlots = getSlots(name);

  // Bridge ContestSlotContext into slot props so contest-scoped plugin
  // components (e.g. IcpcScoreboard, IoiScoreboard) can read contestId and
  // contestType without each plugin reaching for the context themselves.
  // Caller-supplied slotProps win on conflict.
  const contextProps: Record<string, unknown> = contestSlotContext
    ? {
        contestId: contestSlotContext.contestId,
        contestType: contestSlotContext.contestType,
      }
    : {};
  const mergedSlotProps = { ...contextProps, ...slotProps };

  // Filter out slots that require a permission the user doesn't have.
  // Slots without a `permission` field are visible to everyone.
  // Filter out slots that target a contest_type which doesn't match the
  // current contest. Slots without a `contest_type` field always pass; slots
  // with a `contest_type` only render when a ContestSlotContext is in scope
  // and its contestType matches.
  const slots = allSlots.filter((slot) => {
    if (slot.permission && !userPermissions.includes(slot.permission)) {
      return false;
    }
    const slotContestType = slot.contest_type ?? null;
    if (slotContestType) {
      if (!contestSlotContext) return false;
      if (slotContestType !== currentContestType) return false;
    }
    return true;
  });

  const Container = as;

  // Render slots based on their position
  const renderSlot = (slot: SlotConfig, index: number) => {
    const SlotComponent = components[slot.component];
    if (!SlotComponent) {
      console.warn(`Component ${slot.component} not found for slot ${name}`);
      return null;
    }

    return (
      <PluginErrorBoundary
        key={`${slot.name}-${slot.component}-${index}`}
        pluginName={slot._pluginName ?? 'unknown'}
        componentName={slot.component}
      >
        <SlotComponent {...mergedSlotProps} />
      </PluginErrorBoundary>
    );
  };

  // Group slots by position
  const replaceSlots = slots.filter((s) => s.position === 'replace');
  const wrapSlots = slots.filter((s) => s.position === 'wrap');
  const prependSlots = slots.filter((s) => s.position === 'prepend');
  const beforeSlots = slots.filter((s) => s.position === 'before');
  const afterSlots = slots.filter((s) => s.position === 'after');
  const appendSlots = slots.filter((s) => s.position === 'append');

  // Build content based on position types
  let content: ReactNode;

  // If there are replace slots, use them instead of children
  if (replaceSlots.length > 0) {
    content = replaceSlots.map(renderSlot);
  } else {
    // Normal flow: prepend, before, children, after, append
    content = (
      <>
        {beforeSlots.map(renderSlot)}
        {children}
        {afterSlots.map(renderSlot)}
      </>
    );
  }

  // Apply wrap slots (from outermost to innermost)
  wrapSlots.reverse().forEach((slot) => {
    const WrapperComponent = components[slot.component];
    if (WrapperComponent) {
      const wrapperProps = {
        ...mergedSlotProps,
        children: content,
      };
      content = (
        <PluginErrorBoundary
          pluginName={slot._pluginName ?? 'unknown'}
          componentName={slot.component}
        >
          <WrapperComponent {...wrapperProps} />
        </PluginErrorBoundary>
      );
    }
  });

  return (
    <>
      {prependSlots.map(renderSlot)}
      {Container === null ? (
        content
      ) : (
        <Container data-slot-name={name} className={className}>
          {content}
        </Container>
      )}
      {appendSlots.map(renderSlot)}
    </>
  );
}
