import { useTranslation } from '@broccoli/web-sdk/i18n';

import RegisterForm from '@/features/auth/components/RegisterForm';
import { useRegistrationStatus } from '@/features/auth/hooks/use-registration-status';
import type { RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = {
  useTitle: function useRegisterTitle() {
    const { t } = useTranslation();
    const open = useRegistrationStatus().data?.open;
    return t(
      open === false ? 'auth.registrationClosedTitle' : 'auth.registerTitle',
    );
  },
};

export default function RegisterPage() {
  return (
    <div className="flex min-h-screen items-center justify-center px-4">
      <RegisterForm />
    </div>
  );
}
