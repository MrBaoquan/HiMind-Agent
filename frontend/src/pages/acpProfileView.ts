// AI 客户端（ACP）面板的纯逻辑：ID 派生、状态判读、预设清单。
// 命令行解析和 ACP 无关，放在 lib/commandLine.ts，这里转出去给面板和自检用。
// 这里刻意不依赖 tauri / react，方便 check-acp-profiles.mts 直接跑断言。

import { executableName, formatCommand, slugify, splitCommandLine } from '../lib/commandLine.ts';

// 转出去给面板和 check-acp-profiles.mts 用；上面那两个还要在本文件里计算 ID，所以单独 import。
export { executableName, formatCommand, slugify, splitCommandLine } from '../lib/commandLine.ts';

export type AcpPermissionPolicy = 'deny' | 'allow_once' | 'prompt';

export type AcpPermissionPolicyValue = AcpPermissionPolicy | string;

export type AcpPreset = {
  providerId: string;
  name: string;
  summary: string;
  command: string;
  version: string;
  permissionPolicy: AcpPermissionPolicy;
  requires: string;
  requiresLabel: string;
};

export type AcpProfileLike = {
  provider_id: string;
  display_name: string;
  executable: string;
  args: string[];
  version: string;
  permission_policy: AcpPermissionPolicyValue;
  enabled: boolean;
};

export type AcpProviderLike = {
  provider: string;
  status: string;
  capabilities?: Record<string, unknown>;
};

export type AcpProfileStatusTone = 'success' | 'danger' | 'neutral';

export type AcpProfileStatus = {
  tone: AcpProfileStatusTone;
  label: string;
  reason: string;
};

/** 后端 `probe_executables` 的一条结果：前置命令在不在、在哪、什么版本。 */
export type AcpExecutableReport = {
  available?: boolean;
  path?: string;
  version?: string;
  /** `path` = PATH 命中；`install_location` = 只在已知安装位置找到。 */
  source?: string;
  /** 配置目录存在但命令不在 PATH 时的旁证，只用于文案。 */
  config_dir?: string;
};

export type AcpExecutableReports = Record<string, AcpExecutableReport | undefined>;

const PROVIDER_ID_PATTERN = /^[A-Za-z0-9._-]{1,64}$/;

// 预设写死命令、版本和权限策略，用户只需要点一下「接入」。
export const acpPresets: AcpPreset[] = [
  {
    providerId: 'codex',
    name: 'Codex',
    summary: '接入 Codex，作为工作流的 AI 步骤执行。',
    command: 'npx -y @agentclientprotocol/codex-acp@1.12.0',
    version: '1.12.0',
    permissionPolicy: 'prompt',
    requires: 'npx',
    requiresLabel: 'Node.js（npx）',
  },
  {
    providerId: 'claude',
    name: 'Claude Agent',
    summary: '复用本机 Claude 的登录状态执行 AI 步骤。',
    command: 'npx -y @agentclientprotocol/claude-agent-acp@0.78.0',
    version: '0.78.0',
    permissionPolicy: 'prompt',
    requires: 'npx',
    requiresLabel: 'Node.js（npx）',
  },
  {
    providerId: 'opencode',
    name: 'OpenCode',
    summary: '接入本机安装的 OpenCode。',
    command: 'opencode acp',
    // 这里刻意不写死版本号：OpenCode 桌面版自带 CLI 的版本随安装目录变，
    // 写死的版本号只会在界面上骗人，真实版本由探测结果回填。
    version: '',
    permissionPolicy: 'prompt',
    requires: 'opencode',
    requiresLabel: 'OpenCode',
  },
  {
    providerId: 'github-copilot',
    name: 'GitHub Copilot',
    summary: '接入 GitHub Copilot CLI。',
    command: 'npx -y @github/copilot@1.0.83 --acp',
    version: '1.0.83',
    permissionPolicy: 'prompt',
    requires: 'npx',
    requiresLabel: 'Node.js（npx）',
  },
];

