import assert from 'node:assert/strict';
import { test } from 'node:test';

import { EventStreamParser, readEventStream } from './event-stream.ts';

test('SSE handles one-byte chunks, CRLF, multiline data and keepalives', () => {
  const parser = new EventStreamParser();
  const wire =
    ': ping\r\n\r\nid: 9007199254740993\r\nevent: notification\r\ndata: first\r\ndata: second:part\r\n\r\n';
  const events = [...wire].flatMap((char) => parser.push(char));
  assert.deepEqual(events, [
    {
      event: 'notification',
      id: '9007199254740993',
      data: 'first\nsecond:part',
    },
  ]);
});

test('SSE preserves the last event id and ignores incomplete events', () => {
  const parser = new EventStreamParser();
  assert.deepEqual(
    parser.push('id: 1\ndata: a\n\ndata: b\n\nid: 2\ndata: unfinished'),
    [
      { event: 'message', id: '1', data: 'a' },
      { event: 'message', id: '1', data: 'b' },
    ],
  );
});

test('stream reader preserves split UTF-8 and waits for processing before advancing', async () => {
  const bytes = new TextEncoder().encode(
    'id: 7\nevent: notification\ndata: 小局结束\n\n',
  );
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const byte of bytes) controller.enqueue(Uint8Array.of(byte));
      controller.close();
    },
  });
  const received: string[] = [];
  await readEventStream(body, async (event) => {
    await Promise.resolve();
    received.push(event.data);
  });
  assert.deepEqual(received, ['小局结束']);
});
