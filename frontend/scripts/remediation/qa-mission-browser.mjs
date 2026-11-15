import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {appendFileSync, writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {createServer} from 'vite';
import react from '@vitejs/plugin-react';
import {chromium} from 'playwright';
import {createRuntime, workspace} from './service-ui-runtime.mjs';

// Real React component regression. No extracted hook, duplicated budget formula,
// source matching, model calls or production state. Only Tauri transport is a fixture.
const runtime = createRuntime('clock');
const evidence = runtime.directory;
if (process.platform === 'win32') {
  const short = execFileSync('python', ['-c', 'import ctypes,sys; b=ctypes.create_unicode_buffer(32768); assert ctypes.windll.kernel32.GetShortPathNameW(sys.argv[1],b,len(b)); print(b.value)', runtime.temporary], {encoding:'utf8', windowsHide:true}).trim();
  Object.assign(process.env, {TEMP:short, TMP:short, TMPDIR:short});
}
const start = Date.parse('2026-11-13T02:00:00.000Z');
const stamp = '2026-11-13T02:00:00.000Z';
const plan = 'a'.repeat(64);
const save = (name, value) => writeFileSync(resolve(evidence, name), JSON.stringify(value, null, 2));
const assertions = [], observations = [], requests = [], pageErrors = [];
let current, mode = '', holdNext = false, releaseHeld;
let browser, page, server;
const clone = value => structuredClone(value);
function task(id, status = 'ready', repair = '') {
  return {spec:{id, title:`夹具交付 ${id}`, objective:'验证真实组件证据归属', depends_on:[], read_paths:['frontend/src'], write_paths:['frontend/src'], acceptance_criteria:['证据可追溯'], required_checks:['fixture check'], execution:{executor:'personal.codex', role_id:'fixture.frontend', model:''}, repair_of:repair}, status, attempt_run_id:'', verification_run_id:'', verification_passed:null, evidence_refs:[]};
}
function event(id, sequence, type, taskId = '', payload = {}, summary = id) {
  return {event_id:id, mission_id:'clock-fixture', sequence, event_type:type, task_id:taskId, summary, payload, occurred_at:stamp};
}
function snapshot() {
  return {mission:{schema_version:'mission.v1', mission_id:'clock-fixture', title:'受控时钟与证据归属夹具', objective:'真实组件回归', status:'active', workspace:{root:workspace, read_scope:['frontend/src'], write_scope:['frontend/src'], permission_mode:'workspace_write'}, acceptance_criteria:['等待时间持续累计'], plan_revision:1, plan_sha256:plan, plan_summary:'隔离契约样本，不是真实专家运行', required_acceptance:[], proposed_tasks:[], budget:{max_rounds:50, max_minutes:2, max_repair_attempts:5, max_total_tokens:100, max_cost:null}, usage:null, confirmation:{plan_sha256:plan, confirmed_at:stamp, confirmed_by:'fixture', approved_paths:['frontend/src'], approved_absolute_read_paths:[], approved_absolute_write_paths:[]}, tasks:[task('clock-task')], created_at:stamp, updated_at:stamp, effective_wait:{contract_version:'mission.wait.v1', plan_sha256:plan, cursor:3, reason:'not_waiting', actor:'none', budget_reason:null, environment_blocked:false, can_increase_budget:false, can_resume:false}}, events:[event('fixture-confirmed',3,'plan_confirmed')], cursor:3};
}
function waiting(reason) {
  current.mission.status = 'waiting';
  current.mission.effective_wait = {...current.mission.effective_wait, reason, actor:reason === 'system_verification' ? 'system' : 'user', can_increase_budget:reason !== 'system_verification', can_resume:reason === 'manual_pause', environment_blocked:reason === 'environment_blocked', budget_reason:reason === 'budget_exhausted' ? 'Token 100/100（契约样本）' : null};
  if (reason === 'system_verification') current.mission.tasks[0].status = 'awaiting_verification';
  if (reason === 'budget_exhausted') current.mission.usage = {input_tokens:100, output_tokens:0, estimated_cost:0, currency:'', billing_owner:''};
}
function checked(name) { assertions.push(name); console.log('PASS ' + name); }
async function until(fn, message, timeout = 20000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { if (await fn()) return; await new Promise(accept => setTimeout(accept, 30)); }
  throw new Error(message);
}
async function textIs(locator, expected) {
  await until(async () => await locator.innerText() === expected, `Expected DOM text: ${expected}; actual: ${await locator.innerText()}`);
  assert.equal(await locator.innerText(), expected);
}
const overview = () => page.locator('.mission-detail .mission-budget-summary');
const dialog = () => page.getByRole('dialog');
const timeText = summary => summary.locator('dl > div').filter({has:page.locator('dt', {hasText:/^时限$/})}).locator('dd');
async function observe(label, expected, withDialog = false) {
  await textIs(timeText(overview()), expected);
  if (withDialog) await textIs(timeText(dialog().locator('.mission-budget-summary')), expected);
  const record = {label, browser_now:await page.evaluate(() => Date.now()), status:await page.locator('.mission-detail').getAttribute('data-wait-reason'), overview:await timeText(overview()).innerText(), dialog:withDialog ? await timeText(dialog().locator('.mission-budget-summary')).innerText() : null};
  observations.push(record);
  appendFileSync(resolve(evidence, 'clock-dom.jsonl'), JSON.stringify(record) + '\n');
}
async function capture(name, fullPage = true) { await page.screenshot({path:resolve(evidence, name + '.png'), fullPage}); }
async function load(next, name) {
  current = next; mode = ''; save(name + '-input.json', current);
  await page.goto(url);
  await page.locator('.mission-detail[data-mission-id="clock-fixture"]').waitFor();
}
async function refresh() {
  const menu = page.locator('.mission-more-actions');
  if (!await menu.evaluate(el => el.open)) await menu.locator('summary').click();
  await menu.getByRole('button', {name:'刷新任务', exact:true}).click();
}
async function openBudget() {
  const menu = page.locator('.mission-more-actions');
  if (!await menu.evaluate(el => el.open)) await menu.locator('summary').click();
  await menu.getByRole('button', {name:'主动增加预算并续跑', exact:true}).click();
  await dialog().waitFor();
}
async function closeBudget() { await dialog().getByRole('button', {name:'关闭', exact:true}).click(); await dialog().waitFor({state:'hidden'}); }
async function waitReason(reason) {
  await until(async () => await page.locator('.mission-detail').getAttribute('data-wait-reason') === reason, `Wait reason ${reason} was not rendered`);
}
let url;
try {
  server = await createServer({root:resolve(workspace, 'frontend'), cacheDir:runtime.cache, configFile:false, plugins:[react(), {
    name:'mission-clock-transport-fixture', transformIndexHtml(html){return html.replace('<head>', '<head>'+'<script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script>');}, configureServer(vite) {
      vite.middlewares.use('/clock-evidence-transport', async (req, res) => {
        try {
          const chunks = []; for await (const chunk of req) chunks.push(chunk);
          const input = JSON.parse(Buffer.concat(chunks).toString());
          const log = {input, mode, fixture_reason:current?.mission.effective_wait?.reason}; requests.push(log);
          if (mode === 'loading') await new Promise(accept => setTimeout(accept, 1200));
          let result;
          if (['error','denied'].includes(mode)) result = {ok:false, error:mode === 'denied' ? '无权读取任务（传输契约样本）' : '任务读取失败（传输契约样本）'};
          else if (input.command === 'get_mission_snapshot') {
            assert.equal(input.missionId, current.mission.mission_id);
            const after = input.afterSequence || 0;
            const events = current.events.filter(event => event.sequence > after).slice(0, 10000);
            const cursor = events.at(-1)?.sequence ?? after;
            result = {ok:true, value:clone({...current, events, cursor, requires_snapshot:cursor < current.mission.effective_wait.cursor})};
            if (holdNext) { holdNext = false; log.held = true; await new Promise(accept => { releaseHeld = accept; }); }
          } else throw new Error('Unexpected native command: ' + input.command);
          log.response = result; appendFileSync(resolve(evidence, 'transport.jsonl'), JSON.stringify(log) + '\n');
          res.setHeader('Content-Type','application/json; charset=utf-8'); res.end(JSON.stringify(result));
        } catch (error) { res.statusCode = 500; res.end(JSON.stringify({ok:false, error:String(error)})); }
      });
    },
  }], server:{host:'127.0.0.1', port:0}});
  await server.listen();
  url = `http://127.0.0.1:${server.httpServer.address().port}/scripts/fixtures/mission-ui-clock-evidence.html`;
  try { browser = await chromium.launch({headless:true, channel:'msedge'}); }
  catch (error) { writeFileSync(resolve(evidence,'edge-launch-failure.txt'), String(error)); browser = await chromium.launch({headless:true}); }
  const context = await browser.newContext({viewport:{width:1280, height:900}});
  await context.tracing.start({screenshots:true, snapshots:true, sources:false});
  page = await context.newPage();
  page.on('pageerror', error => pageErrors.push(error.message));
  await page.clock.install({time:start});
  await page.clock.pauseAt(start + 1000);

  const two = '剩余 2 分钟 / 上限 2 分钟';
  const one = '剩余 1 分钟 / 上限 2 分钟';
  const exhausted = '时限已耗尽 · 剩余 0 分钟 / 上限 2 分钟';
  for (const reason of ['manual_pause','environment_blocked','budget_exhausted','system_verification']) {
    await page.clock.setSystemTime(start);
    await load(snapshot(), reason);
    await observe(reason + ':active', two);
    if (reason === 'system_verification') {
      // Open only while backend eligibility permits. Then keep the real dialog
      // mounted across active -> waiting verification; never fake eligibility.
      waiting('manual_pause'); await refresh(); await waitReason('manual_pause'); await openBudget();
      current.mission.status = 'active'; current.mission.effective_wait.reason = 'not_waiting'; current.mission.effective_wait.can_increase_budget = false;
      await page.clock.runFor(5000); await waitReason('not_waiting');
    }
    await page.clock.setSystemTime(start + 30000);
    waiting(reason); await page.clock.runFor(5000); await waitReason(reason);
    if (reason !== 'system_verification') await openBudget();
    await observe(reason + ':waiting-35s', two, true);
    await capture(reason + '-waiting');
    // Hard-coded boundary expectations, independent of the implementation formula.
    await page.clock.setSystemTime(start + 59000); await page.clock.runFor(1000);
    await observe(reason + ':waiting-60s', one, true);
    if (reason !== 'system_verification') await closeBudget();
    await page.clock.setSystemTime(start + 118000); await page.clock.runFor(1000);
    await observe(reason + ':waiting-119s', one, reason === 'system_verification');
    if (reason !== 'system_verification') await openBudget();
    await observe(reason + (reason === 'system_verification' ? ':dialog-retained-119s' : ':reopened-119s'), one, true);
    await page.clock.runFor(1000);
    await observe(reason + ':exact-limit-120s', exhausted, true);
    await page.clock.runFor(61000);
    await observe(reason + ':over-limit-181s', exhausted, true);
    await capture(reason + '-exhausted');
    if (reason !== 'system_verification') { await closeBudget(); await openBudget(); await observe(reason + ':reopened-over-limit', exhausted, true); await closeBudget(); }
    else { await closeBudget(); assert.equal(await page.getByRole('button',{name:'主动增加预算并续跑',exact:true}).count(), 0); }
    checked(reason + ': actual active→waiting component, continued clock, exact/over boundary and allowed dialog reopen');
  }
  // No terminal endpoint is defined by MissionView. Do not use updated_at as one.
  await page.clock.setSystemTime(start);
  const terminal = snapshot(); terminal.mission.status = 'completed'; terminal.mission.tasks = [];
  await load(terminal, 'terminal-no-freeze'); await observe('terminal:initial', two);
  await page.clock.runFor(120000); await observe('terminal:wall-clock-limit', exhausted);
  assert.match(await overview().innerText(), /(?:不代表|并非)结算运行耗时/);
  checked('terminal has no invented freeze endpoint; explicit cumulative-time disclosure');

  function deliveries(state) {
    const s = snapshot(); const a = task('original-a', 'completed'); const repair = task('repair-a', state === 'rejected' ? 'failed' : 'completed', 'original-a');
    const b = task('other-b', state === 'completed' ? 'completed' : 'ready');
    a.attempt_run_id = 'fixture-impl-old'; a.verification_run_id = 'fixture-review-old'; a.verification_passed = false;
    repair.attempt_run_id = 'fixture-impl-repair'; repair.verification_run_id = 'fixture-review-repair-2'; repair.verification_passed = state !== 'rejected';
    repair.spec.title += ' · 长文本交付用于检查窄屏换行'.repeat(8);
    repair.evidence_refs = ['fixture-reference/' + 'long-path-segment/'.repeat(18) + 'result.json'];
    if (state === 'completed') { b.attempt_run_id = 'fixture-impl-b'; b.verification_run_id = 'fixture-review-b'; b.verification_passed = true; }
    s.mission.status = state === 'completed' ? 'completed' : 'active'; s.mission.tasks = [a, repair, b];
    const records = (t, tag, passed, bodyProvided = true) => [
      event(tag + '-implementation', 0, 'task_awaiting_verification', t.spec.id, {run_id:t.attempt_run_id, ...(bodyProvided ? {result:tag + ' 实现正文'} : {})}, tag + ' 实现摘要'),
      event(tag + '-started', 0, 'verification_started', t.spec.id, {implementation_run_id:t.attempt_run_id, verification_run_id:t.verification_run_id}),
      event(tag + '-checked', 0, 'host_verification_checked', t.spec.id, {plan_sha256:plan, implementation_run_id:t.attempt_run_id, verification_run_id:t.verification_run_id, passed, checks:[{command:tag + ' check', exit_code:passed ? 0 : 1, test_summary:null}], ...(bodyProvided ? {reviewer_output:tag + (passed ? ' 独立核验正文' : ' 独立核验失败正文')} : {})}),
    ];
    const prior = clone(repair); prior.verification_run_id = 'fixture-review-repair-1';
    s.events.push(...records(a, 'old-failed', false), ...records(prior, 'retry-failed', false), ...records(repair, 'current-repair', state !== 'rejected', state === 'rejected'));
    if (state === 'completed') s.events.push(...records(b, 'other-b', true));
    const decoy = clone(s.events.find(e => e.event_id === 'current-repair-checked'));
    for (const [tag, patch] of [['wrong-plan',{payload:{...decoy.payload, plan_sha256:'b'.repeat(64)}}], ['wrong-implementation',{payload:{...decoy.payload, implementation_run_id:'unrelated-impl'}}], ['wrong-review',{payload:{...decoy.payload, verification_run_id:'unrelated-review'}}], ['wrong-task',{task_id:'other-b'}], ['wrong-mission',{mission_id:'another-mission'}]]) {
      const e = {...clone(decoy), ...patch, event_id:'DECOY-' + tag}; e.payload.reviewer_output = 'DECOY-' + tag; e.payload.passed = true; s.events.push(e);
    }
    s.events.forEach((e, i) => { e.sequence = (i + 1) * 3; e.occurred_at = new Date(start + i * 1000).toISOString(); });
    s.cursor = s.events.at(-1).sequence; s.mission.effective_wait.cursor = s.cursor;
    return s;
  }
  for (const state of ['partial','rejected','completed']) {
    await page.clock.setSystemTime(start); const data = deliveries(state); await load(data, 'evidence-' + state);
    const expectedEvent = data.events.find(e => e.event_id === 'current-repair-checked');
    const group = page.locator('.mission-delivery-group').first(); if (!await group.evaluate(el => el.open)) await group.locator(':scope > summary').click();
    const overviewRecord = group.locator('[data-attempt-id="repair-a"]');
    assert.match(await overviewRecord.innerText(), /fixture-impl-repair/);
    const overviewColumns = await overviewRecord.locator('.mission-evidence-record').allTextContents();
    await capture('evidence-' + state + '-overview');
    await page.getByRole('tab', {name:'执行计划', exact:true}).click();
    const row = page.locator('[data-mission-task-id="repair-a"]');
    const attempt = row.locator('.mission-attempt-evidence').first();
    const entries = attempt.locator('.mission-task-evidence-entry'); assert.equal(await entries.count(), 3);
    for (let i = 0; i < 3; i++) await entries.nth(i).locator('summary').click();
    assert.deepEqual(await attempt.locator('.mission-evidence-record').allTextContents(), overviewColumns, 'row and overview expose identical associated records');
    const currentRecords = await attempt.locator('.mission-task-evidence-entries').innerText();
    assert.match(currentRecords, /fixture-impl-repair/); assert.match(currentRecords, /fixture-review-repair-2/);
    assert.match(currentRecords, /current-repair-implementation/); assert.match(currentRecords, /current-repair-checked/);
    assert.ok(currentRecords.includes(expectedEvent.occurred_at));
    assert.doesNotMatch(currentRecords, /DECOY-|old-failed|retry-failed|other-b check|fixture-review-repair-1/);
    assert.match(currentRecords, /未提供正文/); // Numeric TAP totals were not supplied.
    if (state === 'rejected') { assert.match(await attempt.innerText(), /本次验收未通过/); assert.match(currentRecords, /独立核验失败正文/); }
    else if (state === 'partial') { assert.match(currentRecords, /未提供正文/); assert.match(await attempt.innerText(), /独立验收已记录通过/); }
    const history = attempt.locator('.mission-evidence-history'); await history.locator('summary').click();
    assert.match(await history.innerText(), /retry-failed-checked/); assert.match(await history.innerText(), /fixture-review-repair-1/);
    const lineage = row.locator('.mission-task-lineage'); await lineage.locator(':scope > summary').click();
    assert.match(await lineage.innerText(), /old-failed-checked|fixture-review-old/); assert.match(await lineage.innerText(), /保留失败证据/);
    const other = page.locator('[data-mission-task-id="other-b"] .mission-attempt-evidence').first();
    for (const entry of await other.locator('.mission-task-evidence-entry').all()) await entry.locator('summary').click();
    const otherText = await other.locator('.mission-task-evidence-entries').innerText();
    if (state !== 'completed') { assert.match(otherText, /未提供运行号/); assert.match(otherText, /未提供记录 ID/); assert.match(otherText, /未提供时间/); assert.match(otherText, /未提供检查结果/); assert.doesNotMatch(await other.innerText(), /独立验收已记录通过/); }
    else { assert.match(otherText, /fixture-impl-b/); assert.match(otherText, /fixture-review-b/); assert.match(otherText, /other-b 独立核验正文/); }
    assert.doesNotMatch(otherText, /fixture-impl-repair|fixture-review-repair-2/);
    await history.locator(':scope > summary').click();
    await lineage.locator(':scope > summary').click();
    await row.scrollIntoViewIfNeeded(); await capture('evidence-' + state + '-row');
    await attempt.locator('.mission-task-evidence-entries').screenshot({path:resolve(evidence,'evidence-' + state + '-entries.png')});
    if (state === 'partial') {
      // Keyboard opens the actual native disclosure, with a visible focus rule.
      const summary = entries.first().locator('summary'); await summary.click(); await summary.focus(); await page.keyboard.press('Enter');
      assert.equal(await entries.first().evaluate(el => el.open), true);
      assert.equal(await summary.evaluate(el => getComputedStyle(el).outlineWidth), '2px');
      for (const width of [390,320]) {
        await page.setViewportSize({width, height:844}); await row.scrollIntoViewIfNeeded();
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true);
        await capture('evidence-row-' + width);
        await attempt.locator('.mission-task-evidence-entries').screenshot({path:resolve(evidence,'evidence-entries-' + width + '.png')});
      }
      await page.setViewportSize({width:1280, height:900}); await page.evaluate(() => { document.documentElement.style.zoom = '200%'; });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true); await capture('evidence-row-200-percent');
      await page.evaluate(() => { document.documentElement.style.zoom = ''; });
      for (const colorScheme of ['dark','light']) { await page.emulateMedia({colorScheme, reducedMotion:'reduce'}); await capture('evidence-row-' + colorScheme + '-reduced'); }
      assert.equal(await row.evaluate(el => el.getAnimations({subtree:true}).filter(a => a.playState === 'running' && a.effect?.getTiming().iterations === Infinity).length), 0);
    }
    checked(state + ': overview + row entries, exact task/run/id/time/body association, retry/repair failure history, missing records');
  }

  // Preserve real detail pagination behavior with >10k globally interleaved IDs.
  const paged = snapshot(); paged.mission.tasks = [];
  paged.events = Array.from({length:10005}, (_, i) => event('paged-' + i, (i + 1) * 3, 'task_progress', '', {}, 'page-row-' + i));
  paged.cursor = paged.events.at(-1).sequence; paged.mission.effective_wait.cursor = paged.cursor;
  const beforeRequests = requests.length;
  await load(paged, 'pagination');
  assert.deepEqual(requests.slice(beforeRequests).map(r => r.input.afterSequence), [0,30000]);
  assert.equal(await page.locator('.mission-detail').getAttribute('data-snapshot-cursor'), '30015');
  await page.getByRole('tab',{name:'活动记录',exact:true}).click();
  assert.equal(await page.locator('.mission-timeline > li').count(), 10005);
  assert.equal(await page.locator('.mission-timeline > li p').evaluateAll(nodes => new Set(nodes.map(n => n.textContent)).size), 10005);
  const noNewStart = requests.length; await page.clock.runFor(5000);
  await until(() => requests.length > noNewStart, 'No-new-events poll missing');
  assert.equal(requests.at(-1).input.afterSequence, 30015);
  assert.equal(await page.locator('.mission-timeline > li').count(), 10005);
  // The 10k-row document exceeds screenshot image dimensions; retain viewport,
  // all DOM assertions and the complete trace/transport instead of a huge bitmap.
  await capture('pagination-global-gaps-no-new-events', false);
  checked('actual detail loads 10005 events across two pages/global gaps, retains all unique rows, no-new poll does not reset');

  await load(snapshot(), 'out-of-order'); waiting('manual_pause');
  holdNext = true; await page.clock.runFor(5000);
  await until(() => Boolean(releaseHeld), 'Did not capture stale poll');
  waiting('environment_blocked'); current.events.push(event('newer-environment',9,'runtime_workspace_blocked','',{plan_sha256:plan}));
  current.cursor = 9; current.mission.effective_wait.cursor = 9;
  await refresh(); await waitReason('environment_blocked');
  releaseHeld(); releaseHeld = undefined;
  await until(() => requests.some(r => r.held && r.response), 'Held response not delivered');
  await page.clock.runFor(1000); await waitReason('environment_blocked');
  assert.equal(await page.locator('.mission-detail').getAttribute('data-snapshot-cursor'), '9');
  await capture('out-of-order-latest-kept'); checked('late older poll cannot overwrite newer associated detail snapshot');

  // Fail-closed snapshot association and the six UI states use the same component.
  const mismatch = snapshot(); mismatch.mission.status = 'waiting'; mismatch.mission.effective_wait.can_increase_budget = true;
  mismatch.mission.effective_wait.plan_sha256 = 'b'.repeat(64); await load(mismatch, 'mismatched-plan');
  assert.equal(await page.locator('.mission-detail').getAttribute('data-snapshot-cursor'), null);
  assert.equal(await page.getByRole('button',{name:'主动增加预算并续跑',exact:true}).count(), 0);
  await capture('mismatched-plan');
  const empty = snapshot(); empty.mission.tasks = []; empty.events = []; empty.cursor = 0; empty.mission.effective_wait.cursor = 0;
  await load(empty, 'empty'); assert.match(await page.locator('.mission-delivery').innerText(), /尚无可展示/); await capture('empty');
  for (const fault of ['loading','error','denied']) {
    current = snapshot(); mode = fault; await page.goto(url);
    if (fault === 'loading') { await page.getByText('正在读取任务详情',{exact:true}).waitFor(); await capture('loading'); await page.locator('.mission-detail[data-mission-id]').waitFor(); }
    else { await page.getByRole('alert').waitFor(); assert.match(await page.getByRole('alert').innerText(), fault === 'denied' ? /无权读取/ : /任务读取失败/); await capture(fault); }
  }
  checked('missing plan association fails closed; loading/empty/error/partial/success/denied rendered');
  assert.deepEqual(pageErrors, []);
  await context.tracing.stop({path:resolve(evidence,'browser-trace.zip')});
  save('browser-result.json', {passed:true, assertions, observations, pageErrors, requests:requests.length, classification:'real Chromium/Edge + production React component + fake browser clock; transport DTO contract fixtures, no actual Store or independent expert execution', unverified:['production/Tauri/MCP/backend transactions','real expert verdicts','native browser chrome zoom','true dark palette (existing light tokens retained)','screen reader and complete accessibility conformance']});
  console.log('MISSION_UI_CLOCK_EVIDENCE=' + evidence);
} catch (error) {
  if (page) { await capture('failure', false).catch(() => {}); writeFileSync(resolve(evidence,'failure-dom.txt'), await page.locator('body').innerText().catch(() => '')); await page.context().tracing.stop({path:resolve(evidence,'failure-trace.zip')}).catch(() => {}); }
  save('browser-result.json', {passed:false, assertions, observations, pageErrors, error:String(error.stack)});
  console.error('MISSION_UI_CLOCK_EVIDENCE=' + evidence); throw error;
} finally { releaseHeld?.(); if (browser) await browser.close(); if (server) await server.close(); }
