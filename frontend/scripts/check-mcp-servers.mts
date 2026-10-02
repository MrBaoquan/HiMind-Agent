// HiMind AI 工具面板的回归自检（零依赖：node --experimental-strip-types）。
// 面板分两块逻辑：手填连接（mcpServerView）和工具目录（mcpCatalogView）。
// 前者用户唯一会手写的东西是启动命令，命令解析、服务 ID 派生和保存前校验必须锁死；
// 后者把「装之前先在本地说清楚」的规则固定下来——信任等级、来源摘要、搜索，
// 以及安装前置检查，避免目录界面把「拉不到」显示成「没有」。
import { strict as assert } from 'node:assert';
import {
  isReservedServerName,
  parseCommand,
  resolveServerName,
  transportLabel,
  validServerName,
  validateMcpServer,
  type McpServerLike,
} from '../src/pages/mcpServerView.ts';
import {
  BUILTIN_SOURCE_ID,
  CATALOG_PAGE_SIZE,
  CATALOG_PAGE_STEP,
  CATALOG_RISK_NOTE,
  catalogNote,
  emptyCatalogView,
  filterEntries,
  groupCatalog,
  initialValues,
  installBlocker,
  installNotice,
  installsDisabled,
  isCuratedEntry,
  needsAcknowledgement,
  networkSources,
  relativeTime,
  runtimeNote,
  sourceBreakdown,
  sourceNote,
  trustLabel,
  trustPillKind,
  type CatalogEntry,
  type CatalogInput,
  type CatalogSource,
  type CatalogView,
} from '../src/pages/mcpCatalogView.ts';
import { formatCommand, splitCommandLine } from '../src/lib/commandLine.ts';

const draft = (patch: Partial<McpServerLike> = {}): McpServerLike => ({
  server_name: '',
  display_name: '',
  transport: 'stdio',
  command: 'npx',
  args: [],
  env: {},
  cwd: '',
  url: '',
  headers: {},
  tool_call_timeout_ms: 30_000,
  fail_on_startup_error: false,
  reconnect: true,
  enabled: true,
  ...patch,
});

const entry = (patch: Partial<CatalogEntry> = {}): CatalogEntry => ({
  id: 'io.example/tool',
  source_id: 'registry',
  source_label: 'MCP 公共目录',
  trust: 'verified',
  title: '示例工具',
  description: '',
  version: '1.0.0',
  schema: '',
  repository: '',
  website_url: '',
  transport: 'stdio',
  runtime: 'npx',
  runtime_label: 'npx',
  command_preview: 'npx -y example@1.0.0',
  installable: true,
  reason: '',
  installed_as: '',
  suggested_name: 'example',
  inputs: [],
  ...patch,
});

const input = (patch: Partial<CatalogInput> = {}): CatalogInput => ({
  key: 'env:API_KEY',
  label: 'API 密钥',
  kind: 'string',
  required: false,
  secret: false,
  description: '',
  placeholder: '',
  value: '',
  picker: '',
  ...patch,
});

const source = (patch: Partial<CatalogSource> = {}): CatalogSource => ({
  id: 'registry',
  label: 'MCP 公共目录',
  trust: 'verified',
  url: 'https://registry.modelcontextprotocol.io/v0/servers',
  count: 0,
  fetched_at: 0,
  acknowledged: false,
  error: '',
  ...patch,
});

// ---- 手填连接：命令行解析 ----------------------------------------------------

// 启动命令和终端里一样按空白切分，多个空格不能切出空 token。
assert.deepEqual(parseCommand('npx -y @modelcontextprotocol/server-memory@2026.8.31'), {
  ok: true,
  executable: 'npx',
  args: ['-y', '@modelcontextprotocol/server-memory@2026.8.31'],
});
assert.deepEqual(parseCommand('  npx   -y   pkg  '), { ok: true, executable: 'npx', args: ['-y', 'pkg'] });

// 路径含空格时用双引号包住，引号只用于分组，不进入 token。
assert.deepEqual(parseCommand('"C:\\Program Files\\nodejs\\npx.cmd" -y pkg'), {
  ok: true,
  executable: 'C:\\Program Files\\nodejs\\npx.cmd',
  args: ['-y', 'pkg'],
});

// 引号未闭合、命令为空都必须判错，不能把半条命令悄悄存进配置。
const unterminated = parseCommand('"C:\\Program Files\\npx.cmd -y pkg');
assert.equal(unterminated.ok, false);
const emptyCommand = parseCommand('   ');
assert.equal(emptyCommand.ok, false);
assert.notEqual(emptyCommand.ok ? '' : emptyCommand.error, '');

