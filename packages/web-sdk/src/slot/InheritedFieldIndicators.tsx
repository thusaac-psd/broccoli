/**
 * Shared UI components for displaying config field inheritance state.
 */
import { useTranslation } from '@/i18n';

/** Badge showing the source scope of an inherited value (e.g., "Contest"). */
export function InheritedBadge({ source }: { source: string }) {
  return (
    <span className="inline-flex items-center rounded-full border border-dashed border-primary/30 bg-primary/5 px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-primary">
      {source}
    </span>
  );
}

/**
 * Annotation below a field showing inheritance info.
 * When `isOverride` is true: "Overrides {value} from {source}"
 * When false: "from {source}"
 */
export function InheritedAnnotation({
  source,
  value,
  isOverride,
}: {
  source: string;
  value: string;
  isOverride: boolean;
}) {
  const { t } = useTranslation();
  if (isOverride) {
    return (
      <p className="mt-0.5 text-[10px] text-muted-foreground">
        {t('plugins.config.overridesAnnotation', { value, source })}
      </p>
    );
  }
  return (
    <p className="mt-0.5 text-[10px] text-primary">
      {t('plugins.config.inheritedFromAnnotation', { source })}
    </p>
  );
}
