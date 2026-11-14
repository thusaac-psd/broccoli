import {
  Cog,
  FileCode,
  FileText,
  History,
  type LucideIcon,
} from 'lucide-react';

import type { MessageKey } from '@/lib/i18n/catalog';

export interface MessageTypeMeta {
  /** The label's key, or null for a type this build does not know. */
  labelKey: MessageKey | null;
  icon: LucideIcon;
  retryable: boolean;
}

export function messageTypeMeta(type: string): MessageTypeMeta {
  if (type === 'stuck_submission') {
    return {
      labelKey: 'dlq.type.stuckSubmission',
      icon: FileText,
      retryable: true,
    };
  }
  if (type === 'operation_task') {
    return {
      labelKey: 'dlq.type.operationTask',
      icon: Cog,
      retryable: false,
    };
  }
  if (type === 'stuck_code_run') {
    return {
      labelKey: 'dlq.type.stuckCodeRun',
      icon: FileCode,
      retryable: false,
    };
  }
  if (type === 'stuck_submission_judgement') {
    return {
      labelKey: 'dlq.type.stuckSubmissionJudgement',
      icon: History,
      retryable: false,
    };
  }
  return { labelKey: null, icon: Cog, retryable: false };
}
