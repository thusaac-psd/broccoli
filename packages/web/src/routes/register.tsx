import RegisterForm from '@/features/auth/components/RegisterForm';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('auth.registerTitle');

export default function RegisterPage() {
  return (
    <div className="flex min-h-screen items-center justify-center px-4">
      <RegisterForm />
    </div>
  );
}