export const acpPolicyOptions: Array<{ value: AcpPermissionPolicy; label: string; description: string }> = [
  { value: 'prompt', label: '每次询问', description: 'Agent 需要授权时，在审批中心确认后继续。' },
  { value: 'deny', label: '不授权', description: '拒绝需要授权的操作，Agent 只做只读工作。' },
  { value: 'allow_once', label: '自动授权', description: '不询问，直接放行 Agent 的操作。' },
];

export function bareProviderId(value: string): string {
  return value.trim().replace(/^acp\./i, '');
}

/**
 * 算出一个存得下去的客户端 ID。
 * 顺序：手填 ID → 名称 → 可执行文件名。新建时统一小写，和预设、工作流侧的品牌识别
 * 一致（工作流按 `provider.includes('codex')` 这类小写子串判断）；编辑时沿用已存 ID，
 * 免得只改了个大小写就变成另一个客户端。
 */
export function resolveProviderId(input: { mode: 'create' | 'edit'; typedId: string; displayName: string; command: string }): string {
  const raw = bareProviderId(input.typedId) || slugify(input.displayName) || slugify(executableName(input.command));
  return input.mode === 'edit' ? raw : raw.toLowerCase();
}

export function validProviderId(value: string): boolean {
  return PROVIDER_ID_PATTERN.test(value);
}

/**
 * 预设的启动命令默认只写命令名（`opencode acp`），交给 PATH 解析——这样用户切换
 * Node / OpenCode 版本后配置依然有效。只有当命令名不在 PATH、却在本机已知安装位置
 * 找到时（桌面版自带 CLI 就是这种情况），才把绝对路径写进配置，否则用户明明装了
 * OpenCode，界面却只给一句「未安装」。
 */
export function presetCommand(preset: AcpPreset, executables: AcpExecutableReports = {}): string {
  const report = preset.requires ? executables[preset.requires] : undefined;
  const resolved = report?.source === 'install_location' ? (report.path ?? '').trim() : '';
  if (!resolved) return preset.command;
  const tokens = splitCommandLine(preset.command) ?? [];
  return formatCommand(resolved, tokens.slice(1));
}

/** 版本号的口径：探测到的真实版本优先，预设里写死的版本兜底。 */
export function presetVersion(preset: AcpPreset, executables: AcpExecutableReports = {}): string {
  const report = preset.requires ? executables[preset.requires] : undefined;
  return (report?.version ?? '').trim() || preset.version;
}

/** 编辑态的权限策略必须是后端认识的三个值，脏数据一律退回 deny。 */
export function normalizePolicy(value: AcpPermissionPolicyValue): AcpPermissionPolicy {
  return value === 'allow_once' ? 'allow_once' : value === 'prompt' ? 'prompt' : 'deny';
}

function stringValue(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

/**
 * 列表状态判读。「不可用」必须带得出原因，否则用户只会看到一个红色标签。
 * tone 对应 Pill 的 success / danger / neutral。
 */
export function acpProfileStatus(profile: AcpProfileLike, provider?: AcpProviderLike): AcpProfileStatus {
  if (!profile.enabled) return { tone: 'neutral', label: '已停用', reason: '' };
  if (provider?.status === 'ready') return { tone: 'success', label: '已就绪', reason: '' };
  if (provider?.status === 'unsupported') {
    return {
      tone: 'danger',
      label: '配置异常',
      reason: stringValue(provider.capabilities?.error) || '该客户端不被支持，请检查启动命令。',
    };
  }
  const capabilities = provider?.capabilities;
  if (capabilities?.executable_available === false) {
    return { tone: 'danger', label: '不可用', reason: `未找到命令 ${profile.executable}，请点「编辑」填写它的完整路径。` };
  }
  if (capabilities?.permission_policy_valid === false) {
    return { tone: 'danger', label: '不可用', reason: '权限策略无效，请重新选择。' };
  }
  return {
    tone: 'danger',
    label: '不可用',
    reason: stringValue(capabilities?.executable_error) || '启动命令不可用，请检查路径和参数。',
  };
}
