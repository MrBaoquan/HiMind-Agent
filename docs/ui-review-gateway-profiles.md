# 模型网关页面 UI/UX 规范审查

日期：2026-10-27  
范围：Windows 桌面设置窗口中的「AI 连接 → 模型网关」，含 996 CSS px、620 CSS px、344 CSS px 窄窗复验。  
依据：仓库设计令牌与软件界面规范；WCAG 2.2 AA 的 1.3.1、1.4.1、1.4.3、1.4.10、1.4.11、2.4.7、2.5.8、4.1.3。

## 问题清单

| # | 现象 | 根因 | 整改 | 验收 |
| --- | --- | --- | --- | --- |
| 1 | 996px 设置窗口中模型路由仍为 5 列，服务名和删除按钮被挤压 | 响应式断点按整窗 960px 判断，没有计入设置左栏占用 | 路由表在 1180px 以下切为双列，并显示每列字段标签 | screenshot-final-route-996.png 中服务名和删除按钮完整 |
| 2 | 960px 以下表头隐藏后，字段没有自身标签 | 窄窗只重排网格，没有同步补语义标签 | 每个路由字段使用带可见标签的 label | DOM 检查无丢失标签，check:gateway-profile-ui 通过 |
| 3 | 模型较多时卡片无限增高 | 所有别名 chip 全部展开 | 默认展示 3 个，剩余显示 +N，可展开/收起 | visibleProfileModels 单测与真机通过 |
| 4 | 保存、启停、删除没有可靠的单项进行中状态 | 页面依赖全局 busy，但父级保存入口未设置 aiOperation | 增加 pendingAction、aria-busy、父级 aiOperation 与错误回显 | 保存/启停/删除按钮显示局部 BusyIndicator 并禁止重复点击 |
| 5 | 停用卡片整体降透明度，文字对比变弱 | opacity: .68 作用到整张卡片 | 改用低强调表面色，状态由文字 Pill 表达 | design token 对比检查通过 |
| 6 | 360px 下 AI 连接页签和工具筛选器产生横向滚动 | 页签和筛选器固定单行，宽度超出内容区 | 760px 以下页签换行；520px 容器以下筛选器独占整行 | 344px 内容视口 rootScrollWidth=344，真机无横向滚动条 |
| 7 | 一个入口只能选一个客户端协议 | Profile 数据模型只有 entry_protocol | 新增 entry_protocols，旧字段仅兼容 | 多协议保存、旧配置回填和客户端自动选协议测试通过 |

## 交互与文案

- 协议改为三组带文字的 checkboxes：OpenAI Chat、OpenAI Responses、Anthropic Messages。
- 显示每个协议对应的客户端，并实时展示「兼容上游」。
- 多协议组合只允许共享上游；不兼容服务保留文字原因，不做静默隐藏或替换。
- 至少保留一个协议；最后一个选项不可取消。
- 所有操作错误先指出具体问题，再说明修复方式。
- 每个协议分别显示可用的上游范围；每条模型路由显示会注册到哪些客户端。
- 客户端模型注册和运行时模型目录都只包含该客户端协议兼容的模型。

## 验证

- npm run check:ui：通过。
- check:gateway-profile-ui：通过。
- 设计令牌：正文 17.44:1，弱化文字 6.24:1，控件边框 3.66:1。
- 真机截图：screenshot-final-clean.png、screenshot-final-route-996.png、screenshot-final-360-route.png。
- 360px 真机 DOM：innerWidth=344，rootClientWidth=344，rootScrollWidth=344。

## 遗留

- 本轮真机截图使用浅色主题；深色主题仍需单独截图复核。
- 已检查可见焦点样式和原生 checkbox/select 语义，但未做完整键盘全流程录屏。
- 设置窗口的全局导航在 360px 会变成多行，信息密度高但不再横向溢出。
