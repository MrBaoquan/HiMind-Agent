// AI 客户端（ACP）面板的回归自检（零依赖：node --experimental-strip-types）。
// 面板已经收敛成「点一下就接入」，用户唯一会手写的东西是自定义启动命令，
// 所以命令解析和「不可用」原因断言必须锁死：解析错一次，用户就接不上。
import { strict as assert } from 'node:assert';
import {
  acpPolicyOptions,
  acpPresets,
  acpProfileStatus,
  bareProviderId,
  executableName,
  formatCommand,
  normalizePolicy,
  presetCommand,
  presetVersion,
  resolveProviderId,
  slugify,
  splitCommandLine,
  validProviderId,
} from '../src/pages/acpProfileView.ts';

// 命令按空白切分，多个空格不能切出空 token。
assert.deepEqual(splitCommandLine('npx -y @agentclientprotocol/codex-acp@1.12.0'), [
  'npx',
  '-y',
  '@agentclientprotocol/codex-acp@1.12.0',
]);
assert.deepEqual(splitCommandLine('  opencode   acp  '), ['opencode', 'acp']);
assert.deepEqual(splitCommandLine(''), []);
assert.deepEqual(splitCommandLine('   '), []);

// 路径含空格时用双引号包住；引号只用于分组，不进入 token。
const quotedCommand = '"C:\\Program Files\\nodejs\\npx.cmd" -y pkg';
assert.deepEqual(splitCommandLine(quotedCommand), ['C:\\Program Files\\nodejs\\npx.cmd', '-y', 'pkg']);

// 引号未闭合必须判错，不能把半条命令悄悄存进配置。
assert.equal(splitCommandLine('"C:\\Program Files\\npx.cmd -y pkg'), null);
assert.equal(splitCommandLine('npx "a b'), null);

// 保存用的是 executable + args 两段，回填编辑框又拼成一行，两者必须能来回。
assert.equal(formatCommand('npx', ['-y', 'a b']), 'npx -y "a b"');
assert.equal(formatCommand('opencode', ['acp']), 'opencode acp');
assert.equal(formatCommand('npx', ['']), 'npx');
const roundTrip = ['C:\\Program Files\\nodejs\\npx.cmd', '-y', '@scope/pkg@1.0.0'];
assert.deepEqual(splitCommandLine(formatCommand(roundTrip[0], roundTrip.slice(1))), roundTrip);

// 客户端 ID 兜底来源：先取可执行文件名，再 slug 成合法 ID。
assert.equal(executableName('"C:\\Tools\\node.exe" --x'), 'node.exe');
assert.equal(executableName('/usr/local/bin/opencode'), 'opencode');
assert.equal(executableName('npx'), 'npx');
assert.equal(slugify('My Codex!'), 'my-codex');
assert.equal(slugify('  '), '');
assert.equal(slugify('OpenCode'), 'opencode');

// 用户在高级设置里填的 ID 可能带 acp. 前缀，前缀要去掉但大小写原样保留。
assert.equal(bareProviderId('acp.codex'), 'codex');
assert.equal(bareProviderId('ACP.CODEX'), 'CODEX');
assert.equal(bareProviderId('codex'), 'codex');
assert.equal(bareProviderId('  acp.my-client  '), 'my-client');

// 新建统一小写：工作流侧按 provider.includes('codex') 这类小写子串认品牌。
assert.equal(resolveProviderId({ mode: 'create', typedId: 'MyCodex', displayName: '', command: '' }), 'mycodex');
assert.equal(resolveProviderId({ mode: 'create', typedId: 'acp.codex', displayName: 'Codex', command: 'npx' }), 'codex');
assert.equal(resolveProviderId({ mode: 'create', typedId: '', displayName: 'My Codex', command: '' }), 'my-codex');
// 名称和 ID 都空的时候才退到可执行文件名。
assert.equal(resolveProviderId({ mode: 'create', typedId: '', displayName: '', command: 'npx -y pkg' }), 'npx');
assert.equal(resolveProviderId({ mode: 'create', typedId: '', displayName: '', command: '"C:\\Tools\\OpenCode.exe" acp' }), 'opencode.exe');
// 编辑态沿用已存 ID：只改了个大小写不该变成另一个客户端。
assert.equal(resolveProviderId({ mode: 'edit', typedId: 'MyCodex', displayName: 'x', command: 'npx' }), 'MyCodex');

// ID 规则要和后端 normalize/valid 保持一致，否则保存时才报错。
assert.ok(validProviderId('codex'));
assert.ok(validProviderId('my.client_1'));
assert.ok(validProviderId('a'.repeat(64)));
assert.ok(!validProviderId('a'.repeat(65)));
assert.ok(!validProviderId('codex/acp'));
assert.ok(!validProviderId('我的客户端'));
assert.ok(!validProviderId(''));

// 脏权限策略一律退回 deny，不能把不可识别的值原样提交。
assert.equal(normalizePolicy('allow_once'), 'allow_once');
assert.equal(normalizePolicy('prompt'), 'prompt');
assert.equal(normalizePolicy('deny'), 'deny');
assert.equal(normalizePolicy('allow-always'), 'deny');

// 权限策略三选必须覆盖后端认识的取值，且默认落在「每次询问」。
assert.deepEqual(acpPolicyOptions.map(option => option.value), ['prompt', 'deny', 'allow_once']);
for (const option of acpPolicyOptions) {
  assert.ok(option.label.trim(), '权限策略必须有可读名称');
  assert.ok(option.description.trim(), '权限策略必须有一句人话说明');
}

const baseProfile = {
  provider_id: 'acp.codex',
  display_name: 'Codex',
  executable: 'npx',
  args: ['-y', '@agentclientprotocol/codex-acp@1.12.0'],
  version: '1.12.0',
  permission_policy: 'prompt',
  enabled: true,
};

