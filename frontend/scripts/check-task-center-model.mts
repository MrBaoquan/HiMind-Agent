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
assert.equal(missionStatusLabel(pendingVerification,preparingEvent),'待核验');
assert.equal(missionNextAction(pendingVerification,preparingEvent),'1 项结果待核验', 'stale preparing event is not a live process');
const supervisorError=[{event_type:'mission_supervisor_error',summary:'文件检查失败'} as never];
assert.equal(missionStatusLabel(pendingVerification,supervisorError),'待核验');
assert.match(missionVerificationExplanation(pendingVerification,supervisorError),/不推断进度/);
assert.equal(missionStatusLabel(pendingVerification,[...supervisorError,...preparingEvent]),'待核验');
assert.match(missionVerificationExplanation(pendingVerification),/系统将运行必需检查/);
assert.match(missionVerificationExplanation(pendingVerification),/另一位专家核对/);
assert.equal(missionFlow(pendingVerification)[3].state,'current');
const activeVerification=mission('active',{tasks:[{status:'awaiting_verification',verification_run_id:'run-verify',verification_passed:null}]});
assert.equal(missionNextAction(activeVerification),'1 项核验任务已派发，等待状态更新');
assert.equal(missionStatusLabel(activeVerification),'核验任务已派发');
assert.match(missionVerificationExplanation(activeVerification),/另一位专家核对/);
assert.equal(missionFlow(mission('waiting',{tasks:[{status:'awaiting_verification',verification_run_id:''}]}))[3].state,'current');
assert.equal(missionStatusLabel(mission('waiting'),[{event_type:'mission_pause'} as never]),'等待原因未确认');
assert.equal(missionStatusLabel(mission('waiting'),[{event_type:'mission_waiting'} as never]),'等待原因未确认');
const budgetExhaustedEvent=[{event_type:'budget_exhausted'} as never];
assert.equal(missionStatusLabel(mission('waiting'),budgetExhaustedEvent),'等待原因未确认');
assert.match(missionNextAction(mission('waiting'),budgetExhaustedEvent),/资格未确认/);
assert.equal(missionFlow(mission('active',{tasks:[{status:'awaiting_verification'},{status:'running'}]}))[2].state,'current');
const stalled=mission('active',{tasks:[{status:'failed',verification_passed:false,spec:{id:'fix',repair_of:''}}, {status:'blocked',spec:{id:'review',repair_of:''}}]});
assert.equal(missionStatusLabel(stalled),'状态待核对');
assert.match(missionNextAction(stalled),/等待系统更新/);
assert.equal(missionFlow(stalled)[2].state,'current');
assert.equal(projectTasks([stalled],[]).rows[0].needsUser,false);
assert.equal(missionTodo(stalled),null, 'coordination gap is not proof that a user action is required');
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

// B08: all surfaces consume the same associated reason, actor, eligibility/cursor.
import {missionWait, missionDeliveries, missionResponsibility, eventLabel} from '../src/pages/missionView.ts';
import {retainBudgetRequest,clearBudgetRequest,pendingBudgetRequest} from '../src/pages/missionBudgetRequest.ts';
const waitDto = {contract_version:'mission.wait.v1',plan_sha256:'hash',cursor:5,reason:'budget_exhausted',actor:'user',budget_reason:'Token 100/100',environment_blocked:false,can_increase_budget:true,can_resume:false};
const waitingBudget=mission('waiting',{confirmation:{plan_sha256:'hash'},effective_wait:waitDto});
for(const telemetry of [[],[{event_type:'budget_exhausted'},{event_type:'task_progress'}],[{event_type:'mission_feedback_submitted'}]]) {
 assert.equal(missionStatusLabel(waitingBudget,telemetry as never),'预算已用尽');
 assert.equal(missionNextAction(waitingBudget,telemetry as never),'增加预算并继续');
}
assert.equal(missionTodo(waitingBudget)?.cursor,5);
assert.equal(missionTodo(waitingBudget)?.reason,'budget_exhausted');
assert.equal(missionTodo(waitingBudget)?.actor,'user');
assert.equal(projectTasks([waitingBudget],[]).rows[0].needsUser,true);
for(const extra of [{effective_wait:null},{effective_wait:{...waitDto,contract_version:'future'}},{effective_wait:{...waitDto,plan_sha256:'other'}},{confirmation:{plan_sha256:'old'}},{effective_wait:{...waitDto,cursor:-1}}]) {
 const unknown=mission('waiting',{...waitingBudget,...extra});
 assert.equal(missionWait(unknown),null);assert.equal(missionTodo(unknown),null);
 assert.equal(missionStatusLabel(unknown),'等待原因未确认');
}
const paused={...waitingBudget,effective_wait:{...waitDto,reason:'manual_pause',can_resume:true}};
assert.equal(missionStatusLabel(paused),'已暂停');assert.equal(missionTodo(paused),null);
assert.match(missionResponsibility(paused),/由你决定/);assert.equal(missionWait(paused)?.can_increase_budget,true);
const system={...waitingBudget,effective_wait:{...waitDto,reason:'system_verification',actor:'system',can_increase_budget:false}};
assert.equal(missionTodo(system),null);assert.match(missionNextAction(system),/不重复执行实现/);
const rootTask=(id:string,status:string,repair_of='')=>({spec:{id,repair_of},status,attempt_run_id:id+'-run',verification_run_id:id+'-verify',evidence_refs:[]});
const repaired=mission('active',{tasks:[rootTask('a','failed'),rootTask('b','blocked'),rootTask('r1','failed','a'),rootTask('r2','completed','r1')]});
assert.equal(missionProgress(repaired).total,2);assert.equal(missionProgress(repaired).done,1);
assert.equal(missionDeliveries(repaired).groups[0].attempts.length,3);
const malformed=mission('active',{tasks:[rootTask('a','failed','b'),rootTask('b','failed','a'),rootTask('missing','failed','absent')]});
assert.equal(missionProgress(malformed).total,0);assert.equal(missionDeliveries(malformed).unassociated.length,3);
const newRevision=mission('active',{tasks:[...repaired.tasks,rootTask('r3','ready','r2')]});
assert.equal(missionProgress(newRevision).done,0, 'old successes cannot complete a newer repair');
const input={expected_plan_sha256:'hash',additional_rounds:0,additional_minutes:0,additional_repair_attempts:0,additional_tokens:50,unlimited_tokens:false,additional_cost:null};
const original=retainBudgetRequest('m1',input);
assert.deepEqual(retainBudgetRequest('m1',input),original);assert.deepEqual(pendingBudgetRequest('m1','hash'),original);
assert.notEqual(retainBudgetRequest('m1',{...input,additional_tokens:60}).request_id,original.request_id);
clearBudgetRequest('m1','hash');assert.equal(pendingBudgetRequest('m1','hash'),null);
assert.match(eventLabel({event_type:'mission_feedback_submitted'} as never),/审查意见/);
assert.match(eventLabel({event_type:'mission_follow_up_format_retry'} as never),/重试/);
console.log('B08/B09 regressions passed: DTO association, telemetry, pause/system/unknown, lineage, immutable retry identity');

