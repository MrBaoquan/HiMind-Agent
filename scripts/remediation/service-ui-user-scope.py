"""Author the scope-corrected business report; never run this during frozen checks."""
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess

ROOT = Path(__file__).resolve().parents[2]
TASK = 'repair-service-ui-user-scope-1'
REPORT = ROOT / 'reports/service-ui/user-scope-1'
HISTORY = REPORT / 'history'
stage = runpy.run_path(str(ROOT / 'scripts/remediation/service-ui-qa-stage.py'))
runtime = stage['runtime']()
q = runtime / 'q'


def load(path):
    return json.loads(path.read_text(encoding='utf-8'))


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def ref(path):
    assert path.resolve().is_relative_to(runtime) or path.resolve().is_relative_to(REPORT)
    return {'path': str(path), 'sha256': sha(path)}


def runtime_directory(record):
    text = Path(record['evidence']).read_text(encoding='utf-8')
    match = re.search(r'^SERVICE_UI_RUNTIME_DIRECTORY=(.+)$', text, re.M)
    assert match, record
    directory = Path(match[1].strip()).resolve(strict=True)
    assert directory.is_relative_to(runtime)
    return directory


def protected_review():
    baseline = load(ROOT / 'remediation-input/baseline-manifest.json')
    provenance = load(ROOT / 'docs/mixed-executor-controlled-host-patches.json')
    records = []
    for index, name in enumerate(['docs/mixed-executor-controlled-host-patches.json', 'src/app/mission_dispatch.rs']):
        path = ROOT / name
        current = sha(path)
        head = subprocess.run(['git', 'show', 'HEAD:' + name], cwd=ROOT, capture_output=True, check=False)
        (q / f'protected-{index}-head.bin').write_bytes(head.stdout)
        (q / f'protected-{index}-head.stderr').write_bytes(head.stderr)
        delta = ''.join(difflib.unified_diff(
            head.stdout.decode('utf-8', errors='replace').splitlines(keepends=True),
            path.read_text(encoding='utf-8').splitlines(keepends=True),
            fromfile='accessible-HEAD/' + name, tofile='current/' + name))
        diff = q / f'protected-{index}.diff'
        diff.write_text(delta, encoding='utf-8')
        matches = [record['record_file'] for record in provenance['records']
                   if record.get('record', {}).get('files', {}).get(name, {}).get('after') == current]
        records.append(dict(path=name, current_sha256=current,
                            initial_manifest_sha256=baseline['files'].get(name),
                            head_exit_code=int(head.returncode),
                            head_sha256=hashlib.sha256(head.stdout).hexdigest() if head.returncode == 0 else None,
                            matching_provenance=matches, diff=ref(diff),
                            ownership='unresolved; host adjudication required',
                            protected_file_check='not_performed'))
    save(REPORT / 'protected-provenance.json', records)
    return records


