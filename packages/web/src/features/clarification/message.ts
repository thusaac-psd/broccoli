/** The longest clarification, reply or message the server accepts. */
export const MESSAGE_MAX_LENGTH = 10000;

export function isSendableMessage(content: string): boolean {
  const length = content.trim().length;
  return length > 0 && length <= MESSAGE_MAX_LENGTH;
}