// QA-LEGACY-01: authoritative resume admission must agree with the user action.
import {missionResumeAction, missionActionActor} from '../src/pages/missionView.ts';
const legacy = mission('waiting',{confirmation:{plan_sha256:'hash'}, tasks:[rootTask('pending','awaiting_verification')],
 effective_wait:{...waitDto,reason:'system_verification',actor:'system',can_resume:true,can_increase_budget:false}});
assert.equal(missionResumeAction(legacy)?.label,'续跑待核验');
assert.match(missionNextAction(legacy),/续跑待核验；不重复执行已完成的实现/);
assert.match(missionResponsibility(legacy),/需要你手动续跑/);
assert.equal(missionTodo(legacy)?.kind,'verification_resume');
assert.equal(missionTodo(legacy)?.actor,'user'); assert.equal(missionActionActor(legacy),'user');
assert.equal(projectTasks([legacy],[]).rows[0].needsUser,true);
for(const patch of [{can_resume:false},{contract_version:'future'},{plan_sha256:'foreign'},
 {cursor:-1},{actor:'unknown'},{reason:'unknown'},{reason:'budget_exhausted'},{environment_blocked:true}]) {
 const denied={...legacy,effective_wait:{...legacy.effective_wait!,...patch}};
 assert.equal(missionResumeAction(denied),null);
}
const pausedVerification={...legacy,effective_wait:{...legacy.effective_wait!,reason:'manual_pause',actor:'user'}};
assert.equal(missionResumeAction(pausedVerification)?.optional,true);
assert.equal(missionTodo(pausedVerification),null);
assert.match(missionResponsibility(pausedVerification),/由你决定/);
assert.equal(missionResumeAction({...legacy,tasks:[{...legacy.tasks[0],attempt_run_id:''}]}),null);
console.log('QA-LEGACY-01 recovery action and manual-pause restrictions passed');

// A state head beyond an event page is continuation, not a permanently invalid DTO.
import {readMissionSnapshot} from '../src/pages/missionSnapshot.ts';
const pageRequests:number[]=[];
const paged=await readMissionSnapshot(async after=>{pageRequests.push(after);return {mission:{...waitingBudget,effective_wait:{...waitDto,cursor:12}},cursor:after===0?4:12,requires_snapshot:after===0,events:after===0?[{event_id:'e4',sequence:4}]:[{event_id:'e12',sequence:12}] } as never;},0);
assert.deepEqual(pageRequests,[0,4]);assert.equal(paged.snapshot.cursor,12);assert.equal(paged.snapshot.events.length,2);assert.equal(paged.replace,true);
await assert.rejects(readMissionSnapshot(async()=>({mission:{...waitingBudget,effective_wait:{...waitDto,cursor:12}},cursor:4,events:[]} as never),0),/分页未推进/);
