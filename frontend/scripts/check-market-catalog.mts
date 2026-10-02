// 市场来源聚合的回归自检（零依赖：node --experimental-strip-types）。
// 覆盖一个真实的信息架构要求：同一个扩展同时存在本地源码、GitHub 发布和
// 组织发布时，市场只能出现一条产品；来源在选择版本时才出现，且安装必须
// 绑定用户选中的那一条来源，不能被列表默认来源替换。
import { strict as assert } from 'node:assert';
import {
  countUnitUpdates,
  entryIdentity,
  installActionLabel,
  mergeMarketEntries,
  newerVersion,
  resolveSource,
  resolveSourceIdentity,
  sourceKeyOf,
  sourceNameFor,
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

// 来源筛选要用真实来源名：目录项的 source 带的是配置 ID，据此才能换成用户认得的名字。
assert.equal(sourceKeyOf(resolveSourceIdentity('local:local-1a2b')), 'local-1a2b');
assert.equal(sourceKeyOf(resolveSourceIdentity('github:github-3c4d')), 'github-3c4d');
assert.equal(resolveSourceIdentity('github:github-3c4d').group, 'remote');
assert.equal(sourceKeyOf(resolveSourceIdentity('system')), 'group:system');
assert.equal(sourceKeyOf(resolveSourceIdentity('marketplace')), 'group:organization');
// 没有配置项可查的兜底名字必须是产品叫法，不能退回"本地源码/GitHub 发布"这类类型词。
assert.equal(sourceNameFor('organization'), 'AI 工作台');
assert.equal(sourceNameFor('local'), '本机开发目录');
assert.equal(sourceNameFor('remote'), 'GitHub 导入');
assert.equal(sourceNameFor('system'), '系统内置');

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
// 没有安装记录时，市场展示目录里最新的一版，而不是优先级最高那个来源的版本。
assert.equal(merged[0].version, '1.2.0');

// 单来源产品保持自己的来源名，不显示成「多来源」。
const single = mergeMarketEntries([product({ kind: 'skill', id: 'com.example.skill', source: 'github:github-1' })]);
assert.equal(single.length, 1);
assert.equal(single[0].sourceLabel, 'GitHub 发布');

// 来源筛选和列表展示用真实来源名，多来源产品要能按任意一个来源名筛出来。
const named = mergeMarketEntries([
  product({ kind: 'plugin', id: 'com.example.tools', source: 'local:local-1', sourceId: 'local-1', sourceName: '本机扩展仓库' }),
  product({ kind: 'plugin', id: 'com.example.tools', source: 'github:github-1', sourceId: 'github-1', sourceName: 'HiMind 扩展' }),
]);
assert.deepEqual(named[0].sourceKeys, ['github-1', 'local-1']);
assert.deepEqual(named[0].sourceNames, ['HiMind 扩展', '本机扩展仓库']);
assert.equal(named[0].sourceName, '多来源');
const namedSingle = mergeMarketEntries([product({ kind: 'skill', id: 'com.example.skill', source: 'github:github-1', sourceId: 'github-1', sourceName: 'HiMind 扩展' })]);
assert.equal(namedSingle[0].sourceName, 'HiMind 扩展');
assert.deepEqual(namedSingle[0].sourceKeys, ['github-1']);

// 安装类动作的动词只有一套，并且必须带上目标版本号。
assert.equal(installActionLabel({ target: '1.2.0' }), '安装 v1.2.0');
assert.equal(installActionLabel({ target: '1.2.0', installed: '1.0.0' }), '更新到 v1.2.0');
assert.equal(installActionLabel({ target: '1.2.0', installed: '1.2.0' }), '重新安装 v1.2.0');
assert.equal(installActionLabel({ target: '1.0.0', installed: '1.2.0' }), '降级到 v1.0.0');
assert.equal(installActionLabel({ target: '1.2.0', installed: '1.0.0', locked: '组织管理' }), '组织管理');

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

// 头部版本不能被主来源拖旧：本机装的是本地源的 1.0.2，组织目录还停在 1.0.1 时，
// 市场必须按本机那一版展示。否则列表会写「已安装 · v1.0.1」，详情页的主按钮会变成
// 「降级到 v1.0.1」——用户手里明明是更新的版本，却被推荐往回装。
const installedNewer = mergeMarketEntries([
  product({ kind: 'plugin', id: 'com.example.tools', source: 'local:local-1', version: '1.0.2', sha256: 'd'.repeat(64) }),
  product({
    kind: 'plugin',
    id: 'com.example.tools',
    source: 'organization',
    version: '1.0.1',
    artifactId: 'artifact-1',
    installedVersion: '1.0.2',
  }),
]);
assert.equal(installedNewer[0].version, '1.0.2');
assert.equal(installedNewer[0].source, 'local:local-1');
assert.equal(installedNewer[0].sha256, 'd'.repeat(64));
assert.equal(installedNewer[0].updateVersion, '');

// 有更高版本时，版本号、来源和制品摘要要一起换成那个候选。只换版本号会把 A 来源的
// 版本号和 B 来源的制品拼在一起，安装时会去取一份对不上的包。
const crossSourceUpdate = mergeMarketEntries([
  product({
    kind: 'plugin',
    id: 'com.example.tools',
    source: 'organization',
    version: '1.0.0',
    artifactId: 'artifact-1',
    installedVersion: '1.0.0',
  }),
  product({ kind: 'plugin', id: 'com.example.tools', source: 'github:github-1', version: '1.5.0', sha256: 'e'.repeat(64) }),
]);
assert.equal(crossSourceUpdate[0].version, '1.5.0');
assert.equal(crossSourceUpdate[0].source, 'github:github-1');
assert.equal(crossSourceUpdate[0].sha256, 'e'.repeat(64));
assert.equal(crossSourceUpdate[0].artifactId, '');

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

// 来源卡片的「N 项待更新」必须和市场同一个口径，否则会出现「市场 0 项可更新、
// 来源卡片 4 项待更新」的自相矛盾。现场数据：技能台账记的是 1.1.1，而渲染目录
// 停在 1.1.0，来源卡片据此自己比版本号就会虚报待更新。
// 现在唯一的判定入口是市场算好的「更新目标版本」：卡片只回答这条更新是不是
// 自己提供的（制品版本 === 市场更新目标版本）。
const updateTargets = new Map<string, string>([
  ['plugin:com.himind.free', '1.2.0'],
  ['skill:com.himind.skill', '1.0.4'],
]);
assert.equal(countUnitUpdates([
  // 市场认定 plugin:com.himind.free 可更新到 1.2.0，本单元正好提供 1.2.0 → 计入。
  { asset_kind: 'plugin', asset_id: 'com.himind.free', version: '1.2.0' },
  // 本单元只提供 1.0.0，更新由别的来源提供 → 不该记在本单元头上。
  { asset_kind: 'plugin', asset_id: 'com.himind.other', version: '1.0.0' },
  // 组织管理的制品不会进入更新目标，卡片刻意持有更高版本也不计入。
  { asset_kind: 'plugin', asset_id: 'com.himind.managed', version: '1.4.2' },
  // 组织禁止的制品同理。
  { asset_kind: 'plugin', asset_id: 'com.himind.blocked', version: '1.1.0' },
  // 同版本不同打包摘要属于「重新安装」，不在更新目标里。
  { asset_kind: 'skill', asset_id: 'com.himind.skill', version: '1.0.3' },
  // 台账比渲染目录新（1.1.1 vs 1.1.0）时，卡片不再自己比版本号：
  // 市场说没有更新，卡片就报 0。
  { asset_kind: 'skill', asset_id: 'com.himind.drifted', version: '1.1.1' },
], updateTargets), 1);
// 没有更新目标时只能是 0，避免「市场 0 项、卡片 N 项」再次出现。
assert.equal(countUnitUpdates([
  { asset_kind: 'plugin', asset_id: 'com.himind.free', version: '1.2.0' },
], new Map()), 0);

console.log('market catalog checks passed');
