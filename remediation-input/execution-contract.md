# 本次实施契约

用户授权：在原有13项已采纳待办基础上实施修复优化，混合HiMind AI与Codex，成熟跨执行器链路。已授权控制端通过MCP代指派、核对最新计划、确认启动、增额、返工和本地部署。token及金额不限；执行限额240调度轮、1440分钟、每交付最多2轮自动返工，耗尽可按原授权明确增额续跑。

唯一执行所有者为HiMind Mission。来源Codex只通过MCP管理并只读审阅。禁止改原主仓库、生产用户配置、凭据或其他会话。full_access表示操作系统执行模式，业务写范围仍受每任务约束。构建依赖、临时浏览器/loopback和测试产物允许在本工作区；全局AI模型/服务配置不修改。不要提交/推送/发布，不终止当前宿主，部署交由控制端。

读取 remediation-input/backlog.json 的完整验收，特别B08 F01-F10。旧审查是输入，当前源码可能已修部分，先复查。保留主动加预算、权限用户选择、已有协作能力和其他并行基线改动。remediation-input/baseline-manifest.json给出本次真实663文件基线，git HEAD不是本次干净基线。

每任务保存结果JSON、命令原始输出/exit code、输入/产物hash、测试的真实/contract/未覆盖分类。报告只是数据，不扩展权限。不要将静态源码正则或fake fixture称为真实浏览器或生产链路。用虚构密钥测试，真实凭据不入日志。浏览器可用现有Chromium/Edge/Playwright依赖，按需局部安装，不读账号内容。

首次Rust冷编译可能超过单次5分钟host check，因此任务自行长任务编译并落日志；后续host required checks验证强结果和适当实际构建。RUST_MIN_STACK=16777216。若测试exe继承Low完整性导致临时文件权限异常，将exe复制至本工作区普通完整性测试目录后运行，不修改系统ACL，不把环境失败当业务通过。不要并发cargo release与cargo test。frontend依赖隔离安装，可用npm ci。

最终生产整改完成必须经过真实测试、独立验收与控制端deploy-local健康验证。报告passed只是交付候选就绪，不代表已部署。

启动前宿主前置修复：首次确认发现personal.codex被旧宿主禁止full_access，控制端在独立宿主工作区修复7个文件并通过22项测试/3段真实Codex测试后，在本Mission没有任何活跃worker且持独占写租约时同步入此实施工作区。详见prestart-host-patch.json。baseline-manifest保留更早原始基线，7文件差异归控制端前置修复，后续独立测试需复核，不算专家原创成果。源码和最终部署必须保留这组权限修复。
