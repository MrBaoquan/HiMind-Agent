import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const page = readFileSync(join(root, 'src/pages/ExtensionDevelopmentPage.tsx'), 'utf8');

assert.match(
  page,
  /\.filter\(project => Boolean\(project\.local\)\)/,
  'development workspace must render only projects with a local source directory',
);
assert.ok(!page.includes('UNMANAGED_KEY'), 'remote-only projects must not create a virtual workspace');
assert.ok(!page.includes('工作台项目'), 'development workspace must not expose a Workbench project group');
assert.match(
  page,
  /props\.remoteProjects\.forEach[\s\S]*?project\.remote = remote;/,
  'remote metadata may still be merged into matching local projects',
);

console.log('development local-only checks passed');
