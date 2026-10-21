import type { CreatedUser } from '@broccoli/web-sdk/auth';

export function downloadUserCredentials(
  users: Pick<CreatedUser, 'username' | 'display_name' | 'password'>[],
  filename = 'user-credentials.json',
) {
  const text = JSON.stringify(
    users.map(({ username, display_name, password }) => ({
      username,
      display_name,
      password,
    })),
    null,
    2,
  );
  const url = URL.createObjectURL(
    new Blob([text], { type: 'application/json' }),
  );
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