// 回填编辑框用的是同一行拼法，拆开再拼必须能来回。
const roundTrip = ['C:\\Program Files\\nodejs\\npx.cmd', '-y', '@scope/pkg@1.0.0'];
assert.deepEqual(splitCommandLine(formatCommand(roundTrip[0], roundTrip.slice(1))), roundTrip);

// 服务 ID 兜底顺序：手填 ID → 名称 → 可执行文件名 → 兜底值；新建统一转小写。
assert.equal(resolveServerName({ mode: 'create', typedName: 'My_Server', displayName: '项目知识库', command: 'npx -y pkg' }), 'my_server');
assert.equal(resolveServerName({ mode: 'create', typedName: '', displayName: 'Project Wiki', command: 'npx -y pkg' }), 'project-wiki');
assert.equal(resolveServerName({ mode: 'create', typedName: '', displayName: '项目知识库', command: '"C:\\Tools\\node.exe" --x' }), 'node-exe');
assert.equal(resolveServerName({ mode: 'create', typedName: '', displayName: 'My.MCP Server.v2', command: 'npx pkg' }), 'my-mcp-server-v2');
assert.equal(resolveServerName({ mode: 'create', typedName: '', displayName: '', command: '' }), 'mcp-server');
assert.equal(validServerName(resolveServerName({ mode: 'create', typedName: '', displayName: 'C:\\Tools\\node.exe --x', command: 'C:\\Tools\\node.exe --x' })), true);
// 编辑时保留用户原来的大小写，别再被转一次。
assert.equal(resolveServerName({ mode: 'edit', typedName: 'MixedCase', displayName: '', command: '' }), 'MixedCase');
// 后端只认 32 个字符，超长必须截断而不是原样提交。
assert.equal(resolveServerName({ mode: 'create', typedName: 'A'.repeat(40), displayName: '', command: '' }), 'a'.repeat(32));

// 服务名规则和后端 validate 对齐：字母数字下划线短横线，1–32 位。
assert.equal(validServerName('filesystem'), true);
assert.equal(validServerName('my-server_2'), true);
assert.equal(validServerName('项目'), false);
assert.equal(validServerName('has space'), false);
assert.equal(validServerName('a'.repeat(33)), false);
assert.equal(isReservedServerName('himind'), true);
assert.equal(isReservedServerName('HiMind-Agent'), true);
assert.equal(isReservedServerName('himind-agent '), true);
assert.equal(isReservedServerName('himind-custom'), false);

// 提示语要能落到具体字段上，而不是一律「保存失败」。
assert.equal(validateMcpServer({ serverName: '', draft: draft(), existingNames: [] }).includes('服务 ID'), true);
assert.equal(validateMcpServer({ serverName: 'himind', draft: draft(), existingNames: [] }).includes('内置服务名'), true);
assert.equal(validateMcpServer({ serverName: 'dup', draft: draft(), existingNames: ['DUP'] }).includes('已存在'), true);
// 键规则和后端 validate_map_keys 一致：空键和含 = \0 \r \n 的键非法，普通空格不算。
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ env: { '  ': '1' } }), existingNames: [] }).includes('环境变量名'), true);
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ env: { 'bad\nkey': '1' } }), existingNames: [] }).includes('环境变量名'), true);
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ headers: { 'Bad=Key': '1' } }), existingNames: [] }).includes('请求头'), true);
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ command: '   ' }), existingNames: [] }).includes('启动命令'), true);
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ transport: 'streamable-http', url: 'ftp://x' }), existingNames: [] }).includes('http://'), true);
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ tool_call_timeout_ms: 0 }), existingNames: [] }).includes('至少 1 秒'), true);
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ tool_call_timeout_ms: 10 * 60 * 1000 + 1 }), existingNames: [] }).includes('10 分钟'), true);
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ transport: 'streamable-http', url: 'https://example.com/mcp' }), existingNames: [] }), '');
assert.equal(validateMcpServer({ serverName: 'ok', draft: draft({ env: { API_KEY: 'x' } }), existingNames: ['other'] }), '');

// 列表里显示的连接类型文案只认两种。
assert.equal(transportLabel('stdio'), '本地进程');
assert.equal(transportLabel('streamable-http'), 'HTTP');

// ---- 工具目录：信任等级 ------------------------------------------------------

assert.equal(trustLabel('curated'), '精选');
assert.equal(trustLabel('verified'), '已签名');
assert.equal(trustLabel('unverified'), '未验证来源');
assert.equal(trustPillKind('curated'), 'success');
assert.equal(trustPillKind('verified'), 'neutral');
assert.equal(trustPillKind('unverified'), 'warn');

