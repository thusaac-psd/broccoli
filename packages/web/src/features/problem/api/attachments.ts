import { type ApiClient, getErrorMessage } from '@broccoli/web-sdk/api';

export interface Attachment {
  id: string;
  path: string;
  filename: string;
  content_type: string | null;
  size: number;
  content_hash: string;
  created_at: string;
}

export const attachmentsQueryKey = (problemId: number) => [
  'attachments',
  problemId,
];

export async function fetchAttachments(
  apiClient: ApiClient,
  problemId: number,
): Promise<Attachment[]> {
  const { data, error } = await apiClient.GET('/problems/{id}/attachments', {
    params: { path: { id: problemId } },
  });
  if (error) throw error;

  return (data.attachments ?? []).map((att) => ({
    ...att,
    content_type: att.content_type ?? null,
  }));
}

/** An attachment's URL under the API base path (`useApiBasePath()`). */
export function attachmentUrl(
  apiBasePath: string,
  problemId: number,
  refId: string,
): string {
  return `${apiBasePath}/problems/${problemId}/attachments/${refId}`;
}

export function isImageType(contentType: string | null): boolean {
  return contentType?.startsWith('image/') ?? false;
}

export function attachmentMarkdownRef(
  name: string,
  url: string,
  isImage: boolean,
): string {
  return isImage ? `![${name}](${url})` : `[${name}](${url})`;
}

export async function uploadAttachment(
  apiClient: ApiClient,
  problemId: number,
  file: File,
  path?: string,
): Promise<Attachment> {
  const formData = new FormData();
  formData.append('file', file);
  if (path?.trim() && path.trim() !== file.name) {
    formData.append('path', path.trim());
  }

  const { data, error } = await apiClient.POST('/problems/{id}/attachments', {
    params: { path: { id: problemId } },
    body: formData,
    bodySerializer: (body) => body as BodyInit,
  });
  if (error) throw new Error(getErrorMessage(error, 'Upload failed'));

  return {
    ...data,
    content_type: data.content_type ?? null,
  };
}
