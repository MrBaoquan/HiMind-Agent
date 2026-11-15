import assert from 'node:assert/strict';

// Invoked only by the original real-component browser entry. Native DTO fixtures
// do not duplicate projection/evidence code and never represent real reviewers.
export async function pausedChecksRegression(h) {
  const {page,url,snapshot,task,event,plan,stamp,workspace,nativeWorkspaceRoots,setCurrent,getCurrent,requests,save,capture,checked,until}=h;
  const observations=[];
  const candidate = digit => ({schema_version:'mission_candidate.v1',identity_kind:'scoped_files_sha256',workspace_root:workspace,candidate_id:digit.repeat(64),tree_digest:digit.repeat(64),status_digest:'e'.repeat(64),commit_sha:'not-applicable'});
  function prepared() {
    const s=snapshot(); const t=task('paused-checks','awaiting_verification');
    t.attempt_run_id='current-implementation'; t.verification_run_id='current-independent';
    s.mission.tasks=[t];
    const c=candidate('c');
    s.events.push(event('current-implementation-result',6,'task_awaiting_verification',t.spec.id,{run_id:t.attempt_run_id,result:'实现已经提交'}));
    s.events.push(event('current-preparing',9,'verification_preparing',t.spec.id,{implementation_run_id:t.attempt_run_id}));
    s.events.push(event('current-host-checks',12,'verification_checks_completed',t.spec.id,{implementation_run_id:t.attempt_run_id,
      checks:[{command:'current required check',exit_code:0,test_summary:'宿主转录输出摘要；未提供数值 TAP 总数',output_sha256:'d'.repeat(64)}],
      host_check_boundary:{source:'host_required_check_comparison',matches:true,before:c,after:c}}));
    s.events.push(event('current-review-started',15,'verification_started',t.spec.id,{implementation_run_id:t.attempt_run_id,verification_run_id:t.verification_run_id}));
    s.cursor=15;s.mission.effective_wait.cursor=15;
    s.activities=[{id:t.verification_run_id,workflow_run_id:t.verification_run_id,mission_id:s.mission.mission_id,mission_task_id:t.spec.id,
      status:'running',source:'workflow',title:'独立核验夹具',progress:null,created_at:stamp,updated_at:stamp}];
    return s;
  }
  async function poll() {
    const before=requests.length;
    await page.clock.runFor(5000);
    await until(()=>requests.length>before,'Expected production poll');
  }
  async function enter(s,name) {
    setCurrent(s);save(name+'-input.json',s);await page.goto(url+'?surfaces=1');
    await page.locator('[data-task-id="mission:clock-fixture"]').waitFor();
  }
  const row=()=>page.locator('[data-task-id="mission:clock-fixture"]');
  const action=()=>page.locator('.mission-current-action');
  async function openDetail() {
    await row().getByRole('button',{name:getCurrent().mission.title,exact:true}).click();
    await page.locator('.mission-detail[data-mission-id]').waitFor();
  }
  async function record(name,locator) {const text=await locator.innerText(); observations.push({name,text});await capture(name);return text;}
  const s=prepared();await enter(s,'inflight-active');
  await until(async()=>/核验进行中/.test(await row().innerText()),'Active live verification missing from list');
  assert.match(await record('inflight-active-list',row()),/另一位专家核验中/);
  // Same live verification crosses active -> waiting, without a component reload.
  s.mission.status='waiting';s.mission.effective_wait={...s.mission.effective_wait,reason:'manual_pause',actor:'user',can_resume:true};
  s.events.push(event('pause-dispatch',18,'mission_pause','',{},'用户暂停后续派发，当前步骤可收尾'));s.cursor=18;s.mission.effective_wait.cursor=18;
  await poll();
  await until(async()=>/后续派发已暂停/.test(await row().innerText()),'Waiting list lost live verification');
  const list=await record('inflight-waiting-list',row());
  assert.match(list,/核验进行中/);assert.match(list,/当前步骤可收尾/);assert.match(list,/另一位专家核验中/);
  assert.doesNotMatch(list,/待手动续跑|核验未继续/);assert.equal(await row().getAttribute('data-todo-id'),null);
  assert.equal(await row().getAttribute('data-wait-actor'),'system');
  await page.getByRole('button',{name:'待处理入口',exact:true}).click();
  assert.equal(await page.locator('.inbox-item[data-todo-id]').count(),0);await record('inflight-waiting-inbox',page.locator('body'));
  await page.getByRole('button',{name:'任务入口',exact:true}).click();await openDetail();
  await until(async()=>await action().locator('.busy-indicator').count()===1,'Waiting detail has no live indicator');
  const detail=await record('inflight-waiting-detail',action());
  assert.match(detail,/当前步骤可收尾/);assert.match(detail,/后续派发已暂停/);assert.match(detail,/另一位专家核验中/);
  assert.doesNotMatch(detail,/待手动续跑|核验未继续/);
  await page.getByRole('button',{name:'查看验收详情',exact:true}).click();
  const taskRow=page.locator('[data-mission-task-id="paused-checks"]');
  assert.match(await record('inflight-waiting-task',taskRow),/独立核验进行中/);
  assert.equal(await page.getByRole('button',{name:/^(续跑待核验|继续独立验收|立即检查验收|恢复\/检查运行|重新核验同一实现)$/}).count(),0);
  const controls=requests.filter(r=>!['list_missions','list_local_activity','get_mission_snapshot'].includes(r.input.command)).length;
  await poll();assert.equal(requests.filter(r=>!['list_missions','list_local_activity','get_mission_snapshot'].includes(r.input.command)).length,controls);
  for(const width of [390,320]) {await page.setViewportSize({width,height:844});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true);await capture('inflight-'+width);}
  await page.setViewportSize({width:1280,height:900});
  // Ended or failed activity cannot retain a fabricated running indicator.
  for(const status of ['succeeded','failed']) {
    s.activities[0].status=status;await poll();
    await until(async()=>await action().locator('.busy-indicator').count()===0,'Ended verification still busy');
    assert.equal(await page.getByRole('button',{name:'续跑待核验',exact:true}).count(),1);
    assert.doesNotMatch(await record('inflight-ended-'+status,action()),/关联的独立核验仍在运行/);
  }
  checked('active -> waiting: list/inbox/detail/task/owner/next action/CTA agree, current step may finish, no duplicate start, ended activity clears busy');

  // A run ID alone, an old event, or foreign/unknown ownership never proves live.
  for(const [name,mutate] of [
    ['other-mission',s=>{s.activities[0].mission_id='foreign';}],
    ['other-task',s=>{s.activities[0].mission_task_id='foreign';}],
    ['other-run',s=>{s.activities[0].workflow_run_id='foreign';}],
    ['missing-owner',s=>{delete s.activities[0].mission_id;}],
    ['unknown-status',s=>{s.activities[0].status='unknown';}],
    ['missing-activity',s=>{s.activities=[];}],
  ]) {
    const s=prepared();s.mission.status='waiting';s.mission.effective_wait={...s.mission.effective_wait,reason:'system_verification',actor:'system',can_resume:false};mutate(s);
    await enter(s,'inflight-refuse-'+name);await openDetail();
    assert.equal(await action().locator('.busy-indicator').count(),0);assert.equal(await page.getByRole('button',{name:'续跑待核验',exact:true}).count(),0);
    assert.doesNotMatch(await action().innerText(),/关联的独立核验仍在运行/);
  }
  checked('foreign/missing/mismatched/unknown live-run associations remain conservative; no recovery permissions invented');

  async function entries(s,name,surfaces=false) {
    if(surfaces) {
      await enter(s,name);
      if(s.activities?.length && s.mission.tasks[0].verification_run_id) await until(async()=>/核验进行中/.test(await row().innerText()),'Native fixture live verification missing from catalog');
      await openDetail();
    } else {setCurrent(s);save(name+'-input.json',s);await page.goto(url);await page.locator('.mission-detail[data-mission-id]').waitFor();}
    await page.getByRole('tab',{name:'执行计划',exact:true}).click();
    const attempt=page.locator('[data-mission-task-id="paused-checks"] .mission-attempt-evidence').first();
    for(const entry of await attempt.locator('.mission-task-evidence-entry').all()) await entry.locator('summary').click();
    return attempt;
  }
  for(const state of ['preparing','paused','executing']) {
    const s=prepared();s.activities=[];
    if(state==='preparing') {s.mission.tasks[0].verification_run_id='';s.events=s.events.filter(e=>e.event_type!=='verification_started');}
    if(state==='paused') {s.mission.status='waiting';s.mission.effective_wait={...s.mission.effective_wait,reason:'manual_pause',actor:'user',can_resume:true};}
    // Exclude decoys by task, Mission and implementation attempt.
    const correct=s.events.find(e=>e.event_type==='verification_checks_completed');
    for(const [name,patch] of [['other-task',{task_id:'foreign'}],['other-mission',{mission_id:'foreign'}],['old-implementation',{payload:{...correct.payload,implementation_run_id:'old-attempt'}}]]) s.events.push({...structuredClone(correct),...patch,event_id:'DECOY-'+name,sequence:21+s.events.length,summary:'DECOY-'+name});
    s.cursor=Math.max(...s.events.map(e=>e.sequence));s.mission.effective_wait.cursor=s.cursor;
    const attempt=await entries(s,'checks-'+state);
    const text=await record('checks-'+state,attempt.locator('.mission-task-evidence-entries'));
    assert.match(text,/current-host-checks/);assert.match(text,/current required check/);assert.match(text,/current-implementation/);
    assert.ok(text.includes('c'.repeat(64)));assert.ok(text.includes(stamp));assert.match(text,/检查全绿不等于独立验收通过/);
    assert.match(text,/未提供数值 TAP 总数|数值 TAP 总数：未提供/);assert.doesNotMatch(text,/DECOY-/);
    assert.match(await attempt.innerText(),/尚无关联验收结论/);
    assert.match(await attempt.locator('.mission-task-evidence-entry').nth(2).innerText(),/未提供关联到本次实现与核验运行的验收结果/);
    if(state==='executing') {
      s.events.push(event('review-failed-current',s.cursor+3,'host_verification_checked','paused-checks',{
        plan_sha256:plan,implementation_run_id:'current-implementation',verification_run_id:'current-independent',candidate_before:candidate('c'),passed:false,reviewer_output:'真实状态样本：独立核验失败，历史应保留'}));
      s.cursor+=3;s.mission.effective_wait.cursor=s.cursor;await poll();
      await until(async()=>/本次验收未通过/.test(await attempt.innerText()),'Independent failed verdict did not update');
      const failureId=s.cursor;
      s.events.push(event('review-pass-current',s.cursor+3,'host_verification_checked','paused-checks',{
        plan_sha256:plan,implementation_run_id:'current-implementation',verification_run_id:'current-independent',candidate_before:candidate('c'),passed:true,reviewer_output:'独立核验通过状态样本'}));
      s.cursor+=3;s.mission.effective_wait.cursor=s.cursor;await poll();
      await until(async()=>/独立验收已记录通过/.test(await attempt.innerText()),'Independent verdict did not update');
      const history=attempt.locator('.mission-evidence-history');await history.locator('summary').click();
      assert.match(await history.innerText(),/review-failed-current/);assert.ok(s.events.some(e=>e.sequence===failureId));await capture('checks-verdict-updated-history-kept');
    }
  }
  checked('checks_completed before reviewer: preparation/pause/execution expose real command, task/implementation/candidate/event time; later verdict updates separately and failure history remains');
  for(const [name,mutate] of [
    ['missing-boundary',e=>{delete e.payload.host_check_boundary;}],
    ['unknown-boundary',e=>{delete e.payload.host_check_boundary.matches;}],
    ['candidate-mismatch',e=>{e.payload.host_check_boundary.after=candidate('b');}],
    ['missing-checks',e=>{e.payload.checks=[];}],
    ['wrong-workspace',e=>{e.payload.host_check_boundary.after.workspace_root='foreign';}],
    ['ambiguous-candidate',(e,s)=>{const other=structuredClone(e);other.event_id='other-candidate';other.sequence=14;other.payload.host_check_boundary.before=candidate('b');other.payload.host_check_boundary.after=candidate('b');s.events.push(other);}],
  ]) {
    const s=prepared();mutate(s.events.find(e=>e.event_type==='verification_checks_completed'),s);
    const attempt=await entries(s,'checks-refuse-'+name);const text=await attempt.locator('.mission-task-evidence-entries').innerText();
    assert.match(text,/未提供检查结果/);assert.doesNotMatch(text,/current required check/);
    const history=attempt.locator('.mission-evidence-history');await history.locator('summary').click();assert.match(await history.innerText(),/current-host-checks/);
    await record('checks-refuse-'+name,attempt);
  }
  checked('missing/unknown/mismatched/ambiguous candidates and empty checks never substitute current evidence; raw history stays reachable');
  const otherVerdict=prepared();
  otherVerdict.events.push(event('DECOY-other-candidate-verdict',18,'host_verification_checked','paused-checks',{
    plan_sha256:plan,implementation_run_id:'current-implementation',verification_run_id:'current-independent',candidate_before:candidate('b'),passed:true,reviewer_output:'DECOY verdict from another candidate'}));
  otherVerdict.cursor=18;otherVerdict.mission.effective_wait.cursor=18;
  const attempt=await entries(otherVerdict,'checks-other-candidate-verdict');
  const currentText=await attempt.locator('.mission-task-evidence-entries').innerText();
  assert.match(currentText,/current required check/);assert.doesNotMatch(currentText,/DECOY verdict/);
  assert.match(await attempt.innerText(),/尚无关联验收结论/);
  const history=attempt.locator('.mission-evidence-history');await history.locator('summary').click();assert.match(await history.innerText(),/DECOY-other-candidate-verdict/);
  await record('checks-other-candidate-verdict',attempt);
  checked('a verdict from another candidate cannot sign current checks; it remains accessible in history');
  // Literal transport spellings of one native root, never a test comparator or
  // an extracted projection. The original entry mounts production components.
  const roots=nativeWorkspaceRoots;
  assert.match(roots.slash,/^[A-Za-z]:\//,'Native Windows regression requires a drive-absolute fixture');
  for(const [missionForm,candidateForm] of [
    ['slash','backslash'],['backslash','slash'],['slash','extended'],
    ['extended','slash'],['backslash','extended'],['extended','backslash'],
  ]) {
    const s=prepared(), name='native-'+missionForm+'-to-'+candidateForm;
    s.mission.workspace.root=roots[missionForm];
    const c={...candidate('c'),workspace_root:roots[candidateForm]};
    const check=s.events.find(e=>e.event_type==='verification_checks_completed');
    check.payload.host_check_boundary={source:'host_required_check_comparison',matches:true,before:c,after:c};
    check.payload.checks.push({command:'native failed required check',exit_code:1,test_summary:null},
      {command:'native timed-out required check',exit_code:-1,test_summary:null,error:'Mission check time budget exhausted (fixture only)'});
    const attempt=await entries(s,name,true);
    const checkEntry=attempt.locator('.mission-task-evidence-entry').nth(1);
    const text=await record(name+'-checks',checkEntry);
    for(const expected of ['current required check','native failed required check','native timed-out required check',
      '退出码 0','退出码 1','退出码 -1','current-implementation','current-host-checks',stamp,'c'.repeat(64)]) assert.ok(text.includes(expected),name+': '+expected);
    assert.match(text,/检查全绿不等于独立验收通过/);
    assert.match(await attempt.innerText(),/尚无关联验收结论/);
    // Required checks never grant a CTA to start a second review.
    assert.equal(await page.getByRole('button',{name:/^(续跑待核验|继续独立验收|立即检查验收|重新核验同一实现)$/}).count(),0);
    const controlsBefore=requests.filter(r=>r.input.command==='control_mission').length;
    for(const passed of [false,true]) {
      const id=name+(passed?'-verdict-passed':'-verdict-failed');
      s.cursor+=3;s.mission.effective_wait.cursor=s.cursor;
      s.events.push(event(id,s.cursor,'host_verification_checked','paused-checks',{
        plan_sha256:plan,implementation_run_id:'current-implementation',verification_run_id:'current-independent',
        candidate_before:c,passed,reviewer_output:passed?'关联核验通过样本':'关联核验失败样本'}));
      save(id+'-input.json',s);await poll();
      await until(async()=>new RegExp(passed?'独立验收已记录通过':'本次验收未通过').test(await attempt.innerText()),name+': verdict failed to update');
      assert.match(await checkEntry.innerText(),/退出码 -1/,'Independent verdict must not replace the checks record');
      await record(id,attempt);
    }
    const history=attempt.locator('.mission-evidence-history');await history.locator('summary').click();
    assert.ok((await history.innerText()).includes(name+'-verdict-failed'));
    await record(name+'-history',history);
    assert.equal(requests.filter(r=>r.input.command==='control_mission').length,controlsBefore);
  }
  checked('native slash/backslash/extended roots in all six directions: actual DOM commands/0/1/-1/task/run/candidate/time, separate verdict updates, failed history and no duplicate CTA');
  for(const state of ['preparing','paused']) {
    const s=prepared();s.mission.workspace.root=roots.slash;
    const c={...candidate('c'),workspace_root:roots.extended};
    s.events.find(e=>e.event_type==='verification_checks_completed').payload.host_check_boundary={source:'host_required_check_comparison',matches:true,before:c,after:c};
    if(state==='preparing') {s.mission.tasks[0].verification_run_id='';s.activities=[];s.events=s.events.filter(e=>e.event_type!=='verification_started');s.cursor=12;s.mission.effective_wait.cursor=12;}
    else {s.mission.status='waiting';s.mission.effective_wait={...s.mission.effective_wait,reason:'manual_pause',actor:'user',can_resume:true};}
    const attempt=await entries(s,'native-'+state,true);
    const text=await record('native-'+state,attempt.locator('.mission-task-evidence-entries'));
    assert.match(text,/current-host-checks/);assert.match(text,/current required check/);assert.match(text,/退出码 0/);
    assert.match(await attempt.innerText(),/尚无关联验收结论/);
  }
  for(const [name,missionRoot,candidateRoot] of [
    ['foreign-drive',roots.slash,'Z:'+roots.slash.slice(2)],
    ['foreign-root',roots.slash,'C:/unrelated-workspace'],
    ['different-directory',roots.slash,roots.slash+'-other'],
    ['windows-directory-case',roots.slash,roots.extended.replace('Administrator','administrator')],
    ['posix-case','/Users/Operator/Project','/users/Operator/Project'],
    ['drive-relative',roots.slash,roots.slash.replace(':/',':')],
    ['dot-segment',roots.slash,roots.extended+'\\.'],
    ['parent-segment',roots.slash,roots.extended+'\\child\\..'],
    ['trailing-dot',roots.slash,roots.extended+'.'],
    ['trailing-space',roots.slash,roots.extended+' '],
    ['extended-forward-slash',roots.slash,'\\\\?\\'+roots.slash],
    ['device-namespace',roots.slash,'\\\\.\\'+roots.backslash],
    ['unc-unknown',roots.slash,'\\\\server\\share\\workspace'],
    ['duplicate-separator',roots.slash,roots.backslash.replace(':\\',':\\\\')],
    ['alternate-stream',roots.slash,roots.extended+':stream'],
  ]) {
    const s=prepared();s.mission.workspace.root=missionRoot;
    const c={...candidate('c'),workspace_root:candidateRoot};
    s.events.find(e=>e.event_type==='verification_checks_completed').payload.host_check_boundary={source:'host_required_check_comparison',matches:true,before:c,after:c};
    const attempt=await entries(s,'native-refuse-'+name), text=await attempt.locator('.mission-task-evidence-entries').innerText();
    assert.match(text,/检查候选边界缺失、失配或无法确认/);assert.doesNotMatch(text,/current required check/);
    const history=attempt.locator('.mission-evidence-history');await history.locator('summary').click();
    assert.match(await history.innerText(),/current-host-checks/);await record('native-refuse-'+name,attempt);
  }
  // Workspace spelling is the only relaxation. Raw before/after candidate
  // identity, digests and task/run/plan ownership remain exact.
  for(const [name,mutate] of [
    ['raw-candidate-root',e=>{e.payload.host_check_boundary.after.workspace_root=roots.extended;}],
    ['tree-digest',e=>{e.payload.host_check_boundary.after.tree_digest='b'.repeat(64);}],
    ['status-digest',e=>{e.payload.host_check_boundary.after.status_digest='b'.repeat(64);}],
    ['ambiguous-candidate',(e,s)=>{const other=structuredClone(e);other.event_id='native-other-candidate';other.sequence=14;other.payload.host_check_boundary.before.candidate_id='b'.repeat(64);other.payload.host_check_boundary.after.candidate_id='b'.repeat(64);s.events.push(other);}],
  ]) {
    const s=prepared();s.mission.workspace.root=roots.slash;
    const e=s.events.find(e=>e.event_type==='verification_checks_completed');
    e.payload.host_check_boundary.before={...candidate('c'),workspace_root:roots.backslash};
    e.payload.host_check_boundary.after={...e.payload.host_check_boundary.before};mutate(e,s);
    const attempt=await entries(s,'native-refuse-'+name);
    assert.doesNotMatch(await attempt.locator('.mission-task-evidence-entries').innerText(),/current required check/);
    await record('native-refuse-'+name,attempt);
  }
  for(const [name,patch] of [
    ['mission',{mission_id:'foreign'}],['task',{task_id:'foreign'}],
    ['implementation',{payload:{implementation_run_id:'foreign'}}],['verification',{payload:{verification_run_id:'foreign'}}],
    ['plan',{payload:{plan_sha256:'b'.repeat(64)}}],['candidate',{payload:{candidate_before:candidate('b')}}],
  ]) {
    const s=prepared();s.mission.workspace.root=roots.slash;
    const c={...candidate('c'),workspace_root:roots.extended};
    s.events.find(e=>e.event_type==='verification_checks_completed').payload.host_check_boundary={source:'host_required_check_comparison',matches:true,before:c,after:c};
    const verdict=event('native-DECOY-verdict',18,'host_verification_checked','paused-checks',{
      plan_sha256:plan,implementation_run_id:'current-implementation',verification_run_id:'current-independent',candidate_before:c,passed:true,reviewer_output:'native-DECOY-output'});
    s.events.push({...verdict,...patch,payload:{...verdict.payload,...patch.payload}});s.cursor=18;s.mission.effective_wait.cursor=18;
    const attempt=await entries(s,'native-verdict-refuse-'+name), text=await attempt.locator('.mission-task-evidence-entries').innerText();
    assert.match(text,/current required check/);assert.doesNotMatch(text,/native-DECOY-output/);assert.match(await attempt.innerText(),/尚无关联验收结论/);
    await record('native-verdict-refuse-'+name,attempt);
  }
  checked('native foreign/ambiguous path forms and POSIX case refuse; raw candidate/digest/mission/task/implementation/verification/plan guards preserved');
  save('paused-checks-dom.json',observations);
}