// 只有未验证来源默认停用，也只有它第一次装要确认。
assert.equal(installsDisabled('curated'), false);
assert.equal(installsDisabled('verified'), false);
assert.equal(installsDisabled('unverified'), true);
assert.equal(needsAcknowledgement('unverified', false), true);
assert.equal(needsAcknowledgement('unverified', true), false);
assert.equal(needsAcknowledgement('verified', false), false);

// ---- 工具目录：时间与来源摘要 ------------------------------------------------

const now = 1_800_000_000;
// sourceNote / catalogNote 内部用的是「此刻」，所以这组断言要拿真实时钟当基准。
const clock = Math.floor(Date.now() / 1000);
assert.equal(relativeTime(0, now), '还没同步过');
assert.equal(relativeTime(now - 30, now), '刚刚同步');
assert.equal(relativeTime(now - 120, now), '2 分钟前同步');
assert.equal(relativeTime(now - 7_200, now), '2 小时前同步');
assert.equal(relativeTime(now - 172_800, now), '2 天前同步');
// 时钟回拨时不能显示负数。
assert.equal(relativeTime(now + 500, now), '刚刚同步');

assert.equal(sourceNote(source({ error: '连接超时' })), '连接超时');
assert.equal(sourceNote(source({ id: BUILTIN_SOURCE_ID, label: '内置精选', count: 4 })), '4 个内置工具');
assert.equal(sourceNote(source({ count: 82, fetched_at: clock - 120 })), `82 条 · 2 分钟前同步`);

assert.equal(sourceBreakdown(emptyCatalogView()), '还没读到目录来源');
assert.equal(
  sourceBreakdown({
    entries: [],
    sources: [source({ id: BUILTIN_SOURCE_ID, label: '内置精选', count: 4 }), source({ count: 12, fetched_at: clock - 120 })],
    fetched_at: clock - 120,
    stale: false,
  }),
  `内置精选：4 个内置工具\nMCP 公共目录：12 条 · 2 分钟前同步`,
);

// 来源坏了要说坏在哪，别把「拉不到」显示成「没有」。
const builtinOnly: CatalogView = { entries: [], sources: [source({ id: BUILTIN_SOURCE_ID, count: 4 })], fetched_at: clock, stale: false };
assert.equal(catalogNote(emptyCatalogView()), '只看内置精选，尚未添加目录来源');
assert.equal(networkSources(builtinOnly).length, 0);
assert.equal(catalogNote({ ...builtinOnly, sources: [source({ error: '连接超时' })] }), 'MCP 公共目录：连接超时');
assert.equal(catalogNote({ ...builtinOnly, sources: [source({ count: 0 })] }), '目录还没同步，点刷新拉取');
// 条数不在这里报：页签数整个目录、分组标题数第三方目录，这里只说新不新鲜。
assert.equal(catalogNote({ entries: [], sources: [source({ count: 20, fetched_at: clock - 120 })], fetched_at: clock - 120, stale: false }), '目录 2 分钟前同步');
assert.equal(catalogNote({ entries: [], sources: [source({ count: 20, fetched_at: clock - 120 })], fetched_at: clock - 120, stale: true }), '目录 2 分钟前同步，建议刷新');

// ---- 工具目录：搜索与分页 ----------------------------------------------------

const searchable = [
  entry({ id: 'a', title: '文件系统', description: '读写本地目录' }),
  entry({ id: 'b', title: 'Memory', description: '长期记忆', source_label: '社区目录' }),
  entry({ id: 'c', title: 'Bash', runtime_label: 'uvx' }),
];
assert.equal(filterEntries(searchable, '').length, 3);
assert.equal(filterEntries(searchable, '  ').length, 3);
assert.equal(filterEntries(searchable, '记忆').map(item => item.id).join(','), 'b');
assert.equal(filterEntries(searchable, '社区').map(item => item.id).join(','), 'b');
assert.equal(filterEntries(searchable, 'uvx').map(item => item.id).join(','), 'c');
assert.equal(filterEntries(searchable, 'FILESYSTEM').length, 0);
assert.equal(filterEntries(searchable, '文件系统').map(item => item.id).join(','), 'a');

// 精选（内置来源）单独置顶：它不受分页限制，也不和第三方混在一组里。
assert.equal(isCuratedEntry(entry({ source_id: BUILTIN_SOURCE_ID })), true);
assert.equal(isCuratedEntry(entry({ trust: 'curated' })), true);
assert.equal(isCuratedEntry(entry()), false);

const curated = entry({ id: 'builtin-memory', source_id: BUILTIN_SOURCE_ID, trust: 'curated', title: '记忆' });
const mixed = groupCatalog([entry({ id: 'community' }), curated], '');
assert.deepEqual(mixed.groups.map(group => group.id), ['curated', 'other']);
assert.deepEqual(mixed.groups[0].entries.map(item => item.id), ['builtin-memory']);

