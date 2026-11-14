import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';

import { SwitchField } from '@/features/admin/components/SwitchField';
import {
  REGISTRATION_STATUS_QUERY_KEY,
  useRegistrationStatus,
} from '@/features/auth/hooks/use-registration-status';

/** Opens or closes self-registration for every server. */
export function RegistrationSwitch() {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const { data } = useRegistrationStatus();

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

  return (
    <SwitchField
      id="registration-open"
      label={t('users.registration.label')}
      description={t('users.registration.description')}
      checked={data?.open ?? false}
      onCheckedChange={(open) => void setOpen(open)}
    />
  );
}
