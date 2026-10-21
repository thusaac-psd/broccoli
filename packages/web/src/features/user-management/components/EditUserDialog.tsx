import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
} from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { Loader2 } from 'lucide-react';
import { useEffect, useState } from 'react';
import { toast } from 'sonner';

import { DisplayNameField } from '@/components/DisplayNameField';
import type { ManagedUserRow } from '@/features/user-management/types';

interface EditUserDialogProps {
  user?: ManagedUserRow;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function EditUserDialog({
  user,
  open,
  onOpenChange,
}: EditUserDialogProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const { user: signedInUser, refresh } = useAuth();

  const [username, setUsername] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [password, setPassword] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);

  useEffect(() => {
    if (!open || !user) return;
    setUsername(user.username);
    setDisplayName(user.display_name);
    setPassword('');
  }, [open, user]);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!user) return;

    const nextUsername = username.trim();
    const nextPassword = password.trim();
    const body: {
      username?: string;
      display_name?: string;
      password?: string;
    } = {};

    if (displayName.trim() !== user.display_name) {
      body.display_name = displayName.trim();
    }

    if (nextUsername && nextUsername !== user.username) {
      body.username = nextUsername;
    }

    if (nextPassword) {
      body.password = nextPassword;
    }

    if (Object.keys(body).length === 0) {
      toast.error(t('users.users.noChanges'));
      return;
    }

    setIsSubmitting(true);
    const { error } = await apiClient.PATCH('/users/{id}', {
      params: { path: { id: user.id } },
      body,
    });
    setIsSubmitting(false);

    if (error) {
      toast.error(getErrorMessage(error, t('users.users.updateUserError')));
      return;
    }

    toast.success(t('users.users.updateUserSuccess'));
    queryClient.invalidateQueries({ queryKey: ['admin-users'] });
    queryClient.invalidateQueries({ queryKey: ['admin-user-detail', user.id] });
    queryClient.invalidateQueries({ queryKey: ['all-users-for-participants'] });
    queryClient.invalidateQueries({ queryKey: ['contest-participants'] });
    if (signedInUser?.id === user.id) await refresh();
    onOpenChange(false);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('users.users.editUser')}</DialogTitle>
          <DialogDescription>
            {user
              ? t('users.users.editUserDescription', {
                  username: user.username,
                })
              : t('users.users.editUserDescriptionFallback')}
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="edit-user-username">
              {t('users.users.username')}
            </Label>
            <Input
              id="edit-user-username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder={t('users.users.usernamePlaceholder')}
              disabled={isSubmitting}
            />
          </div>

          <DisplayNameField
            id="edit-user-display-name"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            required
            disabled={isSubmitting}
          />

          <div className="space-y-2">
            <Label htmlFor="edit-user-password">
              {t('users.users.passwordOptional')}
            </Label>
            <Input
              id="edit-user-password"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder={t('users.users.passwordPlaceholder')}
              disabled={isSubmitting}
            />
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={isSubmitting}
            >
              {t('users.common.cancel')}
            </Button>
            <Button type="submit" disabled={isSubmitting}>
              {isSubmitting && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              {t('users.users.saveUser')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
