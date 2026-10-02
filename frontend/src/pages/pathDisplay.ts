/**
 * 卡片/列表里显示目录时保留最后两级：`…\项目看板\himind-agent`。
 * 路径从头部截断只会剩下 `F:\WebProjects\…` 这种没有信息量的前缀，
 * 尾部的目录名才认得出是哪个项目。完整路径仍然放在 title 里，悬停即可核对。
 */
export function tailPath(path: string, keep = 2) {
  const trimmed = path.trim();
  const segments = trimmed.split(/[\\/]/).filter(Boolean);
  if (segments.length <= keep + 1) return trimmed;
  const separator = trimmed.includes('\\') ? '\\' : '/';
  return `…${separator}${segments.slice(-keep).join(separator)}`;
}
