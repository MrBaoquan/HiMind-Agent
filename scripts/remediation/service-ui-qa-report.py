"""Explicit report-authoring phase, never invoked by frozen checks."""
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
RUN = RUNTIME / 'q'
DELIVERY = ROOT / 'reports/service-ui/independent-qa-1'


def read(path):
    return json.loads(path.read_text(encoding='utf-8'))


def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def runtime_directory(label):
    text = (RUN / (label + '.log')).read_text(encoding='utf-8')
    return Path(re.search(r'^SERVICE_UI_RUNTIME_DIRECTORY=(.+)$', text, re.M)[1].strip())


def main():
    DELIVERY.mkdir(exist_ok=True)
    phase = sys.argv[1]
    before = (RUN / 'original-browser.mjs').read_text(encoding='utf-8')
    current = (ROOT / 'frontend/scripts/remediation/service-ui-browser.mjs').read_text(encoding='utf-8')
    preamble = ('<script type="module">import RefreshRuntime from "/@react-refresh";'
                'RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};'
                'window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script>')
    normalized = current.replace(preamble, '').replace("from './service-ui-", "from './remediation/service-ui-")
    normalized = normalized.replace("import.meta.url)), '../..');", "import.meta.url)), '..');")
    assert normalized == before, 'Original assertions/scenarios/failure propagation must be byte-preserved after normalization'
    diff = ''.join(difflib.unified_diff(before.splitlines(True), current.splitlines(True),
                   fromfile='inherited/frontend/scripts/check-service-ui.browser.mjs',
                   tofile='current/frontend/scripts/remediation/service-ui-browser.mjs'))
    (DELIVERY / 'source-diff.patch').write_text(diff, encoding='utf-8')
    browser = runtime_directory('original-browser-direct')
    modes = runtime_directory('current-modes-2')
    check_labels = ['current-ci', 'original-browser-direct', 'original-build-direct', 'current-modes-2']
    if phase == 'final':
        check_labels += ['result-probe-1', 'result-probe-2']
    checks = [read(RUN / (name + '.json')) for name in check_labels]
    changed = [
        'frontend/scripts/check-service-ui.browser.mjs',
        'frontend/scripts/remediation/service-ui-browser.mjs',
        'frontend/scripts/remediation/service-ui-native.mjs',
        'scripts/remediation/service-ui-qa-stage.py',
        'scripts/remediation/service-ui-qa-report.py',
        'reports/service-ui/result.json',
        'reports/service-ui/repair9/delivery.md',
        'reports/service-ui/independent-qa-1/delivery.md',
        'reports/service-ui/independent-qa-1/evidence-index.json',
        'reports/service-ui/independent-qa-1/source-diff.patch',
        'reports/service-ui/independent-qa-1/protected-provenance.json',
    ]
    limits = [
        'QA-A11Y-01 unresolved: native Chrome 200% and product NVDA/Narrator speech not verified. CSS zoom/CDP AX/keyboard evidence is separately classified.',
        'Computer Use stopped after refusing to verify current browser URL allowance. No bypass or further UI automation attempted.',
        'Portable official NVDA 2025.3.3 actually ran with task-only config and Speech Viewer; speech log observes tool usage prompt only. This does not establish product announcements or background isolation.',
        'NVDA task processes were terminated; original user config was not loaded. OS screen-reader flag restoration was not snapshotted, so exact restoration is unverified.',
        'Six checks must still run without modifying reports. The passed=true assertion is expected to fail honestly because required native evidence is missing; no formal VERIFY is invoked.',
        'npm ci runs the exact command in hash-identical generated source copy, keeping dependencies and caches in task resources; original candidate browser/build/modes entries run directly.',
        'Production React components use original fictitious API fixture, not real backend, credentials, model handoff or deployment.',
        'Dark/light media preferences tested with project fixed light palette; no independent dark palette claimed. Repeated-use scenario is conservative assumption.',
        'Prior QA observed Refresh failure; current before-fix isolated run succeeded. Explicit preamble fixes initialization contract, without claiming current reproduction of the same exception.',
        'Protected provenance JSON lacks HEAD/initial manifest baseline; dispatch current SHA matches no recorded host after hash. Ownership unresolved; no protected_file_check pass claimed.',
        'Four high npm advisories and existing chunk-size warning retained. Usage/cost unknown; historical empty test_summary is not a numeric test count.',
        'No commit, push, deploy, Mission-wide acceptance or independent formal signoff. Other producer business code unchanged.',
    ]
    result = dict(passed=False, task_id='repair-service-ui-independent-qa-1',
        summary='QA-HARNESS-01 original service entry repaired and browser/build regressions pass; QA-A11Y-01 native zoom/product speech evidence blocked, delivery not ready.',
        implemented_backlog_ids=['B06', 'B07', 'B10', 'B11', 'B12', 'B13'],
        implemented_backlog_origin='Inherited production components retained and browser-regressed; this task changes harness/evidence only.',
        changed_files=changed, checks=checks, limitations=limits,
        evidence_index='reports/service-ui/independent-qa-1/evidence-index.json',
        final_read_only_replay=str(RUN / 'final-checks.json'),
        local_integrity=str(RUN / 'local-integrity.json'),
        formal_verify='host owned; not invoked or awaited', usage=None, cost=None)
    write(ROOT / 'reports/service-ui/result.json', result)
    provenance = read(RUN / 'protected-provenance.json')
    write(DELIVERY / 'protected-provenance.json', provenance)
    inherited = read(RUN / 'inherited-result.json')
    historical = {
        'classification': 'historical report references only; other task external evidence not read',
        'prior_report': str(RUN / 'inherited-result.json'), 'prior_report_sha256': sha(RUN / 'inherited-result.json'),
        'events': {':559': 'historical 0/1/1/1/1', ':566': 'historical 0/0/0/0/0',
                   ':573': 'historical 0/0/0/0/0', ':589': 'historical 0/0/0/0/0',
                   ':601': 'QA/feedback ordinary-build rejection; no current reproduction inferred from historical stdout',
                   ':612': 'original not obtained this task', ':619': 'historical six 0/0/0/0/0/1; stdout not obtained'},
        'delivery-8': dict(worker_exit=1, cli_error_event='error', error_notifications=6,
                           terminal_event='turn.failed', completed=False, final_observed=True, failed=True,
                           interpretation='worker history, not this task business check failure'),
        'delivery-10': 'input too long before business execution',
        'UI-006': 'reports/service-ui/repair3/browser-1794501571165 and inherited attempt4/zoom-dialog-before-next.json preserved; not reread outside allowed paths',
        'test_summary': 'empty historical values retained; no TAP inference', 'usage': None, 'cost': None,
        'inherited_checks': inherited.get('checks'),
    }
    files = [p for p in browser.iterdir() if p.is_file()]
    render = dict(directory=str(browser), runtime=read(browser / 'browser-runtime.json'),
                  screenshots=sum(p.suffix == '.png' for p in files),
                  manifest=[dict(path=str(p), sha256=sha(p)) for p in sorted(files)],
                  independent_visual_review=['services-320.png', 'expanded-form-320.png', 'zoom-200-pointer.png'],
                  observed='320px action stacks; inflated-copy footer remains visible; CSS zoom error focus visible. These are not native zoom/speech.')
    index = dict(task_id=result['task_id'], phase=phase, runtime_root=str(RUNTIME), checks=checks,
        source_changes=[dict(path=f, sha256=sha(ROOT / f)) for f in changed if not f.startswith('reports/')],
        source_manifest=str(RUN / 'current-source.json'), toolchain=str(RUN / 'toolchain.json'),
        assertion_preservation=read(RUN / 'assertion-preservation.json'),
        current_assertions_byte_preserved_after_import_path_and_preamble_normalization=True,
        browser=render, build_modes=dict(directory=str(modes), result=read(modes / 'build-modes.json')),
        native=read(RUN / 'native-ui-blocker.json'), native_cleanup=read(RUN / 'nvda-cleanup.json'),
        native_logs=[dict(path=str(p),sha256=sha(p)) for p in [RUNTIME / 'nv/native-nvda.log', RUN / 'nvda-speech-transcript.txt', RUNTIME / 'nv/stop.log']],
        negative_cleanup=read(RUN / 'error-cleanup.json'),
        failed_attempts=[read(RUN / (f + '.json')) for f in ['before-browser', 'current-modes', 'expected-timeout', 'expected-startup-failure']],
        before_fix_success=read(RUN / 'before-browser-2.json'),
        protected=provenance, historical=historical,
        final_read_only_replay=str(RUN / 'final-checks.json'), local_integrity=str(RUN / 'local-integrity.json'),
        unverified=limits)
    write(DELIVERY / 'evidence-index.json', index)
    minimum = min(x['textRatio'] for x in read(browser / 'form-contrast-and-boundaries.json'))
    boundary = min(x['boundaryRatio'] for x in read(browser / 'form-contrast-and-boundaries.json') if x['boundaryRequired'])
    delivery = f'''设计结论（direction）：当前交付未就绪，passed=false。QA-HARNESS-01 在原 service 入口修复：frontend/scripts/remediation/service-ui-browser.mjs:40 显式安装 Vite React Refresh，再导入生产组件；旧 check-service-ui.browser.mjs:1 仅转发到该入口。原 fixture、全部断言、场景、异常传播、超时及清理逐字保留（只归一化相对路径和 preamble 后与修复前全文相等）。此前独立 QA 的 $RefreshReg$ 失败保留；本任务修复前隔离运行实际成功，不声称复现同一异常。修复后原候选目录的原 service 命令直接 exit=0，不依赖 QA 专用修正副本。没有发现通过原生读屏确认的产品缺陷，因此未改业务组件。

方向保持克制、紧凑，保守场景为重复管理本机 AI 服务的操作者，首先扫描可运行/需行动数字及健康可信度。记忆点是同 snapshot 工具身份的并列计数。正面对标 VS Code Settings 紧凑设置行。项目令牌/原组件优先，不加载未固定外部技能，不做新产品决策。

结构与状态（state_coverage）：服务页单一主操作为新增 AI 服务；连接→能力与模型→高级与用量。五 tab 四向键/Home/End、深链、唯一 acp tabpanel；工具同 id 去重/冲突 diff；默认来源作用域、跨步错误定位、非密钥保留、预设 radio/pressed/current 均保留真实浏览器断言。六态实测包括加载、初始空/筛选空、错误可重试、部分降级、成功、无权限；健康错误/无权限保留对话及仅语音全量服务。ModalSurface.tsx:22 showModal、:27 焦点返回、:38 cancel/:39 Tab 逻辑以原有真实键盘和 CDP AX 验证；它们不等于 NVDA 读屏验收。

取值与依据（token_values）：沿用项目字号12/13/14px；间距4/8/12/16/24/32px；圆角4/6/8/10px；shadow-3 为 0 12px 24px rgba(17,24,39,.10), 0 24px 48px rgba(17,24,39,.12)。通用100/160/220ms、ease=cubic-bezier(.25,.1,.25,1)；减少动效为 opacity 80ms，无位移缩放。容器<480px动作下排，focus outline 2px/offset 2px；正文≥4.5:1、控件边界/焦点≥3:1、命中区≥24px、预设≥32px。当前表单最小正文 {minimum:.4f}:1、边界 {boundary:.4f}:1。依据是固定任务、styles.css:4774 起令牌、ServiceUi.css 和 ModalSurface，未声称整站 AA。

验证（render_evidence）：当前 ci/browser/build/build-modes 四条实际 exit=0，详见 result.json 逐项真实命令/cwd/整数退出码/日志SHA。ci 在哈希一致任务源码副本执行，原 browser/build/modes 在候选目录直接执行；所有生成物只在分配根。Chrome {read(browser / 'browser-runtime.json')['version']} 加载生产组件与原虚构 API。320/390/480/620/880px、长 URL、50%膨胀、CSS zoom=2 的指针键盘、深浅偏好/减少动效、字号/对比度与降级断言通过，共41张图。以第二遍评审立场目视 services-320、expanded-form-320、zoom-200-pointer：主操作下排；表单底部按钮可见；错误焦点在视口。B13 UI-006 原始及历史失败未删除。

构建入口与消费者：package.json:3→service-ui-build.mjs:8（缺变量普通模式；显式变量 createRuntime）。公开 typescript/package.json→bin.tsc 路径与依赖不变。runtime.mjs:7/22/30 缺变量、无效根仍失败；显式有效根外部输出。build-modes.mjs 原命令从当前源码生成本任务根副本：清理旧 dist，验证新鲜非空 index、实际 assets、哈希/mtime；有效显式根不写本地 dist；relative/missing/file/workspace/ancestor 五类无效根均非零且无回退。当前直接执行 exit=0。另一个额外嵌套源码副本尝试 exit=1，具体为 TS7 #getExePath ERR_PACKAGE_IMPORT_NOT_DEFINED，已保留 current-modes.log；去除检查包装器重复嵌套后原命令通过，未改 TS7/依赖/断言。首次 before-browser 资源配置为祖先根而 exit=1，同样保留；改用同授权根内独立兄弟输出后运行。tauri.conf.json:6 frontendDist=frontend/dist 与 scripts/deploy-local.ps1:238 起先 npm 后 Cargo 消费链仅只读核实，文件未修改，不宣称部署。

QA-A11Y-01（unverified）：官方 NVDA 2025.3.3（签名 Valid NV Access Limited；安装器 SHA 15b5e9864032565e7f895e4d80d767f180db0ec264e892240e675c426cfca921）在本任务资源根运行。最初官方启动器因深路径辅助 DLL/进程加载失败，原日志保留；7zip 提取同签名包到根内较短路径后启动 nvda.exe pid61764，原生观察到朗读查看器 window6751928、采集提示 window5312580，以及测试 Chrome window26678148。仅观察到 NVDA 自身提示语音（日志/转录保留），未观察到产品状态/警报、背景隔离、嵌套模态读屏行为，也未设置或证明浏览器工具栏200%。Computer Use 因无法核实当前浏览器 URL 是否获准，停止本轮原生操作；没有绕过。任务 Chrome/服务和 NVDA 进程已终止，只有本任务 config 被读取/写入；原始用户配置未加载，但 OS screen-reader flag 未事前快照，不能证明精确恢复。真实语音与原生缩放仍缺证据，不能 pass。

历史和归属：repair9 原“未发现必须新增源码修复”结论已追加更正，不覆盖原文；repair11 更正仍保留。:559/:566/:573/:589/:601/:612/:619 与本任务日志分开，只转述获准报告，不读其他任务外部缓存/CONTROL。delivery-8 worker exit=1、cli_error_event=error、error_notifications=6、terminal_event=turn.failed、completed=false、final_observed=true、failed=true 原样保留，不推为本次业务失败；delivery-10 是运行前输入超长。空 test_summary 不推算 TAP；usage/cost 未知。protected-provenance.json 中 docs 保护文件无初始/HEAD基线，dispatch 当前 SHA 不匹配记录的 after，归属不明；只读差异保留外部，未覆盖并行改动、不宣称 protected_file_check 通过。

固化与检查：本报告是明确写作阶段，随后 freeze 并只读执行全部六项原命令；预期 passed=true 的结果断言因真实原生证据缺口 exit=1，其余按实际记录，不预填成功。最终六项原始记录 {RUN / 'final-checks.json'}；每项前后授权文件集合/内容比较 {RUN / 'local-integrity.json'}。检查不修改候选/报告，外部日志保留。此比较不冒充 host:candidate-integrity；不调用或等待正式 VERIFY，不自签独立验收。本阶段 false 的原因是证据不足，不是未来 VERIFY 尚未运行。

假设与风险：具体频繁使用情境按保守假设。API为虚构隔离数据；真实后端、模型交接、凭据与部署未验。项目固定light而非独立暗色。npm四项high advisory及大chunk警告保留。没有提交、推送、部署，也没有宣布全Mission通过。
'''
    (DELIVERY / 'delivery.md').write_text(delivery, encoding='utf-8')
    correction = ('\n\n更正轨迹（repair-service-ui-independent-qa-1，2026-11-13）：保留 repair9/repair11 原文。'
                  '原“未发现必须新增源码修复”也未覆盖原 service 浏览器入口 Refresh 初始化缺口。'
                  '本阶段将全部原场景保留到原 service 入口并显式 preamble，原候选入口直接成功；'
                  '原生 Chrome 200% 与产品 NVDA 语音仍未验证，不能沿用历史 passed。'
                  '本次 passed=false，原因与工具阻断详见 ../independent-qa-1/delivery.md，历史日志未删除。\n')
    history = ROOT / 'reports/service-ui/repair9/delivery.md'
    if '更正轨迹（repair-service-ui-independent-qa-1' not in history.read_text(encoding='utf-8'):
        with history.open('a', encoding='utf-8') as stream:
            stream.write(correction)
    print(json.dumps(dict(passed=False, phase=phase, evidence=str(DELIVERY / 'evidence-index.json'))))


if __name__ == '__main__':
    main()
