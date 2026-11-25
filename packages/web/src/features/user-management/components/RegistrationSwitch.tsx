import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { loadedData, useQueryState } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { QueryErrorState } from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';

import { SwitchField } from '@/features/admin/components/SwitchField';
import {
  REGISTRATION_STATUS_QUERY_KEY,
  useRegistrationStatus,
} from '@/features/auth/hooks/use-registration-status';

/**
 * Opens or closes self-registration for every server. Until the setting
 * loads the switch shows nothing and cannot be flipped, and a setting that
 * could not load says so instead of reading as closed.
 */
export function RegistrationSwitch() {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const status = useQueryState(useRegistrationStatus());
  const open = loadedData(status)?.open;

  const setOpen = async (open: boolean) => {
    const { error } = await apiClient.PUT('/admin/settings/registration', {
      body: { open },
    });
    if (error) {
      toast.error(errorMessage(error, t('users.registration.saveFailed')));
      return;
    }
    queryClient.setQueryData(REGISTRATION_STATUS_QUERY_KEY, { open });
  };

  if (status.status === 'error') {
    return <QueryErrorState state={status} className="py-4" />;
  }
  return (
    <SwitchField
      id="registration-open"
      label={t('users.registration.label')}
      description={t('users.registration.description')}
      checked={open === true}
      disabled={open === undefined}
      onCheckedChange={(next) => void setOpen(next)}
    />
  );
}
