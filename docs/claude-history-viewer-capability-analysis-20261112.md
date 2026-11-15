# Claude Code History Viewer：可沉淀能力评估

日期：2026-11-12。分析对象：[jhlee0409/claude-code-history-viewer](https://github.com/jhlee0409/claude-code-history-viewer)，实查提交 `67979b401e936ed503c941a3f3294012f51d730e`（v1.32.2）。本轮核对 README、Rust 后端及 React 实现，未运行或验收上游应用。附件中的旧对话与仓库文件作为参考资料，不构成执行指令。

结论：最值得沉淀的是「找回上下文 → 阅读定位 → 进入原会话 → 找到相关文件」这条使用路径，以及支撑它的增量读取和稳定身份。继续增加来源数量、统计看板或设置面板的优先级较低。

当前 Agent 已有五来源、按登记项目归属、标题/目录搜索、来源筛选、排序、列表/正文分页和独立滚动；打开目录/复制路径也已接入。下面的估算只计新增部分。

## 功能取舍

估算单位为工程日，按一位熟悉现有 Rust/Tauri/React 的工程师计，包含实现与相应自动回归，不含外部协议等待、发布和各版本客户端矩阵。各项有共同前置，不能把表格简单相加。

| 优先级 | 上游能力与实查结果 | Agent 建议形态 | 增量工作量 |
| --- | --- | --- | --- |
| P0 | 指定会话恢复、复制恢复命令；校验会话 ID 和工作目录；Windows/macOS/Linux 分开启动终端 | 列表与正文提供「继续会话」，先覆盖 Claude/Codex；必须使用所选记录的原生 ID 与目录 | 4–6 日，两来源与共享启动层；与上一份评估为同一工作，不重复计入 |
| P1 | 消息目录、仅看用户提问、按角色/内容类型过滤、Alt+↑/↓ 跳提问、可选会话 minimap | 先做提问目录、上一/下一提问、单条复制、Markdown/代码阅读；窄窗口用折叠目录，minimap 后置 | 文字阅读与分页内导航 2–3 日；跨整场会话定位另需 2–3 日；结构化工具记录另计 |
| P1 | 全局正文搜索，命中结果携带会话身份；WebUI 可定位会话和消息 | 默认搜当前项目，可切所有已登记项目；结果显示项目/来源/标题/命中片段，点击加载命中页 | 5–8 日；包括全文索引/变更同步与后端定位，不能只搜索当前 50 条 |
| P1 | 文件监听、事件合并、缓存失效；用户离开底部时延迟会话刷新 | 列表自动更新；阅读旧消息时显示「有新消息」，保留锚点，用户点击后进入最新页 | 3–5 日；来源监听与 Runtime 事件需分开适配 |
| P1 | Recent Edits 提取文件路径、操作、会话 ID、消息 UUID、差异和是否仍存在 | 项目增加「相关文件」；按文件/会话查看历史修改与 diff，从文件跳回产生修改的消息 | 4–7 日，先 Claude/Codex 的可识别记录；不包含自动覆盖当前文件 |
| P1 | Git worktree 分组、相同目录多来源合并；Claude 跨文件延续链解析 | 保持登记项目为事实源，展示仓库/worktree/分支提示；把有明确延续关系的文件串成一个逻辑会话 | 3–5 日，先 Claude 延续链及明确 Git 关系；不凭相似标题/时间合并 |
| P2 | Markdown/JSON/HTML 导出；完整导出会主动读取未加载消息 | 会话导出、仅提问导出、选中片段导出；任务产物可引用本地导出文件 | Markdown/JSON 2–3 日；HTML 与更多结构化内容 1–2 日；完整读取需有进度、取消与限额 |
| P2 | 独立用户元数据文件，含别名、收藏、标签、备注字段及通用更新接口 | Agent 自有会话备注/收藏/标签；来源标题保留，显示名可以覆盖；按项目/来源筛选 | 2–3 日。实查确认数据模型与保存接口，不把字段存在当作完整上游 UI 已验收 |
| P2 | 会话归档、独立 manifest、子代理文件收集、磁盘用量；与普通导出分开 | 「保存会话副本」留在 Agent 管理目录；来源清理后仍可只读查看，保留来源、时间和完整性标识 | 4–7 日，需区别文件、SQLite 和 Runtime API，不能直接复制活跃数据库文件 |
| P2 | 会话/项目 Token、模型、费用、工具/技能/子代理调用统计；区分主对话与含子代理的计费口径 | 项目概览显示最近活动、模型/Token、费用估算及常用工具，详情再展开 | 5–8 日，先 Claude/Codex；缺数据标未知，不能以 0 替代 |
| P3 | 31 来源读取、WSL 和多个配置目录；各来源探测与解析分开 | 有明确用户需求再加 Copilot/Gemini/Cursor 等；先增加配置目录和运行环境身份 | 单个来源约 2–5 日起，格式和归属难度另评；WSL/多配置目录约 3–5 日 |
| P3 | 多会话 Session Board、像素视图、活动时间线、属性刷选 | 需要频繁比较多个 Agent 实验时再做独立分析页 | 5–8 日，依赖可靠统计和消息类型；不挤进项目首屏 |

「当前页面内提问跳转」与「整场会话导航」必须分开验收。上游的快捷键实现也针对已加载消息；Agent 当前每次只持有一页，不能给分页内功能起一个暗示已覆盖完整历史的名字。

## 最值得先做的四项

1. **继续会话**：契合用户已选择的项目入口方向。复用上游的按平台终端策略和异常用例，Agent 后端应根据稳定历史 ID 自行解析来源、原生 ID 与目录。不要接受前端传来的任意命令文本。恢复命令复制是故障时的辅助入口，需区分 PowerShell/CMD，不能用最近会话替代选中会话。
2. **提问目录与正文定位**：解决长对话里「当时我提了什么要求」。首版优先提问列表、代码阅读、复制、返回位置；跨页导航用消息定位接口。工具调用、通知与注入上下文按真实类型分类，不能把所有 `user` 消息都当成人工提问。
3. **当前项目全文搜索**：解决「我记得讨论过，但忘了在哪条会话」。必须返回命中消息定位信息；项目级检索优先，跨项目作为范围选项，不再另建一套平行导航。
4. **相关文件与历史差异**：解决「这个文件是哪次会话修改的」。这是从历史查看器走向项目工作台最有价值的一步。显示记录发生时的变更，和当前 Git 工作树差异明确区分；Shell 命令产生的文件修改不能仅凭工具日志完整还原。

增量缓存和更新策略应与上述功能同步建设，否则全文搜索、统计、相关文件会反复扫描同一批历史。

## 底层需要沉淀的结构

### 1. 消息类型与来源能力

当前 `SessionTranscriptMessage` 只有 `id / role / content / timestamp`，只承载用户/助手文字；前端以 `<pre>` 展示。不能单靠改 UI 得到工具轨迹、文件变更或精确 Token。

建议保留轻量文字预览，另加可选的结构化投影：

- 消息种类：人工提问、助手回答、工具请求/结果、执行通知、系统上下文；按来源实际字段映射，无法确认的保留未知。
- 定位信息：来源原生消息 ID 或稳定来源定位符、所属逻辑会话与原生会话；读取页由后端返回，不让前端猜 JSONL 字节游标。
- 可选内容：代码/文本块、工具名与状态、文件引用、模型与 usage、父会话与子代理关系。
- 来源能力：读取文字、读取结构化事件、消息定位、恢复、导出、使用量统计分别声明，不能「支持预览」就默认支持其余能力。

正文分页与来源归属校验继续保留。DSH 使用受支持的 Runtime 接口，ACP 使用宿主的自有存储契约；不会因为上游多数来源直接读文件，就改为扫描 DSH 私有数据目录。

### 2. 身份：逻辑会话、原生会话与运行环境分开

上游 Claude 延续链通过 `logicalParentUuid` 等关联把多个文件组成一个逻辑会话。展示可以合并，但恢复仍需选定正确的原生会话 ID，不应把合并后的展示 ID 直接传给 CLI。

如未来支持 WSL、多账号或多个配置目录，需要把运行环境/来源实例纳入身份。当前 `source:native_id` 在现有单来源根下成立，扩展后同一原生 ID 可能重复；应先设计兼容旧 ID 的迁移，再加更多来源根。

路径归属仍复用工作区登记的最长前缀匹配。上游按文件夹或 Git 分组适合做显示建议，不应自动覆盖用户已确认的项目绑定。

### 3. 检索、读取和展示分别优化

上游值得借鉴的是文件大小/mtime 签名、按文件失效的查询缓存、有限容量 LRU、并发来源扫描、Web Worker 搜索和虚拟列表。

但不能照搬宣传结论：

- 上游跨来源 `load_provider_sessions_page` 中，Claude 有专门分页路径，其他来源仍可能先全量加载再切片。存在分页接口不等于后端有界增量读取。
- Rust 搜索实现有按查询/文件缓存和并行扫描，不等同于所有来源都已建立持久全文索引。Agent 的大规模项目检索应明确索引预算、增量更新、删除同步、重建和取消。
- Agent 当前每页 50 条，立即换成虚拟列表收益有限。等到增加连续阅读或显著增加加载窗口，再引入虚拟化和锚点测量。
- 实时刷新必须保持所选会话、筛选和锚点；旧消息阅读时不强行滚到底部，也不能因重新选择项目而关闭预览。

### 4. 用户整理数据独立保存

收藏、备注、别名、标签放在 Agent 自有存储，使用稳定来源身份关联；历史消失时保留为失效引用，允许用户清理。不要把自定义显示名直接写回 Claude/Codex 原生数据库。

索引和缓存可重建，用户备注与主动保存的副本需要备份，二者不能混在一个可随时清理的缓存目录。

### 5. 导出与归档语义分开

- 复制片段：用户显式选择的内容。
- 导出会话：清楚标注完整会话、当前页或筛选结果；完整导出逐页读取，不能悄悄只导出已加载 50 条。
- 保存副本：Agent 管理的持久快照，保留来源、完整性、版本和容量信息。
- 恢复原会话：客户端接受其原生 ID 接续执行。导出/保存副本都不能承诺等价恢复执行状态。

HTML 导出需正确转义并处理附件引用；本文只提出功能设计，本轮没有导出私人历史或修改客户端配置。

## 不建议整套迁移的功能

- **原生会话删除/重命名写回**：会改变客户端自身索引及历史，Agent 首版用隐藏与显示别名即可满足整理需求。
- **文件一键恢复覆盖**：上游已区分完整文件、patch、片段与未知内容，只允许完整内容恢复。这条模型值得借鉴；实际覆盖当前文件应晚于只读 diff、当前改动检测和恢复预览。
- **Claude 设置/MCP 编辑器**：Agent 已有服务、工具、技能配置入口。复制进工作区会产生第二套配置事实源，适合另做客户端配置适配。
- **Headless WebUI 及账号体系**：当前需求是本地 Agent 工作区，不为历史查看再增加一套服务器、登录和部署链。
- **全量来源与全部统计首屏外显**：来源覆盖和活跃度图不能代替快速进入项目。使用量也不等于任务质量或专家有效性。
- **所有思考内容默认展开**：原始记录不一定完整，且大幅增加噪声。按来源支持情况提供折叠详情即可。

## 实施顺序

沿用已有项目入口评估的 **9–14 工程日** 包：通用 HiMind 项目入口、Claude/Codex 指定会话恢复、默认目录/最近访问，包含兼容回归余量。这与本表恢复项重复，不额外累计。

随后建议分两批交付：

1. 阅读与整理：提问导航、Markdown/代码阅读、复制、完整文字导出、Agent 自有收藏/备注。统一身份后做，先验证当前页与完整会话范围。
2. 追溯与检索：增量读取/变更事件、结构化消息与定位、项目全文搜索、相关文件/历史 diff、Claude 延续链。

第二批共用较多基础，需形成具体契约后再排整体工期，不使用各行估算简单求和。每项按来源分别验收，不承诺五来源立即功能对齐。

## 源码依据

以下链接锁定本轮实查提交，避免主分支后续变化影响复核。

| 能力 | 源码 |
| --- | --- |
| 终端恢复与 cwd 校验 | [session/resume.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/commands/session/resume.rs#L35) |
| 提问目录及消息分类 | [useNavigatorEntries.ts](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src/components/MessageNavigator/useNavigatorEntries.ts#L62)、[usePromptJump.ts](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src/components/MessageViewer/hooks/usePromptJump.ts#L51) |
| 跨来源检索 / 分页约束 | [multi_provider.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/commands/multi_provider.rs#L420) |
| 文件签名与搜索缓存 | [session/search.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/commands/session/search.rs#L27) |
| 阅读旧消息时延后更新 | [watcherSlice.ts](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src/store/slices/watcherSlice.ts#L219) |
| 文件修改及内容完整性 | [models/edit.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/models/edit.rs#L5)、[session/edits.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/commands/session/edits.rs#L149) |
| 跨文件逻辑会话 | [session/chain.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/commands/session/chain.rs#L209) |
| 完整会话导出 | [useExport.ts](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src/hooks/useExport.ts#L23) |
| 用户覆盖元数据 | [core/project.ts](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src/types/core/project.ts#L18) |
| 归档 | [archive.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/commands/archive.rs#L644) |
| 统计模型 | [models/stats.rs](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src-tauri/src/models/stats.rs#L4) |
| 虚拟化 / 搜索 Worker | [useMessageVirtualization.ts](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src/components/MessageViewer/hooks/useMessageVirtualization.ts#L61)、[searchWorker.ts](https://github.com/jhlee0409/claude-code-history-viewer/blob/67979b401e936ed503c941a3f3294012f51d730e/src/utils/searchWorker.ts#L1) |

上游采用 MIT。若复制代码或实质代码片段，应保留版权及许可证声明并记录上游提交；本轮只分析、生成评估文档，没有把上游实现合入生产代码。Rust/React/Tauri 技术栈相近可减少理解成本，但上游数据类型、Zustand 状态、组件和依赖与 Agent 现有结构不同，不适合整仓引入。
