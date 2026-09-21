// 市场来源聚合的回归自检（零依赖：node --experimental-strip-types）。
// 覆盖一个真实的信息架构要求：同一个扩展同时存在本地源码、GitHub 发布和
// 组织发布时，市场只能出现一条产品；来源在选择版本时才出现，且安装必须
// 绑定用户选中的那一条来源，不能被列表默认来源替换。
import { strict as assert } from 'node:assert';
import {
  entryIdentity,
  mergeMarketEntries,
  newerVersion,
  resolveSource,
  versionIdentity,
  type MarketProduct,
} from '../src/pages/marketCatalog.ts';

function product(overrides: Partial<MarketProduct> & Pick<MarketProduct, 'kind' | 'id' | 'source'>): MarketProduct {
  const source = resolveSource(overrides.source);
  return {
    key: entryIdentity(overrides.kind, overrides.id, overrides.source, '', ''),
    version: '1.0.0',
    sourceGroup: source.group,
    sourceLabel: source.label,
    artifactId: '',
    sha256: '',
    minAgentVersion: '',
    categories: [],
    capabilityIds: [],
    dependencies: [],
    support: [],
    policyLabel: '可选安装',
    policyKind: 'neutral',
    blocked: false,
    managed: false,
    installedVersion: '',
    updateVersion: '',
    ...overrides,
  } as MarketProduct;
}

// 来源前缀与渠道名都要能归组，否则同一产品会被拆成多条。
assert.equal(resolveSource('local:abc').group, 'local');
assert.equal(resolveSource('github:abc').group, 'remote');
assert.equal(resolveSource('', 'remote').group, 'remote');
assert.equal(resolveSource('system').group, 'system');
assert.equal(resolveSource('marketplace').group, 'organization');

// 三种来源的同一个稳定 ID 合成一条产品，来源信息在候选里保留。
const merged = mergeMarketEntries([
  product({ kind: 'plugin', id: 'com.example.tools', source: 'local:local-1', version: '1.1.0' }),
  product({ kind: 'plugin', id: 'com.example.tools', source: 'github:github-1', version: '1.2.0', sha256: 'a'.repeat(64) }),
  product({ kind: 'plugin', id: 'com.example.tools', source: 'organization', version: '1.0.0', artifactId: 'artifact-1' }),
]);
assert.equal(merged.length, 1);
assert.equal(merged[0].key, 'plugin:com.example.tools');
assert.equal(merged[0].sourceLabel, '多来源');
assert.deepEqual([...merged[0].sourceGroups!].sort(), ['local', 'organization', 'remote']);
assert.equal(merged[0].sourceCandidates?.length, 3);

// 单来源产品保持自己的来源名，不显示成「多来源」。
const single = mergeMarketEntries([product({ kind: 'skill', id: 'com.example.skill', source: 'github:github-1' })]);
assert.equal(single.length, 1);
assert.equal(single[0].sourceLabel, 'GitHub 发布');

// 组织策略优先级最高：只要有一条被禁止，整条产品都不能安装。
const governed = mergeMarketEntries([
  product({ kind: 'plugin', id: 'com.example.tools', source: 'local:local-1' }),
  product({
    kind: 'plugin',
    id: 'com.example.tools',
    source: 'organization',
    blocked: true,
    policyLabel: '组织已禁止',
    policyKind: 'danger',
  }),
]);
assert.equal(governed[0].blocked, true);
assert.equal(governed[0].policyLabel, '组织已禁止');
assert.equal(governed[0].policyKind, 'danger');

// 「可更新」取所有来源里的最高版本，且只在高于已安装版本时出现。
const upgradable = mergeMarketEntries([
  product({ kind: 'skill', id: 'com.example.skill', source: 'local:local-1', version: '1.0.0', installedVersion: '1.0.0' }),
  product({ kind: 'skill', id: 'com.example.skill', source: 'github:github-1', version: '1.4.0', sha256: 'b'.repeat(64) }),
]);
assert.equal(upgradable[0].installedVersion, '1.0.0');
assert.equal(upgradable[0].updateVersion, '1.4.0');
assert.equal(newerVersion('1.0.0', '1.0.0'), '');

// 版本号相同但制品摘要不同，必须保留为两个候选，不能被去重成一条。
const sameVersion = mergeMarketEntries([
  product({ kind: 'workflow', id: 'com.example.flow', source: 'local:local-1', version: '2.0.0' }),
  product({ kind: 'workflow', id: 'com.example.flow', source: 'github:github-1', version: '2.0.0', sha256: 'c'.repeat(64) }),
]);
assert.equal(sameVersion[0].sourceCandidates?.length, 2);
assert.notEqual(
  versionIdentity(sameVersion[0].sourceCandidates![0]),
  versionIdentity(sameVersion[0].sourceCandidates![1]),
);

// 不同扩展 ID 不能互相合并。
assert.equal(mergeMarketEntries([
  product({ kind: 'plugin', id: 'com.example.a', source: 'local:local-1' }),
  product({ kind: 'plugin', id: 'com.example.b', source: 'local:local-1' }),
]).length, 2);

console.log('market catalog checks passed');