def main():
    old = load(HISTORY / 'result.json')
    previous = load(HISTORY / 'independent-qa-1__evidence-index.json')
    assert old['passed'] is False
    for entry in load(HISTORY / 'archive.json'):
        assert sha(ROOT / entry['original']) == entry['sha256']
        assert sha(ROOT / entry['archive']) == entry['sha256']
    checks = [load(q / (label + '.json')) for label in
              ['prepare-ci', 'current-browser', 'current-build', 'current-modes', 'current-runtime']]
    source = stage['manifest'](ROOT)
    assert source == load(q / 'source-before.json') == stage['manifest'](q / 's')
    for check in checks:
        assert check['exit_code'] == 0 and not check['timed_out'] and check['startup_error'] is None
        assert sha(Path(check['evidence'])) == check['sha256']
        assert load(Path(check['source_manifest'])) == source
    prior_hashes = previous['source_changes']
    for item in prior_hashes:
        assert sha(ROOT / item['path']) == item['sha256'], item['path']
    tools = stage['toolchain'](ROOT)
    assert tools == stage['toolchain'](q / 's')
    save(q / 'current-toolchain.json', tools)
    browser = runtime_directory(checks[1])
    browser_assertions = load(browser / 'browser-assertions.json')
    assert browser_assertions['passed'] is True and browser_assertions['errors'] == []
    assert all(load(browser / 'cleanup.json').values())
    modes = runtime_directory(checks[3])
    mode_data = load(modes / 'build-modes.json')
    assert mode_data['passed'] is True and len(mode_data['scenarios']) == 7
    assert load(modes / 'cleanup.json')['child_active'] is False
    negative = []
    for label, environment in [('expected-timeout', {'SERVICE_UI_TIMEOUT_MS': '1'}),
                               ('expected-startup-failure', {'SERVICE_UI_BROWSER_CHANNEL': 'invalid-service-ui-channel'})]:
        record = load(q / (label + '.json'))
        assert record['exit_code'] != 0
        directory = runtime_directory(record)
        cleanup = load(directory / 'cleanup.json')
        assert all(cleanup.values())
        negative.append(dict(record=record, environment=environment,
                             classification='expected-negative-harness-check', cleanup=ref(directory / 'cleanup.json')))
    protected = protected_review()
    source_review = dict(
        frontend_delta_this_task=[], previous_source_changes_verified=prior_hashes,
        current_source_manifest=ref(q / 'source-before.json'),
        inherited_manifest_sha256=old['checks'][0]['source_manifest_sha256'],
        manifest_matches_inherited=sha(q / 'source-before.json') == old['checks'][0]['source_manifest_sha256'],
        assertions_and_scenarios='Entire frontend source/file manifest unchanged; no assertion removal or weakening.',
        refresh_fix='frontend/scripts/remediation/service-ui-browser.mjs:40; explicit RefreshRuntime preamble before production component import.',
        build_fix='frontend/scripts/remediation/service-ui-build.mjs:8,13,14,27,30; absent root ordinary dist, explicit root createRuntime, public package.json/bin.tsc resolution.',
        prior_diffs=['reports/service-ui/independent-qa-1/source-diff.patch', 'reports/service-ui/repair11/source-diff.patch'],
        nested_copy_failure='Historical extra nesting caused TS7 #getExePath ERR_PACKAGE_IMPORT_NOT_DEFINED; source copy is already owned by the original modes entry. Current original entry directly exits 0; no TS7/dependency/assertion changes.',
        previous_failure_is_preserved='history/result.json plus unchanged independent-qa-1 reports; external historical resources not read.')
    assert source_review['manifest_matches_inherited']
    save(REPORT / 'source-review.json', source_review)
    deferred = {name: dict(status='user_deferred', scope='not_in_current_scope', passed=None,
                            blocking=False, authority='direct human instruction; native event825 supplied in task contract')
                for name in ['screen_reader_speech', 'all_200_percent_zoom_including_native']}
    limitations = [
        'Screen-reader speech and ALL 200% zoom are user_deferred/not_in_current_scope, neither passed nor current blockers. No NVDA/Narrator/native zoom/Computer Use was attempted. Existing unchanged CSS zoom fixtures run only as supplementary evidence, not a native zoom claim.',
        'Real Chrome production-component API fixture with fictitious data; installed/live backend, credentials, mixed-executor handoff and deployment acceptance are not claimed.',
        'Light/dark media preference and reduced motion tested against the project fixed light palette; no independent dark palette or whole-site AA certification claimed.',
        'npm ci uses an exact current frontend source/lockfile copy under this task root; original browser/build/build-mode commands run directly in candidate. Dependency toolchain hashes are compared. Generated artifacts do not replace installed/live acceptance.',
        'Protected file ownership remains unresolved: accessible initial/HEAD baselines and provenance are recorded read-only; formal host:candidate-integrity/protected checks remain host owned.',
        'Previous native evidence, Computer Use stop and unverified OS screen-reader flag restoration remain historical limitations unchanged; no settings work performed in this task.',
        'Existing four high npm advisories and chunk-size warning remain. Empty historical test_summary is not a TAP count; usage/cost stay unknown.',
        'Conservative repeated-use operator scenario retained. No product priority, dependency, service, model, system configuration, other producer, commit/push/deploy changes or Mission-wide/independent signoff.',
        'passed describes business-stage readiness before the frozen final replay. Final six raw outcomes and local invariant comparison are external immutable-run evidence referenced below; a final stage-ready recommendation requires all six actual successes.'
    ]
    changed = [str(path.relative_to(ROOT)).replace('\\', '/') for path in sorted(REPORT.rglob('*')) if path.is_file()]
    changed += ['scripts/remediation/service-ui-user-scope.py', 'reports/service-ui/result.json',
                'reports/service-ui/user-scope-1/delivery.md', 'reports/service-ui/user-scope-1/evidence-index.json']
    final_replay = str(q / 'final-checks.json')
    integrity = str(q / 'local-integrity.json')
    index = dict(task_id=TASK, phase='business_ready_pending_frozen_six', runtime_root=str(runtime),
                 scope_authority={'message': '读屏可以不用考虑，200%缩放暂时也不考虑', 'event': 'mission_1794473598404_ffe1e069:825'},
                 deferred=deferred, checks=checks, negative_cleanup=negative,
                 source_review='reports/service-ui/user-scope-1/source-review.json',
                 source_manifest=ref(q / 'source-before.json'), toolchain=ref(q / 'current-toolchain.json'),
                 browser=dict(directory=str(browser), classification='real-browser-production-components-with-controlled-API-fixture',
                              files=[ref(p) for p in sorted(browser.iterdir()) if p.is_file()],
                              ordinary_viewports=[320,390,480,620,880], screenshots=len(list(browser.glob('*.png'))),
                              supplementary_css_zoom='not a current gate or native zoom claim',
                              visual_review=['services-320.png', 'services-880.png', 'expanded-form-320.png', 'full-service-list-denied.png']),
                 build_modes=dict(report=ref(modes / 'build-modes.json'), scenarios=mode_data['scenarios'],
                                  source_origin=ref(modes / 'source-origin.json'), cleanup=ref(modes / 'cleanup.json')),
                 protected=protected, history=dict(classification='historical BIZ report data only; external resources not accessed',
                                                   archive='reports/service-ui/user-scope-1/history/archive.json',
                                                   inherited_history=previous['historical'],
                                                   inherited_failures=previous['failed_attempts'],
                                                   old_result_checks=old['checks']),
                 final_read_only_replay=final_replay, local_integrity=integrity, unverified=limitations,
                 usage=None, cost=None, formal_verify='host owned; not invoked or awaited')
    save(REPORT / 'evidence-index.json', index)
    result = dict(passed=True, task_id=TASK,
                  summary='Current scoped business checks ready; original frontend and all assertions retained. Deferred coverage excluded. Final frozen six-check outcome is referenced separately.',
                  phase='business_ready_pending_frozen_six', implemented_backlog_ids=['B06','B07','B10','B11','B12','B13'],
                  implemented_backlog_origin='Inherited production implementation, independently browser-regressed in this task; no new product delta.',
                  changed_files=sorted(set(changed)), checks=checks, limitations=limitations, deferred=deferred,
                  source_identity=ref(q / 'source-before.json'), evidence_index='reports/service-ui/user-scope-1/evidence-index.json',
                  final_read_only_replay=final_replay, local_integrity=integrity,
                  previous_failure_archive='reports/service-ui/user-scope-1/history/result.json',
                  formal_verify='host owned; not invoked or awaited', usage=None, cost=None)
    save(ROOT / 'reports/service-ui/result.json', result)
    text = f'''设计结论（direction）：当前范围的业务交付已准备好，送验建议待固化后的全部六条原命令实际结果确认。用户明确「读屏可以不用考虑，200%缩放暂时也不考虑」及任务提供的 event825 为本轮范围依据；两项均为 user_deferred/not_in_current_scope，passed=null、非阻断，不作为通过项。保留此前 passed=false 原文和哈希归档 history/result.json；没有只翻旧标志。本轮独立完成 ci、原 browser、原 build、原 build-modes 和 runtime，退出码全部0。生产源码全量清单与上一轮 manifest SHA 一致，没有发现剩余范围内需要新增源码修复的失败，因此不改生产组件、依赖或断言。方向保持克制、紧凑，保守用户场景为反复管理本机AI服务的操作者；首先扫描可运行/需行动数量和健康可信度。记忆点为同 snapshot 工具身份的并列数字；正面对标 VS Code Settings 紧凑设置行，沿用项目令牌，不加载未固定外部技能。

结构与状态（state_coverage）：五 tab 的四向键/Home/End、深链和唯一 acp tabpanel；服务术语、默认来源作用域、同 snapshot id 去重和冲突 diff；表单连接→能力与模型→高级与用量。每屏主操作为新增服务/当前步骤下一步/最终保存。ModalSurface.tsx:22/27/38/39 原生 top layer、背景隔离、焦点圈定、嵌套 Esc、焦点返回和忙态均保留真实 Chrome 键盘断言。AiProvidersPanel.tsx:764/830/836/846/847 的 status/alert/aria-busy 语义保留；语义和CDP AX证据不冒充读屏语音。预设 radio、pressed/current、跨步错误定位和非密钥内容保留均由原入口验证。

加载：busy与最近快照；初始空：新增提示；筛选空：调整关键词或能力；错误：健康未知、局部重试；部分可用：degraded目录不阻断配置；成功：健康更新及保存；无权限：明确授权提示且配置仍可用。初始空/筛选空分别截图。健康读取失败和无权限时，两项全量服务（对话及仅语音）均保留，重试恢复。六类状态适用且实际覆盖，没有把环境未验证记为不适用。

取值与依据（token_values）：styles.css:4774 起字号12/13/14px、间距4/8/12/16/24/32px、圆角4/6/8/10px；shadow-1/2/3项目档位，弹窗shadow-3为0 12px 24px rgba(17,24,39,.10), 0 24px 48px rgba(17,24,39,.12)。通用动效100/160/220ms，沿用项目ease；减少动效opacity 80ms、无位移缩放。ServiceUi.css容器479.98px以下动作下排，命中区至少24px、预设32px、焦点outline 2px/offset 2px；正文≥4.5:1、边界/焦点≥3:1。依据为既有项目令牌、ModalSurface和任务固定验收，未新增设计令牌或声称整站AA。当前合成色、字号、命中区实测值见浏览器JSON。

验证（render_evidence）：Chrome 157.0.8103.50，生产React组件、受控虚构API，不是已安装/真实后端验收。320/390/480/620/880px、长URL、50%膨胀、五tab与模态、预设、跨步错误、字号、对比度、降级、深浅系统偏好和减少动效均执行原断言；41张图在 {browser}。另以独立评审立场目视 services-320、services-880、expanded-form-320、full-service-list-denied：320px主操作独占下一行，膨胀表单的底部按钮可见且正文滚动，两项服务在无权限时保留。B13 UI-006保留真实浏览器摘要布局、嵌套296px容器与窄表单测量；历史 reports/service-ui/repair3/browser-1794501571165 不动。未修改的CSS zoom fixture仍随原脚本运行，其证据仅补充，不作本轮200%或native通过项。未启动NVDA/Narrator、不执行原生zoom或Computer Use，也不换通道重试先前停止的原生操作。成功、故意1ms deadline及无效浏览器channel均核对cleanup：browserClosed/serverClosed/profileRemoved=true；预期负例exit=1和完整日志保留，未算业务失败或吞异常。

源码修复与差异：原 Refresh preamble 在 service-ui-browser.mjs:40；旧check-service-ui.browser.mjs:1转发原service入口，未依赖QA修正版副本。原构建修复在service-ui-build.mjs:8/13/14/27/30：未设置变量走普通dist；显式根走createRuntime、外部dist；TS7经公开typescript/package.json/bin.tsc解析。实际之前差异见 independent-qa-1/source-diff.patch、repair11/source-diff.patch，逐项当前哈希核对见 source-review.json。本轮没有新生产差异。原build-modes入口在候选直接执行，从当前源码自行复制、安装、清旧产物，普通模式验证新鲜非空frontend/dist/index.html和实际JS/CSS引用及mtime/SHA，有效根验证外部输出与本地dist缺失，relative/missing/file/workspace/ancestor五类无效根非零且无回退。全部7场景成功，原始子命令/整数退出码/log SHA在 {modes / 'build-modes.json'}。历史额外嵌套副本的exit=1属于TS7 #getExePath ERR_PACKAGE_IMPORT_NOT_DEFINED；本轮不再重复嵌套，原入口直接成功，未改依赖/解析/模式断言。createRuntime缺变量仍按runtime测试失败，无候选回退；普通build的缺变量是明确保留的另一模式。

消费者与更正轨迹：只读核实tauri.conf.json:6 frontendDist=frontend/dist，scripts/deploy-local.ps1:238起先npm、再Cargo；两者依赖新鲜本地dist，显式宿主外部构建不能单独证明发布消费。未修改或运行部署文件。repair9/delivery.md原“没有发现必须新增源码修复”不能推广为普通构建或原service Refresh正确；既有repair11和independent-qa-1追加更正原样保留，本报告再次确认真实缺口和已落实源码。旧原生缺证据曾使passed=false；本轮是新范围和新执行证据，不抹去旧失败。

历史与归属：:559/:566/:573/:589/:601/:612/:619均是历史宿主记录，与本任务新日志分开；未读取其他任务外部缓存或CONTROL。delivery-8保留 worker exit=1、cli_error_event=error、error_notifications=6、terminal_event=turn.failed、completed=false、final_observed=true、failed=true，不推为本次业务检查失败；delivery-10为输入超长、运行前阻断。空test_summary不换算TAP；usage/cost保持null。两保护文件只读HEAD/原始manifest/provenance比较见protected-provenance.json和本任务外部diff：docs初始及HEAD缺基线，dispatch当前hash未匹配已记录after，归属交宿主；未覆盖并行改动、不宣称protected_file_check通过。

固化与六项检查：本报告属于写作阶段；随后原qa-stage freeze将报告、源码、依赖哈希和授权候选文件集合固化。全部六条原命令将以原字符串重跑，ci在相同源码/lockfile/依赖/结果报告的本任务副本，browser/build/modes在候选直接运行。不得在检查期间改报告；实际结果以 {final_replay} 为准，每项包含command/cwd/整数exit/log SHA/source identity；逐项及最终文件集合与内容比较 {integrity}。该比较只作本地可追溯完整性，不冒充host:candidate-integrity。最终外部结果全部成功才能在返回值建议本阶段就绪；失败则保留真实失败并另起可追踪固化，绝不预填六项成功。

假设与风险（unverified）：重复使用场景采用保守假设；真实后端/凭据/模型交接/安装态/部署未验，受控API fixture独立标注。深色偏好保持项目固定light，不声称暗色主题。既有npm四项high advisory和chunk警告保留。上一轮OS读屏标志精确恢复未验证是历史限制，本轮不操作系统设置。没有提交、推送、部署或全Mission/跨执行器成熟度签字；正式host保护检查和独立VERIFY由宿主后续执行，本轮正常返回，不调用、不等待、不冒充。
'''
    (REPORT / 'delivery.md').write_text(text, encoding='utf-8')
    print(json.dumps({'passed': True, 'phase': result['phase'], 'result': 'reports/service-ui/result.json',
                      'evidence': 'reports/service-ui/user-scope-1/evidence-index.json'}))


if __name__ == '__main__':
    main()
