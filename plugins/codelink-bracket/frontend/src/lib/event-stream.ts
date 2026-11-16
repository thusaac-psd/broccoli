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

/**
 * How long a stream may stay silent before it counts as dead. The server
 * sends a keep-alive comment every 15 seconds, so a stream that sends nothing
 * for longer is half-open, however connected it still looks.
 */
export const STREAM_IDLE_MS = 20_000;

/** The stream sent nothing, not even a keep-alive, for `STREAM_IDLE_MS`. */
export class StreamIdleError extends Error {
  constructor(idleMs: number) {
    super(`The event stream sent nothing for ${idleMs} ms`);
    this.name = 'StreamIdleError';
  }
}

/**
 * Consume an authenticated fetch response without buffering the whole
 * stream. It throws `StreamIdleError` once the stream sends nothing for
 * `idleMs`, so the caller reconnects instead of trusting a dead stream.
 */
export async function readEventStream(
  body: ReadableStream<Uint8Array>,
  onEvent: (event: ServerEvent) => void | Promise<void>,
  idleMs: number = STREAM_IDLE_MS,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  const parser = new EventStreamParser();
  try {
    for (;;) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const idle = new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new StreamIdleError(idleMs)), idleMs);
      });
      const { done, value } = await Promise.race([reader.read(), idle]).finally(
        () => clearTimeout(timer),
      );
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