// 停用不算故障，不能标红。
assert.deepEqual(acpProfileStatus({ ...baseProfile, enabled: false }, { provider: 'acp.codex', status: 'ready' }), {
  tone: 'neutral',
  label: '已停用',
  reason: '',
});
assert.deepEqual(acpProfileStatus(baseProfile, { provider: 'acp.codex', status: 'ready' }), {
  tone: 'success',
  label: '已就绪',
  reason: '',
});

// 任何「不可用 / 配置异常」都必须给得出原因，否则用户只看到一个红色标签。
const unsupported = acpProfileStatus(baseProfile, { provider: 'acp.codex', status: 'unsupported', capabilities: { error: '协议版本不匹配' } });
assert.equal(unsupported.tone, 'danger');
assert.equal(unsupported.label, '配置异常');
assert.equal(unsupported.reason, '协议版本不匹配');

const missingExecutable = acpProfileStatus(baseProfile, {
  provider: 'acp.codex',
  status: 'unavailable',
  capabilities: { executable_available: false, executable_error: 'command not found: npx' },
});
assert.equal(missingExecutable.tone, 'danger');
assert.equal(missingExecutable.label, '不可用');
assert.match(missingExecutable.reason, /未找到命令 npx/);

const badPolicy = acpProfileStatus(baseProfile, {
  provider: 'acp.codex',
  status: 'unavailable',
  capabilities: { executable_available: true, permission_policy_valid: false },
});
assert.match(badPolicy.reason, /权限策略无效/);

// 兜底文案不能是空字符串。
const fallback = acpProfileStatus(baseProfile, { provider: 'acp.codex', status: 'unavailable', capabilities: {} });
assert.ok(fallback.reason.trim());
const noProvider = acpProfileStatus(baseProfile);
assert.equal(noProvider.tone, 'danger');
assert.ok(noProvider.reason.trim());

// 预设是「一键接入」的全部依据：ID 不能重复、必须是合法 ID、命令必须能解析出可执行文件。
assert.equal(acpPresets.length, 4);
assert.deepEqual(acpPresets.map(preset => preset.providerId), ['codex', 'claude', 'opencode', 'github-copilot']);
const seenPresetIds = new Set<string>();
for (const preset of acpPresets) {
  assert.ok(!seenPresetIds.has(preset.providerId), `预设 ID 重复：${preset.providerId}`);
  seenPresetIds.add(preset.providerId);
  assert.ok(validProviderId(preset.providerId), `预设 ID 不合法：${preset.providerId}`);
  // 工作流侧按 provider_id 里的 codex / claude / opencode 识别品牌，预设 ID 不能改名。
  assert.match(`acp.${preset.providerId}`, /acp\.(codex|claude|opencode|github-copilot)$/);
  assert.ok(preset.name.trim(), '预设必须有名称');
  assert.ok(preset.summary.trim(), '预设必须有说明');
  // 版本号只在命令里锁死具体版本时才允许写死（npx pkg@x.y.z）。像 OpenCode 这种用
  // 本机安装的客户端，版本随安装目录变，写死一个假版本只会在界面上骗人。
  if (preset.version) {
    assert.ok(preset.command.includes(preset.version), `预设版本与命令不一致：${preset.providerId}`);
  }
  assert.ok(preset.requires.trim(), '预设必须标注前置命令');
  assert.ok(preset.requiresLabel.trim(), '预设必须标注前置命令的展示名');
  const tokens = splitCommandLine(preset.command);
  assert.ok(tokens && tokens.length >= 1, `预设命令无法解析：${preset.command}`);
  assert.ok(tokens[0].trim(), `预设命令缺少可执行文件：${preset.command}`);
}

// 「未安装」是误报重灾区：OpenCode 桌面版自带 CLI 但不进 PATH，探测到安装位置时
// 必须改用绝对路径接入，否则用户明明装了却点不动按钮。
const opencodePreset = acpPresets.find(preset => preset.providerId === 'opencode')!;
assert.equal(presetCommand(opencodePreset, {}), 'opencode acp');
assert.equal(presetCommand(opencodePreset, { opencode: { available: true, path: 'C:\\Tools\\opencode.exe', source: 'path' } }), 'opencode acp');
assert.equal(
  presetCommand(opencodePreset, { opencode: { available: false, path: '', source: 'path', config_dir: 'C:\\Users\\me\\.config\\opencode' } }),
  'opencode acp',
);
const desktopCli = 'C:\\Users\\me\\AppData\\Roaming\\ai.opencode.desktop\\cli\\2.0.20\\opencode-cli.exe';
assert.equal(
  presetCommand(opencodePreset, { opencode: { available: true, path: desktopCli, version: '2.0.20', source: 'install_location' } }),
  `${desktopCli} acp`,
);
// 路径含空格才加引号；加完必须还能解析回来，否则保存的就是半条命令。
const spacedCli = 'C:\\Program Files\\OpenCode\\opencode-cli.exe';
const spacedCommand = presetCommand(opencodePreset, { opencode: { available: true, path: spacedCli, source: 'install_location' } });
assert.deepEqual(splitCommandLine(spacedCommand), [spacedCli, 'acp']);
// 探测到的真实版本优先，预设不再写死版本号。
assert.equal(presetVersion(opencodePreset, { opencode: { available: true, path: desktopCli, version: '2.0.20', source: 'install_location' } }), '2.0.20');
assert.equal(presetVersion(opencodePreset, {}), opencodePreset.version);
assert.equal(presetVersion(acpPresets[0], {}), '1.12.0');

console.log('acp profile checks passed（命令解析 / ID 派生 / 状态判读 / 4 个预设）');
