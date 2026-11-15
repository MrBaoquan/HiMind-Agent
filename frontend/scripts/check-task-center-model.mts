import assert from 'node:assert/strict';
import {projectTasks, executionTodoId, missionTodo, activityNeedsUser, workflowNeedsUser, workspaceKey} from '../src/pages/taskCenterModel.ts';
import {missionStage, missionProgress, missionNextAction, missionFlow, missionVerificationExplanation, missionStatusLabel, MISSION_TASK_STATUS_LABELS} from '../src/pages/missionView.ts';
import type {AgentActivityItem, MissionView, WorkflowCenterSnapshot} from '../src/services/agentApi';
const mission = (status='active', extra={}) => ({mission_id:'m1', title:'修复搜索', objective:'修复中文搜索', status,
 workspace:{root:'F:/project'}, tasks:[], plan_revision:1, plan_sha256:'hash', created_at:'2026-11-11', updated_at:'2026-11-11', ...extra}) as MissionView;
const run = (id:string, extra={}) => ({id, workflow_run_id:id, source:'workflow', title:id,status:'running',progress:null,
 created_at:'2026-11-11',updated_at:'2026-11-11',...extra}) as AgentActivityItem;
const m=mission('active',{tasks:[{attempt_run_id:'bound', status:'running'}]});
const records=[run('bound'),run('verify',{mission_id:'m1',mission_task_id:'t1'}),run('retry',{parent_run_id:'bound',parent_kind:'retry'}),
 run('root'),run('child',{parent_run_id:'root'}),run('grandchild',{parent_run_id:'child'}),
 run('orphan',{parent_run_id:'not-loaded'}),run('unknown',{mission_id:'not-loaded'}),
 run('looks_like_mission_m1'),run('cycle-a',{parent_run_id:'cycle-b'}),run('cycle-b',{parent_run_id:'cycle-a'})];
