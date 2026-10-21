import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Input, Label } from '@broccoli/web-sdk/ui';
import type { ComponentProps } from 'react';

export function DisplayNameField(props: ComponentProps<typeof Input>) {
  const { t } = useTranslation();
  const hintId = `${props.id}-hint`;
  return (
    <div className="space-y-2">
      <Label htmlFor={props.id}>{t('auth.displayName')}</Label>
      <Input {...props} aria-describedby={hintId} />
      <p id={hintId} className="text-xs text-muted-foreground">
        {t(
          props.required ? 'auth.displayNameEditHint' : 'auth.displayNameHint',
        )}
      </p>
    </div>
  );
}
