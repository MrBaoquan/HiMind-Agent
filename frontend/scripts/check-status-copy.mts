// User-facing extension status vocabulary and priority invariants.
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import {
  VERSION_CURRENT_LABEL,
  extensionStatus,
  primaryExtensionStatus,
} from '../src/data/statusPresentation.ts';

assert.equal(extensionStatus('update').label, '可更新');
assert.equal(extensionStatus('installed').label, '已安装');
assert.equal(extensionStatus('disabled').label, '已停用');
assert.equal(extensionStatus('error').label, '运行异常');
assert.equal(extensionStatus('built').label, '已构建');
assert.equal(extensionStatus('unbuilt').label, '未构建');
assert.equal(VERSION_CURRENT_LABEL, '当前');
assert.equal(primaryExtensionStatus(['installed', 'update'])?.key, 'update',
  '可更新必须覆盖已安装，避免同一对象显示两个主状态');
assert.equal(primaryExtensionStatus(['update', 'disabled'])?.key, 'disabled',
  '停用必须优先于可更新');
assert.equal(primaryExtensionStatus(['disabled', 'error'])?.key, 'error',
  '运行异常必须优先于停用');
assert.equal(primaryExtensionStatus(['installed'])?.label, '已安装');

const market = readFileSync(new URL('../src/pages/ExtensionsPage.tsx', import.meta.url), 'utf8');
assert.match(market, /return extensionStatus\('update'\)/, '市场列表必须使用统一的可更新状态');
assert.doesNotMatch(market, /if \(entry\.installedVersion\) return extensionStatus\('installed'\)/,
  '市场列表不应把默认已安装态渲染成每行角标');
assert.doesNotMatch(market, /return \{ label: entry\.kind === 'instruction' \? '可导入' : '未安装'/,
  '市场列表不能把未安装默认态渲染成每行角标');
assert.doesNotMatch(market, /本机已安装/, '版本区必须使用统一的“当前”，不再使用同义词');
assert.doesNotMatch(market, />已是最新</, '已安装且最新是默认态，不再额外显示');
assert.match(market, /VERSION_CURRENT_LABEL/, '版本行必须使用统一的当前版本标记');

const plugins = readFileSync(new URL('../src/pages/PluginsPage.tsx', import.meta.url), 'utf8');
assert.match(plugins, /primaryExtensionStatus\(statusKeys\)/, '插件详情必须使用统一状态优先级');
assert.doesNotMatch(plugins, /item\.enabled \? '已安装'/, '插件启用态不能写成已安装');
assert.match(plugins, /VERSION_CURRENT_LABEL/, '插件版本行必须使用当前标记');
assert.doesNotMatch(plugins, /插件可用<\/span>/, '默认健康的插件状态不应常驻占位');
assert.doesNotMatch(plugins, /已安装的插件都是最新版/, '可更新筛选空态应直接说明没有可更新项；不重复“已安装/最新”');

const skills = readFileSync(new URL('../src/pages/SkillsWorkspacePage.tsx', import.meta.url), 'utf8');
assert.match(skills, /function skillStatePresentation/, '技能状态必须走统一展示函数');
assert.match(skills, /if \(item\.client_state === 'outdated'\) keys\.push\('update'\)/, '技能更新必须映射为可更新');
assert.match(skills, /VERSION_CURRENT_LABEL/, '技能版本行必须使用当前标记');
assert.doesNotMatch(skills, /clientStateLabel|statePill/, '旧的逐字面状态映射应已移除');
assert.doesNotMatch(skills, /<strong>已安装<\/strong>/, '我的能力技能列表不应重复默认安装态');
assert.doesNotMatch(skills, /待更新/, '技能同步进度应使用统一术语“可更新”');
assert.doesNotMatch(skills, /`已装 v\$\{/, '技能依赖版本行应使用统一的“当前”标记');

const workflows = readFileSync(new URL('../src/pages/WorkflowsPage.tsx', import.meta.url), 'utf8');
assert.doesNotMatch(workflows, /selectedWorkflow\.enabled \? 'success' : 'neutral'/,
  '工作流详情不应为默认启用态重复渲染状态胶囊');
assert.match(workflows, /extensionStatus\('disabled'\)\.label/,
  '工作流停用态必须复用统一状态词汇');

const development = readFileSync(new URL('../src/pages/ExtensionDevelopmentPage.tsx', import.meta.url), 'utf8');
assert.doesNotMatch(development, /<h3>\{project\.name\}<\/h3><Pill/,
  '扩展开发详情标题不应重复列表中的构建/发布状态');
assert.doesNotMatch(development, /\{active \? <small>\{submissionStatus\(active\)\.label\}/,
  '扩展开发列表不应同时显示提交状态行和状态角标');
assert.match(development, /state\.label !== '已构建'/,
  '扩展开发默认“已构建”态应在列表中静默，只保留例外状态');

const sources = readFileSync(new URL('../src/components/ExtensionSourcesDialog.tsx', import.meta.url), 'utf8');
assert.doesNotMatch(sources, /text: statuses\.get\(ready\.id\)\?\.using_cache \? '缓存可用' : '可用'/,
  '来源正常可用是默认态，不应在每张卡片重复文字');
assert.doesNotMatch(sources, /待更新/, '来源管理应使用统一术语“可更新”');
assert.match(sources, /extensionStatus\('update'\)\.label/,
  '来源更新的数量与提示必须复用统一状态词汇');

const mcpConnections = readFileSync(new URL('../src/components/McpConnectionsPanel.tsx', import.meta.url), 'utf8');
assert.doesNotMatch(mcpConnections, /server\.enabled \? '已启用' : '已停用'/,
  'MCP 默认启用态应由开关表达，不再重复状态胶囊');
console.log('status copy checks passed');