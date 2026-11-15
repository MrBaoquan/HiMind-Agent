import assert from 'node:assert/strict';
import {missionResponsibility,missionNextAction} from '../../src/pages/missionView.ts';
const h='a'.repeat(64);
const m:any={status:'waiting',plan_sha256:h,confirmation:{plan_sha256:h},tasks:[{status:'awaiting_verification'}],effective_wait:{contract_version:'mission.wait.v1',plan_sha256:h,cursor:3,reason:'system_verification',actor:'system',can_resume:true,can_increase_budget:false,environment_blocked:false}};
const actual={responsibility:missionResponsibility(m),next_action:missionNextAction(m),can_resume:m.effective_wait.can_resume};
console.log(JSON.stringify({case:'legacy Waiting+owned AwaitingVerification; explicit resume required',actual}));
assert.match(actual.next_action,/继续|恢复|续跑/, 'Waiting is not supervisor-dispatchable: next action must request explicit resume');
assert.doesNotMatch(actual.responsibility,/无需你操作/,'Explicit user resume cannot be described as no user action');
