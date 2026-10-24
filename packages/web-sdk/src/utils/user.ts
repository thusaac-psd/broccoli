export interface UserDisplayIdentity {
  username: string;
  display_name?: string | null;
}

/** Prefer a human-readable name, including when an older plugin omits it. */
export function getUserDisplayName(user: UserDisplayIdentity): string {
  return user.display_name?.trim() || user.username;
}
