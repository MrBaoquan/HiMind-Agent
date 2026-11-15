# 当前阶段交付说明

## 设计结论

按保守场景推进：本机操作者重复管理 AI 服务、默认来源与工具接入；先扫描可运行数、需行动数及健康目录是否可信。方向为克制、紧凑，记忆点是同一带时间快照中的可运行/需行动并列统计。正面对标采用本项目现有设置行，不引入外部产品风格、依赖或业务默认。保留已有实现及并行修改，具体输入差异见 baseline.diff；本轮增量不把旧实现算作新成果。

## 结构与状态

五页签保持工具接入、AI 服务、模型网关、能力路由、运行环境；页签由父页持有面板语义。服务页主操作为新增 AI 服务，错误恢复为局部重试健康目录。表单沿连接→能力与模型→高级与用量推进，早期字段错误可从末步摘要回定位，非密钥输入保留。原生 ModalSurface 负责嵌套隔离、Esc 和触发点返回，无 document 级表单 Tab 处理。

加载、初始空、筛选空、错误、部分可用、成功、无权限均在真实生产组件渲染并留截图/AX/行为断言；API 使用隔离虚构数据，不代表真实网络或后端 E2E。错误和无权限保留对话及仅媒体服务两行、未知健康及旧快照；重试恢复。工具重复 id 只有一行，可运行与需行动允许同一行同时计入；冲突先看字段差异，不自动写配置。

## 取值与依据

复用项目字号令牌：事实/备用模型 12px、名称 13px、局部标题 14px；间距沿用 4/8/12/16/24/32px，圆角及 shadow-3 不变。表单和服务页采样正文对比度≥4.5:1、输入边界/焦点≥3:1，交互命中区≥24px，预设标签≥32px；实际合成 RGB 和比值见各 contrast JSON，不推论整站符合 AA。减少动效下无位移/缩放动画；系统深色偏好沿用项目固定 light 配色，不新增未授权暗色主题。

## 失败复现与修复

1. 本轮 reproduction 在 service-ui-scenarios.mjs 的五页签循环后等待 acp 超时。原因是循环最后一次 ArrowRight 的 requestAnimationFrame 聚焦尚未提交就发送 ArrowLeft。新增等待实际焦点与选择同步，不降低任何场景断言。
2. 本地此前 repair2-extended-run4.log 指向 200% 指针点击被其他元素拦截。本轮 attempt2 重现；Edge 123 的最小页面确认 getBoundingClientRect 未反映 CSS zoom，elementFromPoint 命中 BODY 而非按钮。入口改用已安装 Chrome 157，保留失败启动非零退出，不使用 force 点击或跳过缩放。项目依赖由 npm ci 隔离安装；尝试下载匹配 Chromium 超时，下载子进程仅按本任务确认的 PID/命令行清理，未改全局配置。
3. Chrome 上 attempt3/attempt4 暴露生产布局真实缺陷：200% 时 modal 高1280px、top=-190、bottom=1090，下一步按钮 top=1000 超出900px视口。UI-006 先行实测文件 zoom-dialog-before-next.json 已保留。生产 CSS 的 viewport 单位改为 dialog 包含块百分比约束；回归 modal top=81、bottom=819、高738，横向 left=24/right=856，真实指针可点下一步和错误链接。未强行加入 <480/<440 布局改造：指定五宽、长 URL 和50%膨胀未复现横向溢出；字段内容仍可编辑滚动，不把输入原生内容滚动算成页面溢出。
4. 工作台来源的遗留“模型服务”文案统一为“AI 服务”；保留任务显式选择优先、默认不影响已有任务的作用域说明。

## 验证

原入口实际启动 Chrome、Vite loopback 并加载 AiConnectionsPage 及真实生产子组件；320/390/480/620/880 宽、长 URL、50%文案膨胀、200% CSS zoom 的指针/键盘、五深链及四方向/Home/End、唯一 acp 面板、嵌套确认、Esc/焦点返回/保存忙态、radio/pressed/current、跨步错误及非密钥保留均有行为断言。成功、启动异常及 deadline 三条路径均有 cleanup.json，browserClosed/serverClosed 均为 true。故意失败路径 exit=1 是清理测试，不冒充 required_check 成功。

实现后重新按评审立场复查：保留全部强断言；未用静态正则替代浏览器，不引入接口或宿主生命周期修改。报告生成器先落业务结果再执行两条原 Python 检查；正式宿主 VERIFY 未调用、不作为当前就绪前置条件。

## 假设、历史与未验证

历史来源 local-run:workflow_run_mission_mission_1794473598404_ffe1e069_repair-service-ui-delivery-2 只作为关联标识；本任务可访问 reports/service-ui 的 repair2 日志，但未获准访问宿主 local-run 内部存储。授权可读目录未发现 codex-final-hang-1794490376225663700.json，文件内容不可访问。terminal turn 后 shutdown deadline 未退出仅为用户给定历史，不证明业务完成。

宿主事件 mission_1794473598404_ffe1e069:506 的五项历史退出码依次 0/1/0/1/1；五项 test_summary 为空，保留为 null，不填零、不推算 TAP 数。它们与本轮命令独立。

截图和浏览器几何/AX证据已生成；本会话图片查看及 Computer Use 工具报 sandboxCwd must use the file URI scheme，未完成肉眼图片复核。未运行 NVDA/讲述人实际语音；不宣称完整无障碍标准认证。200% 是 CSS zoom（含真实指针与键盘），不是浏览器工具栏原生缩放。未验证真实后端/凭据、部署及正式宿主独立验收；不冒充这些结论。当前任务没有要求把这些额外核验当作阶段通过前置条件。
