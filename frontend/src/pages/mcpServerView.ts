// 工具连接（MCP）面板的纯逻辑：服务 ID 派生、启动命令解析、表单校验。
// 可安装清单不在这里 —— 它来自 MCP 目录（server.json），见 mcpCatalogView.ts。
// 校验规则对齐后端 src/app/mcp_settings.rs 的 validate，能在界面上拦住的就别等保存才报错。
// 这里刻意不依赖 tauri / react，方便 check-mcp-servers.mts 直接跑断言。
import { executableName, slugify, splitCommandLine } from '../lib/commandLine.ts';

export type McpServerLike = {
  server_name: string;
  display_name: string;
  transport: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  cwd: string;
  url: string;
  headers: Record<string, string>;
  tool_call_timeout_ms: number;
  fail_on_startup_error: boolean;
  reconnect: boolean;
  enabled: boolean;
};

const SERVER_NAME_PATTERN = /^[A-Za-z0-9_-]{1,32}$/;
const RESERVED_SERVER_NAMES = ['himind-agent', 'himind'];
const HTTP_PATTERN = /^https?:\/\/\S+$/i;

export function isReservedServerName(value: string): boolean {
  const name = value.trim().toLowerCase();
  return RESERVED_SERVER_NAMES.includes(name);
}

export function validServerName(value: string): boolean {
  return SERVER_NAME_PATTERN.test(value.trim());
}

/**
 * 算出一个存得下去的服务 ID。顺序：手填 ID → 名称 → 可执行文件名 → 兜底。
 * 后端只认 ASCII 字母数字和 `_`、`-`，所以中文名称会滑到下一档，用户可以在高级设置里改。
 */
export function resolveServerName(input: { mode: 'create' | 'edit'; typedName: string; displayName: string; command: string }): string {
  // 手填的 ID 原样保留，交给校验给出明确提示；派生出来的必须自己洗干净，
  // 否则 `node.exe` 这种可执行文件名会带着点号提交，被后端判成非法服务名。
  const typed = input.typedName.trim();
  const raw = typed || serverId(input.displayName) || serverId(executableName(input.command)) || 'mcp-server';
  const trimmed = raw.slice(0, 32);
  return input.mode === 'edit' ? trimmed : trimmed.toLowerCase();
}

/** 派生服务 ID 专用：slug 之后再去掉点号，让它落在后端的合法字符集里。 */
function serverId(value: string): string {
  return slugify(value).replace(/\.+/g, '-').replace(/^[-_]+|[-_]+$/g, '');
}

export type ParsedCommand = { ok: true; executable: string; args: string[] } | { ok: false; error: string };

/** 一行启动命令拆成可执行文件 + 参数，顺带给出能直接显示的错误。 */
export function parseCommand(command: string): ParsedCommand {
  const tokens = splitCommandLine(command);
  if (!tokens) return { ok: false, error: '启动命令格式不对，请检查双引号是否配对。' };
  if (!tokens.length) return { ok: false, error: '请填写启动命令。' };
  return { ok: true, executable: tokens[0], args: tokens.slice(1) };
}

function invalidMapKey(values: Record<string, string>): string {
  return Object.keys(values).find(key => !key.trim() || /[=\0\r\n]/.test(key)) ?? '';
}

/** 返回空字符串表示可以保存；否则是给用户看的一句话。 */
export function validateMcpServer(input: { serverName: string; draft: McpServerLike; existingNames: string[] }): string {
  const name = input.serverName.trim();
  if (!validServerName(name)) return '服务 ID 只能包含字母、数字、下划线和短横线，且不超过 32 个字符。';
  if (isReservedServerName(name)) return 'himind-agent 是内置服务名，请换一个。';
  if (input.existingNames.some(existing => existing.trim().toLowerCase() === name.toLowerCase())) {
    return `已存在服务 ID「${name}」，请在高级设置里换一个。`;
  }
  const envKey = invalidMapKey(input.draft.env);
  if (envKey) return `环境变量名「${envKey}」不合法。`;
  const headerKey = invalidMapKey(input.draft.headers);
  if (headerKey) return `请求头名称「${headerKey}」不合法。`;
  if (input.draft.transport === 'stdio') {
    if (!input.draft.command.trim()) return '请填写启动命令。';
  } else if (!HTTP_PATTERN.test(input.draft.url.trim())) {
    return '服务地址需要以 http:// 或 https:// 开头。';
  }
  if (input.draft.tool_call_timeout_ms <= 0) return '工具调用超时至少 1 秒。';
  if (input.draft.tool_call_timeout_ms > 10 * 60 * 1000) return '工具调用超时不能超过 10 分钟。';
  return '';
}

export function transportLabel(transport: string): string {
  return transport === 'stdio' ? '本地进程' : 'HTTP';
}
