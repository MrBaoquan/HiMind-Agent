const clientLabels: Record<string, string> = {
  vscode: 'VS Code',
  'cc-switch': 'CC Switch',
  codex: 'Codex',
  workbuddy: 'WorkBuddy',
  'kimi-code': 'Kimi Code',
  'qwen-code': 'Qwen Code',
  'claude-code': 'Claude Code',
  'claude-desktop': 'Claude Desktop',
  opencode: 'OpenCode',
  continue: 'Continue',
  aider: 'Aider',
  crush: 'Crush',
  qoder: 'Qoder',
  'qoder-cn': 'Qoder CN',
  zcode: 'ZCode',
};

/** Shared display names keep the shell lightweight and avoid loading the full AI service page. */
export function clientLabel(target: string) {
  return clientLabels[target] ?? target;
}
