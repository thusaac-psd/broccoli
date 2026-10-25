import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';

const sources = {
  'ranking.js': '../../plugins/codelink-qualifier/frontend/src/ranking.ts',
  'user.js': '../../packages/web-sdk/src/utils/user.ts',
};
const output = new URL('./public/assets/', import.meta.url);
await mkdir(output, { recursive: true });
for (const [name, source] of Object.entries(sources)) {
  const input = await readFile(new URL(source, import.meta.url), 'utf8');
  await writeFile(new URL(name, output), stripTypeScriptTypes(input));
}
console.log(
  'Built public scoreboard using the shared ranking and name helpers.',
);
