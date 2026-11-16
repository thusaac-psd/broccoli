import type { Client } from 'openapi-fetch';

import type { components, paths } from '@/api/schema';

export type ApiClient = Client<paths>;

/** The API's named schema components, e.g. `ApiSchemas['ErrorBody']`. */
export type ApiSchemas = components['schemas'];
