import type { ApiSchemas } from '@broccoli/web-sdk/api';

export type ManagedUserRow = ApiSchemas['UserResponse'];

export interface RolePermissionsRow {
  role: string;
  permissions: string[];
  permission_count: number;
}
