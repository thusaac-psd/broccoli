import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useQueryClient } from '@tanstack/react-query';
import { useCallback } from 'react';
import { toast } from 'sonner';

import {
  type Attachment,
  attachmentsQueryKey,
  uploadAttachment,
} from '@/features/problem/api/attachments';

const MAX_FILE_SIZE = 1024 * 1024 * 1024; // 1GB

export function useAttachmentUpload(problemId: number) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();

  const upload = useCallback(
    async (file: File, path?: string): Promise<Attachment | null> => {
      if (file.size > MAX_FILE_SIZE) {
        toast.error(t('admin.attachments.fileTooLarge'));
        return null;
      }

      try {
        const attachment = await uploadAttachment(
          apiClient,
          problemId,
          file,
          path,
        );
        queryClient.invalidateQueries({
          queryKey: attachmentsQueryKey(problemId),
        });
        return attachment;
      } catch (err) {
        toast.error(errorMessage(err, t('admin.attachments.uploadError')));
        return null;
      }
    },
    [errorMessage, apiClient, problemId, queryClient, t],
  );

  return { upload };
}
