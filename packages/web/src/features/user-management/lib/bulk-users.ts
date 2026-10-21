export type ParsedBulkUser = {
  username: string;
  display_name: string;
  password?: string;
};

export function normalizeBulkUsers(input: unknown): ParsedBulkUser[] {
  if (!Array.isArray(input)) {
    throw new Error('admin.bulkParticipantsInvalidJson');
  }

  const users: ParsedBulkUser[] = [];
  const seen = new Set<string>();

  for (const item of input) {
    let username = '';
    let displayName: unknown;
    let password: string | undefined;

    if (typeof item === 'string') {
      username = item.trim();
    } else if (item && typeof item === 'object') {
      const record = item as {
        username?: unknown;
        display_name?: unknown;
        password?: unknown;
      };
      username =
        typeof record.username === 'string' ? record.username.trim() : '';
      displayName = record.display_name;
      if (record.password != null && typeof record.password !== 'string') {
        throw new Error('admin.bulkParticipantsInvalidPassword');
      }
      if (typeof record.password === 'string') {
        password = record.password;
      }
    }

    if (!username) {
      throw new Error('admin.bulkParticipantsInvalidUsername');
    }

    if (username.length > 32 || !/^[A-Za-z0-9_]+$/.test(username)) {
      throw new Error('admin.bulkParticipantsInvalidUsername');
    }

    if (
      password !== undefined &&
      (new TextEncoder().encode(password).length < 8 ||
        new TextEncoder().encode(password).length > 128)
    ) {
      throw new Error('admin.bulkParticipantsInvalidPassword');
    }

    if (displayName != null && typeof displayName !== 'string') {
      throw new Error('admin.bulkParticipantsInvalidDisplayName');
    }
    const name =
      (typeof displayName === 'string' ? displayName.trim() : '') || username;
    if ([...name].length > 64 || /\p{Cc}/u.test(name)) {
      throw new Error('admin.bulkParticipantsInvalidDisplayName');
    }

    const key = username.toLowerCase();
    if (seen.has(key)) {
      throw new Error('admin.bulkParticipantsDuplicate');
    }
    seen.add(key);

    users.push({ username, display_name: name, password });
  }

  if (users.length === 0) {
    throw new Error('admin.bulkParticipantsEmpty');
  }

  return users;
}
