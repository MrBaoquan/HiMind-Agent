// Mission assignment UI regression checks. These are intentionally dependency-free
// source assertions so the guard can run before a browser harness is available.
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assignmentProblem, bindExpert, changeExecutor, changeService, serviceOptions } from '../src/pages/missionAssignment.ts';
import type { ExpertSummary, MissionAssignmentOptions, MissionExecutionOption, MissionRoleAssignment } from '../src/services/agentApi';

const here = dirname(fileURLToPath(import.meta.url));
const source = (path: string) => readFileSync(join(here, '..', path), 'utf8');
const dialog = source('src/pages/CollaborationDialog.tsx');
const panel = source('src/pages/MissionTeamPanel.tsx');
const taskCenter = source('src/pages/TaskCenterPage.tsx');
const assignmentStart = panel.indexOf('function CollaborationAssignmentDialog');
const assignmentEnd = panel.indexOf('function CollaborationAuthorizationPanel', assignmentStart);
assert.ok(assignmentStart >= 0 && assignmentEnd > assignmentStart, '指派实现必须是独立的 CollaborationAssignmentDialog');
const assignment = panel.slice(assignmentStart, assignmentEnd);

assert.ok(/createPortal/.test(dialog), '指派弹窗必须通过 portal 挂载 document.body');
assert.ok(/showModal\(\)/.test(dialog), '指派弹窗必须使用原生 dialog.showModal()');
assert.ok(/onCancel=/.test(dialog) && /focus\(\)/.test(dialog), 'Escape 关闭和焦点恢复必须接线');
assert.ok(/<CollaborationDialog/.test(assignment), '指派表单必须包在真正的模态弹窗中');
assert.ok(/options\.experts/.test(assignment), '职责必须选择已安装 Expert');
assert.ok(/options\.executors/.test(assignment), '执行器必须来自宿主目录');
assert.ok(/serviceOptions\(/.test(assignment) && /selectedExecutor\?\.models/.test(assignment), '服务和模型必须根据执行器目录联动');
assert.ok(/<select/.test(assignment), '职责绑定必须使用选择器');
assert.ok(!/<input\b/.test(assignment), '执行器、服务、模型和 Expert 指派区不得使用自由文本输入');
assert.ok(/assignmentProblem\(/.test(assignment) && /disabled=\{[^}]*problems\.length/.test(assignment), '无效目录绑定必须阻止提交');
assert.ok(/使用推荐服务与模型/.test(assignment), '必须保留一键应用推荐配置');
assert.ok(/任务与职责映射/.test(assignment), '每项计划任务必须能选择承担职责');

const creationStart = panel.indexOf('function CreateMissionDialog');
const creationEnd = panel.indexOf('function TaskExecutionEditor', creationStart);
const creation = panel.slice(creationStart, creationEnd);
assert.ok(/createMissionHandoff/.test(creation), 'Agent 新建必须走协作任务交接入口');
assert.ok(/workflowId/.test(creation) && /请选择已安装工作流/.test(creation), '新建协作任务必须固定 Workflow');
assert.ok(/<CollaborationDialog/.test(creation), '新建协作任务必须使用真正模态弹窗');

assert.ok(/openAssignmentRevision=\{missionNavigationRevision\}/.test(taskCenter), 'Codex 深链 revision 必须传给协作任务详情');
assert.ok(!/CollaborationAssignmentPanel/.test(panel), '详情页不得保留旧的内嵌指派面板');
console.log('mission assignment UI checks passed（专家目录 / 执行目录联动 / 原生模态 / 深链重开）');

const expert = { id: 'com.himind.expert.test', version: '1.0.0', digest: 'sha256:pin' } as ExpertSummary;
const executor: MissionExecutionOption = {
  executor: 'himind.builtin', label: 'HiMind AI', available: true, reason: '',
  models: [
    {alias:'himind:a:one', label:'one', service_id:'a', service_label:'A'},
    {alias:'himind:a:two', label:'two', service_id:'a', service_label:'A'},
    {alias:'himind:b:one', label:'one', service_id:'b', service_label:'B'},
  ],
};
const client: MissionExecutionOption = {executor:'personal.codex', label:'Codex', available:true, reason:'', models:[]};
const options = {experts:[expert], executors:[executor, client]} as MissionAssignmentOptions;
const role: MissionRoleAssignment = bindExpert({role_id:'reviewer', label:'Review', executor:'himind.builtin', service_id:'a', model:'himind:a:two'}, expert);
assert.equal(assignmentProblem(role, options), '');
assert.deepEqual(serviceOptions(executor).map(item => item.id), ['a', 'b']);
const switched = changeService(role, executor, 'b');
assert.equal(switched.model, 'himind:b:one');
assert.equal(assignmentProblem(switched, options), '');
assert.ok(assignmentProblem({...role, service_id:'b'}, options));
assert.ok(assignmentProblem({...role, expert:null}, options));
assert.ok(assignmentProblem({...role, expert:{...role.expert!, expert_digest:'drift'}}, options));
const onClient = changeExecutor(role, client);
assert.equal(onClient.model, ''); assert.equal(onClient.service_id, '');
assert.deepEqual(onClient.expert, role.expert);
assert.equal(assignmentProblem(onClient, options), '');
assert.ok(assignmentProblem({...role, model:'', service_id:''}, {...options, executors:[{...executor, models:[]}]}));
console.log('mission directory behavior checks passed');
