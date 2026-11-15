本阶段交付范围为 B08/B09 的两个 P2 与中断续接核对；只建议送验，不签署 Mission 完成、独立验收或宿主正式 VERIFY。冻结后的六项原必需检查结果写入本任务资源根 frozen-replay，不回写候选。

## 设计结论（direction）

保守场景：桌面操作者反复管理多专家交付，先扫描“谁正在处理、下一步是什么”。沿用项目克制、紧凑的工具布局。记忆点是“当前步骤可收尾 / 后续派发已暂停”与“实现结果 / 必需检查 / 独立验收”互相对应。正面对标 GitHub Checks 的检查与审查分区，仅作结构参照，未联网实测。未加载宿主未固定外部技能，未引入依赖或改变固定执行绑定。

## 中断核对

本轮初始源码哈希与 user-scope-1/manifest.json 一致：missionView.ts、MissionEvidence.tsx、MissionTeamPanel.tsx、原浏览器入口、missionSnapshot.ts、CollaborationDialog.tsx。初始 result.json 与 paused-verification-2/history/previous-result.json 内容哈希相等，均属于 repair-mission-ui-user-scope-1；未发现 -2 新签出的交付报告。response_stream 中断只表明运行中断，既不证明产品失败，也不证明两个 P2 已交付。

已存在且保留：预算等待墙钟、时限到达/超过、弹窗重开、修复谱系、事件分页与全局序列间隔、晚到快照防覆盖、legacy 手动续跑统一、Refresh preamble、普通键盘与原生模态焦点处理。没有重新实现这些已正确功能。初始源码仍缺：waiting 下识别明确 running 的关联独立核验；准备事件 verification_checks_completed 的独立必需检查入口。本次在真实源码补足，并接入原实际组件浏览器入口。

## 结构与状态（state_coverage）

列表 → 详情 → 执行计划中的三类证据。独立核验只有在当前有效确认计划、Mission/任务所有权、不同于实现的当前 verification_run_id 和活动 status=running 同时匹配时才显示核验进行中；历史 verification_started、单独 run ID、无归属/外来/未知状态不证明运行。列表、待办、详情、任务行、责任人、下一步、CTA 采用同一判断。running 时主操作为查看验收详情，无重复启动、续跑核验或继续执行下一项入口；waiting 仅暂停后续派发，不声称自动派发已恢复。结束/失败活动消除 busy；有宿主准入且无在途运行时仍遵循原手动续跑契约。预算、环境、权限、未知关联与手动暂停限制保留。

必需检查从当前任务和 implementation_run_id 的准备窗口读取，展示事件 ID/时间、检查前后 candidate ID、命令、退出码、原输出摘要及摘要哈希。缺边界、未知边界、候选失配、同一窗口多个候选、其他实现重试/任务/Mission 均不串用；原始记录保留在历史。checks 事件没有 plan_sha256 或实时文件身份，前端只展示检查时冻结候选，不推断当前文件仍一致。新准备窗口由关联 preparing / retry / implementation 事件限定。host_verification_checked 单独作为独立验收，按计划、任务、实现 run、核验 run 与当前可确认检查候选核对；其他候选的 verdict 只留历史。旧事件缺候选边界明确标缺，仅按原任务/run 关联展示历史验收内容，不伪造候选证明。检查全绿不等于独立验收通过，TAP 数值总数未单独提供时不编造。

六状态：加载显示读取提示；初始空与筛选空沿用原提示，证据缺失就近说明；读取错误/无权限保留 alert 和重试；部分交付可打开已有证据；成功分别记录实现、必需检查与独立验收。权限拒绝是传输契约样本，生产账号权限链未验证。

## 取值与依据（token_values）

沿用 frontend/styles.css，不新增视觉令牌：字号 12/13/14px、正文行高 1.6、间距 4/8/12/16/24/32px；控件圆角 6px、浮层 8px；证据不加阴影。主按钮至少 36px、证据 summary 至少 40px、焦点描边 2px；既有 reduced-motion 禁用无限动画，opacity 100ms。状态轮询 5000ms、墙钟心跳 1000ms。正文目标对比度 4.5:1，未据此声称整页 AA。未改变令牌，所以本任务没有新配色测量结论。

依据：只读 src/mission/store.rs 的 pause 事件“用户暂停后续派发，当前步骤可收尾”；src/app/mission_dispatch.rs 在宿主检查前和核验前检查 Mission active，完成 checks 后仍持久化 verification_checks_completed，且复用保存的检查输入，不能重复重跑；host_check_boundary 区分检查前后候选；host_verification_checked 独立记录专家结果。项目设计约束优先于个人视觉偏好。

## 验证（render_evidence）

原 node frontend/scripts/remediation/mission-ui-clock-evidence-browser.mjs 使用生产 React、计时器、轮询、分页、agentApi 与真实组件，只有原生传输被 DTO 样本代替；没有复制业务投影逻辑。新增 active→waiting 不重载转换、明确中文文案、DOM、责任人/CTA、待办排除、禁止重复调用、结束与失败活动、边界缺失和外来记录、检查准备/暂停/执行、后续 verdict 更新与失败历史；截图、transport.jsonl、paused-checks-dom.json 和 trace 均在本任务根。保留原预算、10005 条分页/全局间隔、无新事件、乱序响应、部分/失败/完成、legacy 反例、状态语义、普通键盘、焦点、模态与背景隔离。

目标视口 1280×900、390×844、320×844；保留补充 Store 640px 与服务 320/390/480/620/880px 非缩放回归，长路径、深浅系统偏好与减少动效。真实 Store 探针从本任务根新构建原只读源码，生成物、Cargo lock、输入哈希均保留；没有读取其他任务 cache 或修改 Rust。

读屏语音和全部 200% 缩放均 user_deferred、passed=null，不计通过。执行偏差如实保留：首次准备运行器复制了旧补充检查，执行了旧 CSS 200% fixture；这些输出不计延期项通过。后续仅在本任务生成副本中跳过精确缩放段，保存原/生成哈希和删除的逐字片段，所有非缩放断言保留，原其他任务脚本不变。最终原 Mission 入口不再执行缩放。

QA-LEGACY-01：保留有资格时明确手动续跑，与在途 running 例外区分，禁止恢复猜测或自动恢复。QA-HARNESS-01：原入口已有 Refresh preamble，原非延期断言保留并真实执行。QA-LIVE-01 未豁免，是下游实际执行器/安装验收门禁，本前端 worker 不冒领通过。

## 假设与风险（unverified）

使用场景为保守假设，未做访谈；真实活动状态与宿主准入真实性由宿主负责，UI 不扩大权限。分页读取事件不是完整执行账本，历史总数只指已读取。固定 light 调色板不等于真正深色主题；系统 dark preference 回归不声称深色配色通过。未验证安装版 Tauri/MCP、真实 HiMind↔Codex 模型交接、生产部署、完整无障碍符合性、正式 VERIFY。当前任务仅送验准备，正式 VERIFY 尚未发生不构成本阶段失败理由。

作者实现后从评审者立场复核：收紧其他候选 verdict、活动所有权与重复派发，补充真实组件反例。这是作者复核，不冒充独立专家签字。旧报告/失败证据与 :858/:860/:872 的层次见 source-review.json 与 result.json，原始日志不删除。冻结后不修改源码、报告或既有证据，最终退出码与身份不变性写入外部 frozen-replay。