const projection=projectTasks([m],records);
assert.equal(projection.rows.length,7);
assert.deepEqual(projection.related.get('mission:m1')?.map(i=>i.id),['bound','verify','retry']);
assert.deepEqual(projection.related.get('activity:root')?.map(i=>i.id),['child','grandchild']);
for(const id of ['orphan','unknown','looks_like_mission_m1','cycle-a','cycle-b']) assert.ok(projection.rows.some(row=>row.id==='activity:'+id));
assert.equal(projectTasks([], [run('orphan',{parent_run_id:'missing'})]).rows.length,1);
for(const status of ['completed','failed','canceled']) {
 const row=projectTasks([mission(status)],[]).rows[0];
 assert.equal(row.ended,true);assert.equal(row.needsUser,false);assert.equal(missionTodo(mission(status)),null);
 assert.equal(missionStage(mission(status)),status);
}
for(const status of ['draft','planned','awaiting_assignment','awaiting_confirmation']) {
 assert.ok(missionTodo(mission(status)));assert.equal(projectTasks([mission(status)],[]).rows[0].needsUser,true);
}
assert.notEqual(missionTodo(mission('awaiting_confirmation'))?.id,missionTodo(mission('awaiting_confirmation',{plan_sha256:'changed'}))?.id);
assert.equal(missionTodo(mission('active')),null);
assert.equal(missionTodo(mission('waiting')),null);
assert.equal(missionStage(mission('active',{tasks:[{status:'awaiting_verification'},{status:'running'}]})),'execute');
assert.equal(missionStage(mission('active',{tasks:[{status:'awaiting_verification'},{status:'blocked'}]})),'verify');
assert.equal(missionStage(mission('active',{tasks:[{status:'awaiting_verification'},{status:'completed'}]})),'verify');
assert.deepEqual(missionProgress(mission()),{done:0,total:0,running:0,awaitingVerification:0,startedVerification:0,ready:0,blocked:0,failed:0});
assert.equal(MISSION_TASK_STATUS_LABELS.awaiting_verification,'待另一位专家核验');
const pendingVerification=mission('active',{tasks:[{status:'awaiting_verification',verification_run_id:'',verification_passed:null}]});
assert.equal(missionNextAction(pendingVerification),'1 项结果待核验');
assert.equal(missionStatusLabel(pendingVerification),'待核验');
const preparingEvent=[{event_type:'verification_preparing'} as never];
assert.equal(missionStatusLabel(pendingVerification,preparingEvent),'系统检查中');
assert.equal(missionNextAction(pendingVerification,preparingEvent),'系统正在运行必需检查');
const supervisorError=[{event_type:'mission_supervisor_error',summary:'文件检查失败'} as never];
assert.equal(missionStatusLabel(pendingVerification,supervisorError),'暂时无法推进');
assert.equal(missionVerificationExplanation(pendingVerification,supervisorError),'文件检查失败');
assert.equal(missionStatusLabel(pendingVerification,[...supervisorError,...preparingEvent]),'系统检查中');
assert.match(missionVerificationExplanation(pendingVerification),/系统将运行必需检查/);
assert.match(missionVerificationExplanation(pendingVerification),/另一位专家核对/);
assert.equal(missionFlow(pendingVerification)[3].state,'current');
const activeVerification=mission('active',{tasks:[{status:'awaiting_verification',verification_run_id:'run-verify',verification_passed:null}]});
assert.equal(missionNextAction(activeVerification),'1 项由另一位专家核验中');
assert.equal(missionStatusLabel(activeVerification),'核验进行中');
assert.match(missionVerificationExplanation(activeVerification),/另一位专家核对/);
assert.equal(missionFlow(mission('waiting',{tasks:[{status:'awaiting_verification',verification_run_id:''}]}))[3].state,'current');
assert.equal(missionStatusLabel(mission('waiting'),[{event_type:'mission_pause'} as never]),'已暂停');
assert.equal(missionStatusLabel(mission('waiting'),[{event_type:'mission_waiting'} as never]),'等待处理');
const budgetExhaustedEvent=[{event_type:'budget_exhausted'} as never];
assert.equal(missionStatusLabel(mission('waiting'),budgetExhaustedEvent),'预算已用尽');
assert.equal(missionNextAction(mission('waiting'),budgetExhaustedEvent),'增加预算并继续');
assert.equal(missionFlow(mission('active',{tasks:[{status:'awaiting_verification'},{status:'running'}]}))[2].state,'current');
const stalled=mission('active',{tasks:[{status:'failed',verification_passed:false,spec:{id:'fix',repair_of:''}}, {status:'blocked',spec:{id:'review',repair_of:''}}]});
assert.equal(missionStatusLabel(stalled),'任务已停下，需处理失败');
assert.match(missionNextAction(stalled),/任务已停下|返工次数已用完/);
assert.equal(missionFlow(stalled)[3].state,'current');
assert.equal(projectTasks([stalled],[]).rows[0].needsUser,true);
assert.equal(missionTodo(stalled)?.kind,'attention');
assert.ok(!missionNextAction(mission('completed')).includes('沉淀'));
for(const kind of ['external','user','','unknown']) assert.equal(activityNeedsUser(run('r',{status:'waiting',pause_kind:kind})),false);
for(const kind of ['approval','feedback','form','evidence']) assert.equal(activityNeedsUser(run('r',{status:'waiting',pause_kind:kind})),true);
const waiting=(kind:string)=>({run:{status:'waiting'},waiting_kind:kind}) as WorkflowCenterSnapshot['runs'][number];
assert.equal(workflowNeedsUser(waiting('external_wait')),false);
assert.equal(workflowNeedsUser(waiting('')),false);
assert.equal(workflowNeedsUser(waiting('feedback')),true);
assert.equal(workflowNeedsUser({...waiting('feedback'),interaction_request:{kind:'external_wait'}} as WorkflowCenterSnapshot['runs'][number]),false);
const userRun=run('approval',{mission_id:'m1',status:'waiting',pause_kind:'approval'});
assert.equal(projectTasks([mission()], [userRun]).rows[0].needsUser,true);
assert.equal(projectTasks([mission()], [{...userRun,status:'completed'}]).rows[0].needsUser,false);
assert.equal(workspaceKey('F:/Project/'),workspaceKey('f:/project'));
assert.notEqual(workspaceKey('/Case'),workspaceKey('/case'));
console.log('task center model checks passed: ownership, cycles, views, user actions, lifecycle, stages, plan identity');


assert.equal(projectTasks([mission('canceled')], [userRun]).rows[0].needsUser,false);
assert.equal(projectTasks([], [run('root',{status:'canceled'}),run('child',{parent_run_id:'root',status:'waiting',pause_kind:'approval'})]).rows[0].needsUser,false);
assert.equal(executionTodoId('r1','s1','feedback'),'run:r1:s1:feedback');
