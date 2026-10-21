export interface ManagedUserRow {
  id: number;
  username: string;
  display_name: string;
  roles: string[];
  created_at: string;
}

export interface RolePermissionsRow {
  role: string;
  permissions: string[];
  permission_count: number;
}