const many = Array.from({ length: CATALOG_PAGE_SIZE + 5 }, (_value, index) => entry({ id: `tool-${index}`, title: `工具 ${index}` }));
const paged = groupCatalog(many, '工具');
assert.equal(paged.groups.length, 1);
assert.equal(paged.groups[0].id, 'other');
assert.equal(paged.groups[0].entries.length, CATALOG_PAGE_SIZE);
assert.equal(paged.hidden, 5);
// 命中数不到一页时，没有「还有 N 条」的尾巴。
assert.equal(groupCatalog(many, '工具 1').hidden, 0);
// 「显示更多」一次加一屏；精选永远铺在最上面，不被第三方挤掉。
assert.equal(CATALOG_PAGE_STEP, CATALOG_PAGE_SIZE);
const curatedMany = [...Array.from({ length: CATALOG_PAGE_SIZE + 10 }, (_value, index) => entry({ id: `tool-${index}` })), curated];
const curatedPaged = groupCatalog(curatedMany, '');
assert.deepEqual(curatedPaged.groups.map(group => group.id), ['curated', 'other']);
assert.equal(curatedPaged.groups[0].entries.length, 1);
assert.equal(curatedPaged.groups[1].entries.length, CATALOG_PAGE_SIZE);
assert.equal(curatedPaged.hidden, 10);
// 组级风险提示只说一次后果，不贴到每张卡上。
assert.equal(CATALOG_RISK_NOTE, '本地 MCP 会在这台机器上执行任意代码，只装你信任的来源。');

// ---- 工具目录：卡片依赖说明 --------------------------------------------------

assert.equal(runtimeNote(entry({ runtime: '' }), {}), null);
assert.deepEqual(runtimeNote(entry({ runtime: 'npx' }), { npx: { available: true } }), { label: '需要 npx', missing: false });
assert.deepEqual(runtimeNote(entry({ runtime: 'uvx', runtime_label: 'uvx' }), { uvx: { available: false } }), { label: '需要 uvx', missing: true });
// 本机没查过就不提示缺失，避免把「没查」显示成「缺失」。
assert.deepEqual(runtimeNote(entry({ runtime: 'uvx', runtime_label: 'uvx' }), {}), { label: '需要 uvx', missing: false });

// ---- 工具目录：安装前置检查 --------------------------------------------------

assert.equal(installBlocker(entry({ installable: false, reason: '只有 HTTP 端点' }), {}, false, false, {}), '只有 HTTP 端点');
assert.equal(installBlocker(entry({ installable: false }), {}, false, false, {}), '这个工具当前装不了。');
assert.equal(installBlocker(entry({ installed_as: 'memory' }), {}, false, false, {}), '已经装过了，服务 ID 是「memory」。');

const requiring = entry({ inputs: [input({ key: 'env:API_KEY', label: 'API 密钥', required: true })] });
assert.equal(installBlocker(requiring, {}, false, false, {}), '请先填写「API 密钥」。');
assert.equal(installBlocker(requiring, { 'env:API_KEY': '   ' }, false, false, {}), '请先填写「API 密钥」。');
assert.equal(installBlocker(requiring, { 'env:API_KEY': 'abc' }, false, false, {}), '');

const untrusted = entry({ trust: 'unverified', inputs: [] });
assert.equal(installBlocker(untrusted, {}, false, false, {}), '请先确认已了解这条工具会在本机执行代码。');
assert.equal(installBlocker(untrusted, {}, true, false, {}), '');
// 来源已经确认过，就不用再问，也不该被拦。
assert.equal(installBlocker(untrusted, {}, false, true, {}), '');

const needsUvx = entry({ runtime: 'uvx', runtime_label: 'uvx' });
assert.equal(installBlocker(needsUvx, {}, false, false, { uvx: { available: false } }), '这条工具需要 uvx，本机还没装。');
assert.equal(installBlocker(needsUvx, {}, false, false, { uvx: { available: true } }), '');
// 本机没查过（requirements 为空）不拦，交给后端最后的判断。
assert.equal(installBlocker(needsUvx, {}, false, false, {}), '');

// ---- 工具目录：默认值与安装结果文案 ------------------------------------------

assert.deepEqual(initialValues(entry({ inputs: [input({ key: 'arg:dir', value: 'F:\\data' }), input({ key: 'env:X' })] })), { 'arg:dir': 'F:\\data' });
assert.equal(installNotice(entry({ title: '文件系统', trust: 'curated' })), '已添加「文件系统」，HiMind AI 已重新连接。');
assert.equal(installNotice(entry({ title: '未知工具', trust: 'unverified' })).includes('默认停用'), true);

console.log('check:mcp-servers OK');
