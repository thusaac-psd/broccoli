import type { ApiSchemas } from '@broccoli/web-sdk/api';

export type DlqMessage = ApiSchemas['DlqMessageResponse'];
export type DlqMessageDetail = ApiSchemas['DlqMessageDetailResponse'];
export type DlqListResponse = ApiSchemas['DlqListResponse'];
export type DlqStats = ApiSchemas['DlqStatsResponse'];

export type DlqResolvedFilter = 'all' | 'unresolved' | 'resolved';
