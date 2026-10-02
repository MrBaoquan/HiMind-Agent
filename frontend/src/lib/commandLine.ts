// 命令行解析（零依赖）。ACP 客户端和 MCP 工具连接都要「一行启动命令 ⇄
// 可执行文件 + 参数」，两边各写一份迟早会分叉，所以放这里共用。
// 这里刻意不依赖 tauri / react，方便 check-*.mts 直接跑断言。

/** 把一行命令按空白切分，双引号内的空格保留；引号未闭合返回 null。 */
export function splitCommandLine(value: string): string[] | null {
  const tokens: string[] = [];
  let current = '';
  let quoted = false;
  let started = false;
  for (const char of value.trim()) {
    if (char === '"') {
      quoted = !quoted;
      started = true;
      continue;
    }
    if (!quoted && /\s/.test(char)) {
      if (started) {
        tokens.push(current);
        current = '';
        started = false;
      }
      continue;
    }
    current += char;
    started = true;
  }
  if (quoted) return null;
  if (started) tokens.push(current);
  return tokens;
}

function quoteToken(token: string): string {
  return /\s/.test(token) ? `"${token}"` : token;
}

/** 把可执行文件 + 参数拼回一行命令，便于回填编辑框。 */
export function formatCommand(executable: string, args: string[]): string {
  return [executable, ...args].filter(token => token !== '').map(quoteToken).join(' ');
}

/** 取命令的第一个 token 的文件名，用于兜底生成 ID。 */
export function executableName(command: string): string {
  const first = (splitCommandLine(command) ?? [])[0] ?? '';
  return first.replace(/\\/g, '/').split('/').pop() ?? first;
}

export function slugify(value: string): string {
  return value.trim().toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^[-.]+|[-.]+$/g, '');
}
