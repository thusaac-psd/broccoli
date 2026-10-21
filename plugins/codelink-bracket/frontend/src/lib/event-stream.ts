export interface ServerEvent {
  event: string;
  data: string;
}

/** Incremental SSE framing, including CRLF split across network chunks. */
export class EventStreamParser {
  private buffer = '';
  private event = '';
  private data: string[] = [];

  push(chunk: string): ServerEvent[] {
    this.buffer += chunk;
    const events: ServerEvent[] = [];
    for (;;) {
      const end = this.buffer.search(/[\r\n]/);
      if (end < 0) break;
      const cr = this.buffer[end] === '\r';
      if (cr && end === this.buffer.length - 1) break;
      const line = this.buffer.slice(0, end);
      this.buffer = this.buffer.slice(
        end + (cr && this.buffer[end + 1] === '\n' ? 2 : 1),
      );
      if (line === '') {
        if (this.data.length) {
          events.push({
            event: this.event || 'message',
            data: this.data.join('\n'),
          });
        }
        this.event = '';
        this.data = [];
        continue;
      }
      if (line.startsWith(':')) continue;
      const colon = line.indexOf(':');
      const field = colon < 0 ? line : line.slice(0, colon);
      const value = colon < 0 ? '' : line.slice(colon + 1).replace(/^ /, '');
      if (field === 'data') this.data.push(value);
      if (field === 'event') this.event = value;
    }
    return events;
  }
}

/** Consume an authenticated fetch response without buffering the whole stream. */
export async function readEventStream(
  body: ReadableStream<Uint8Array>,
  onEvent: (event: ServerEvent) => void | Promise<void>,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  const parser = new EventStreamParser();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      for (const event of parser.push(
        decoder.decode(value, { stream: true }),
      )) {
        await onEvent(event);
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
