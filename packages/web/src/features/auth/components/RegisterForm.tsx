import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { safeReturnPath, signInPath, useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardTitle,
  Input,
  Label,
} from '@broccoli/web-sdk/ui';
import { type FormEvent, useState } from 'react';
import { Link, useNavigate, useSearchParams } from 'react-router';

import { DisplayNameField } from '@/components/DisplayNameField';
import { useRegistrationStatus } from '@/features/auth/hooks/use-registration-status';

export default function RegisterForm() {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const { login } = useAuth();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const next = safeReturnPath(searchParams.get('next'));

  const [username, setUsername] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [error, setError] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const apiClient = useApiClient();
  const registration = useRegistrationStatus();

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError('');

    if (
      !username.trim() ||
      username.length > 32 ||
      !/^[A-Za-z0-9_]+$/.test(username)
    ) {
      setError(t('validation.usernameFormat'));
      return;
    }

    if (password.length < 8 || password.length > 128) {
      setError(t('validation.passwordLength'));
      return;
    }

    if (password !== confirmPassword) {
      setError(t('auth.passwordMismatch'));
      return;
    }

    setIsSubmitting(true);

    try {
      const { error: regError } = await apiClient.POST('/auth/register', {
        body: { username, display_name: displayName, password },
      });

      if (regError) {
        setError(errorMessage(regError, t('auth.validationError')));
        return;
      }

      await login({ username, password });
      navigate(next);
    } catch (err) {
      setError(errorMessage(err, t('auth.validationError')));
    } finally {
      setIsSubmitting(false);
    }
  };

  if (registration.data?.open === false) {
    return (
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="text-2xl">{t('auth.registerTitle')}</CardTitle>
        </CardHeader>
        <CardContent>
          <p role="status" className="text-sm text-muted-foreground">
            {t('auth.registrationClosed')}
          </p>
        </CardContent>
        <CardFooter>
          <Link to="/login" className="text-sm text-primary underline">
            {t('auth.login')}
          </Link>
        </CardFooter>
      </Card>
    );
  }

  return (
    <Card className="w-full max-w-md">
      <CardHeader>
        <CardTitle className="text-2xl">{t('auth.registerTitle')}</CardTitle>
      </CardHeader>
      <form onSubmit={handleSubmit}>
        <CardContent className="space-y-4">
          {error && <p className="text-sm text-destructive">{error}</p>}
          <div className="space-y-2">
            <Label htmlFor="username">{t('auth.username')}</Label>
            <Input
              id="username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              required
            />
          </div>
          <DisplayNameField
            id="register-display-name"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            disabled={isSubmitting}
          />
          <div className="space-y-2">
            <Label htmlFor="password">{t('auth.password')}</Label>
            <Input
              id="password"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              required
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="confirmPassword">{t('auth.confirmPassword')}</Label>
            <Input
              id="confirmPassword"
              type="password"
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
              required
            />
          </div>
        </CardContent>
        <CardFooter className="flex-col gap-4">
          <Button type="submit" className="w-full" disabled={isSubmitting}>
            {t('auth.register')}
          </Button>
          <p className="text-sm text-muted-foreground">
            {t('auth.haveAccount')}{' '}
            <Link to={signInPath(next)} className="text-primary underline">
              {t('auth.login')}
            </Link>
          </p>
        </CardFooter>
      </form>
    </Card>
  );
}
