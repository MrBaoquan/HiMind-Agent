/** Keep diagnostic text available without making it the library row label. */
export function workflowIssuePresentation(message: string) {
  if (/content changed/i.test(message)) return {
    reason: '依赖插件内容已变化',
    recovery: '检查已安装插件，再更新或重新安装工作流。',
  };
  if (/unavailable Plugin|plugin returned empty response/i.test(message)) return {
    reason: '依赖插件未响应',
    recovery: '检查依赖插件是否可用，再刷新工作流。',
  };
  return {
    reason: '工作流配置无法读取',
    recovery: '查看诊断信息，检查插件与工作流版本。',
  };
}
