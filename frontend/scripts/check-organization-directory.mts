import assert from 'node:assert/strict';
import { createOrganizationDirectoryAdapter } from '../src/services/organizationDirectory.ts';

const calls = [];
const adapter = createOrganizationDirectoryAdapter(async params => {
  calls.push(params);
  return { protocol: 'organization-directory.v1', namespace: 'external-test', nodes: [], next_cursor: 'page-2' };
}, 'external-test');
assert.equal(adapter.namespace, 'external-test');
assert.equal((await adapter.children('engineering', 'page-1')).next_cursor, 'page-2');
await adapter.search('张三', 'search-page');
await adapter.resolve(['person:u1', 'department:d1'], 'resolve-page');
assert.deepEqual(calls, [
  [['operation', 'children'], ['parent_id', 'engineering'], ['cursor', 'page-1']],
  [['operation', 'search'], ['query', '张三'], ['cursor', 'search-page']],
  [['operation', 'resolve'], ['ref', 'person:u1'], ['ref', 'department:d1'], ['cursor', 'resolve-page']],
]);
for (const response of [
  { protocol: 'unexpected.v2', namespace: 'external-test', nodes: [] },
  { protocol: 'organization-directory.v1', namespace: 'other-organization', nodes: [] },
]) {
  const invalid = createOrganizationDirectoryAdapter(async () => response, 'external-test');
  await assert.rejects(() => invalid.children(), /协议不兼容/);
}
console.log('PASS directory protocol: children/search/resolve pagination and version/namespace rejection');
