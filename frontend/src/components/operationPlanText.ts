// 计划卡上的文案与语义。
//
// 计划里的落点与依赖都带着后端枚举（scope / strategy / dependency.action），
// 它们不是给人看的：直接把取值渲染出来会漏出 `resolve`、`unavailable` 这类
// 英文单词，而"这一项要不要处理"又必须由取值决定，不能各处各写一遍三元表达式。
// 因此这里集中登记两件事：中文标签，以及这一项处于哪种状态。
//
// 取值来源是 Agent 侧真正会写出来的枚举：
//   · 技能 / 插件依赖：install / update / satisfied / blocked / unavailable
//     （`src/app/plugin_manager.rs`，技能安装复用同一套解析结果）
//   · 发布计划的依赖项：keep（已锁定精确版本）/ resolve（只有最低版本）
//   · 落点 scope：agent / user / project / organization / remote
//   · 落盘 strategy：store / copy / symlink / extract / release / submit
// 未登记的取值退回原值，方便对照日志排查。

export type DependencyTone = 'ready' | 'pending' | 'blocked';

const DEPENDENCY_ACTION_LABELS: Record<string, string> = {
  install: '将安装',
  update: '将更新',
  satisfied: '已就绪',
  keep: '已就绪',
  resolve: '未锁定版本',
  blocked: '被阻断',
  unavailable: '未上架',
};

const DEPENDENCY_ACTION_TONES: Record<string, DependencyTone> = {
  install: 'pending',
  update: 'pending',
  satisfied: 'ready',
  keep: 'ready',
  resolve: 'pending',
  blocked: 'blocked',
  unavailable: 'blocked',
};

const SCOPE_LABELS: Record<string, string> = {
  agent: '本机 Agent',
  user: '用户目录',
  project: '项目目录',
  organization: '组织',
  remote: '远端',
};

const STRATEGY_LABELS: Record<string, string> = {
  store: '写入技能库',
  copy: '复制',
  symlink: '符号链接',
  extract: '解包',
  release: '发布制品',
  submit: '提交审核',
};

export function scopeLabel(scope: string) {
  return SCOPE_LABELS[scope] || scope;
}

export function strategyLabel(strategy: string) {
  return STRATEGY_LABELS[strategy] || strategy;
}

export function dependencyActionLabel(action: string) {
  return DEPENDENCY_ACTION_LABELS[action] || action;
}

export function dependencyActionTone(action: string): DependencyTone {
  return DEPENDENCY_ACTION_TONES[action] || 'pending';
}

/** 这一项依赖这次要不要动手。`satisfied` 与 `keep` 都表示本机已经满足。 */
export function dependencyNeedsWork(action: string) {
  return dependencyActionTone(action) !== 'ready';
}

/** 依赖已经登记过标签：自检用它保证后端新增取值时界面不会漏出英文枚举。 */
export function dependencyActionKnown(action: string) {
  return Object.prototype.hasOwnProperty.call(DEPENDENCY_ACTION_LABELS, action);
}

export const DEPENDENCY_ACTIONS = Object.keys(DEPENDENCY_ACTION_LABELS);
export const SCOPE_KEYS = Object.keys(SCOPE_LABELS);
export const STRATEGY_KEYS = Object.keys(STRATEGY_LABELS);
