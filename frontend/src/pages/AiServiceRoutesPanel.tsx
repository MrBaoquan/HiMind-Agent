import { useEffect, useState } from 'react';
import { Activity, Layers, Plus, Route, Settings2, ShieldCheck, X } from 'lucide-react';
import { ActionMenu, ActionMenuItem } from '../components/ActionMenu';
import { BusyIndicator } from '../components/BusyIndicator';
import {
  agentApi,
  type AIServiceListResult,
  type AICapabilityTestResult,
  type CapabilityRouteEntry,
  type CapabilityRoutePlan,
  type CustomAIService,
  type MediaProviderCatalogResult,
} from '../services/agentApi';
import {
  AI_SERVICE_CAPABILITY_LABELS,
  MEDIA_CAPABILITIES,
  capabilityModel,
  capabilityModels,
  serviceSupportsCapability,
  type AIServiceCapability,
} from './aiServiceCapabilities';

type AiServiceRoutesPanelProps = {
  aiServices: AIServiceListResult | null;
  onOpenServices: () => void;
  /** 覆盖写入某能力的完整路由表：下标 0 是主选，其余按序是备用。 */
  onSetCapabilityRoutes: (capability: AIServiceCapability, entries: CapabilityRouteEntry[]) => Promise<void>;
};

const CAPABILITIES: AIServiceCapability[] = ['chat', ...MEDIA_CAPABILITIES];

const CAPABILITY_HINTS: Record<AIServiceCapability, string> = {
  chat: 'HiMind AI 对话与工作流文本推理',
  'image.generate': '生图；与对话模型分开',
  'image.edit': '改图/图生图',
  'video.generate': '文生视频/图生视频，异步任务',
  'audio.speech': '单次语音合成',
  'audio.transcribe': '单次语音转写',
  // 与「AI 服务」页视觉理解卡片同一句：那里本来就是这个说法，两个入口用同一套术语。
  'vision.describe': '看图打标、OCR 与画面理解',
};

type TestView = {
  busy: boolean;
  result?: AICapabilityTestResult;
  needsConfirmation?: boolean;
};

function testPayload(capability: AIServiceCapability): Record<string, unknown> {
  switch (capability) {
    case 'chat': return {};
    case 'image.generate': return { prompt: 'HiMind 能力自检：一张简洁的蓝色几何图形', n: 1 };
    case 'image.edit': return { prompt: '保持主体不变，改成蓝色背景' };
    case 'video.generate': return { prompt: '一张纸飞机缓慢飞过桌面', duration_seconds: 2, resolution: '720P' };
    case 'audio.speech': return { text: 'HiMind 能力自检', format: 'mp3' };
    case 'audio.transcribe': return {};
    case 'vision.describe': return {};
  }
}

function planStatus(plan: CapabilityRoutePlan | undefined): { kind: 'success' | 'warn' | 'neutral'; text: string } {
  if (!plan) return { kind: 'neutral', text: '等待解析' };
  if (plan.state === 'resolved') return { kind: 'success', text: plan.fallbacks.length ? `可用 · ${plan.fallbacks.length + 1} 级` : '可用' };
  if (plan.state === 'ambiguous') return { kind: 'warn', text: '多个候选，需指定' };
  if (plan.reason === 'model_unavailable') return { kind: 'warn', text: '所选模型已不可用' };
  return { kind: 'neutral', text: '无可用服务' };
}

/** 备用项下拉的取值编码：`服务 ID::模型`，模型为空表示跟随服务默认。 */
const ENTRY_SEPARATOR = '::';

function encodeEntry(entry: CapabilityRouteEntry): string {
  return `${entry.service_id}${ENTRY_SEPARATOR}${entry.model}`;
}

function decodeEntry(value: string): CapabilityRouteEntry {
  const index = value.indexOf(ENTRY_SEPARATOR);
  if (index < 0) return { service_id: value, model: '' };
  return { service_id: value.slice(0, index), model: value.slice(index + ENTRY_SEPARATOR.length) };
}

/** 路由项的展示拆分：服务名与真正会用的模型。备用 chip 两行各放一段，
 *  菜单与 title 里拼回一整句。 */
function entryParts(service: CustomAIService | undefined, capability: AIServiceCapability, model: string, fallbackId: string): { service: string; model: string } {
  if (!service) return { service: fallbackId, model: model || '跟随服务默认' };
  return { service: service.display_name, model: model || capabilityModel(service, capability) };
}

/** 路由项的一行文案：服务名 + 真正会用的模型。 */
function entryLabel(service: CustomAIService | undefined, capability: AIServiceCapability, model: string, fallbackId: string): string {
  const parts = entryParts(service, capability, model, fallbackId);
  return parts.model ? `${parts.service} · ${parts.model}` : parts.service;
}

export function AiServiceRoutesPanel({
  aiServices,
  onOpenServices,
  onSetCapabilityRoutes,
}: AiServiceRoutesPanelProps) {
  const [savingKey, setSavingKey] = useState('');
  const [catalog, setCatalog] = useState<MediaProviderCatalogResult | null>(null);
  const [catalogError, setCatalogError] = useState('');
  const [tests, setTests] = useState<Record<string, TestView>>({});
  const customServices = aiServices?.custom?.services ?? [];
  const routeTable = aiServices?.custom?.capability_routes ?? {};
  const managed = aiServices?.managed ?? { available: false };

  const planFor = (capability: AIServiceCapability) => catalog?.routes?.find(route => route.capability === capability);
  const candidatesFor = (capability: AIServiceCapability) => customServices.filter(service => serviceSupportsCapability(service, capability));
  /** 该能力的有序路由表：下标 0 是主选，其余是备用。 */
  const entriesFor = (capability: AIServiceCapability): CapabilityRouteEntry[] => routeTable[capability] ?? [];
  const serviceById = (id: string) => customServices.find(service => service.id === id);

  async function refreshCatalog() {
    try {
      setCatalog(await agentApi.getMediaProviderCatalog());
      setCatalogError('');
    } catch (error) {
      setCatalogError(error instanceof Error ? error.message : String(error));
    }
  }

  useEffect(() => {
    void refreshCatalog();
  }, [aiServices]);

  async function writeRoutes(capability: AIServiceCapability, entries: CapabilityRouteEntry[]) {
    setSavingKey(capability);
    try {
      await onSetCapabilityRoutes(capability, entries);
      await refreshCatalog();
    } catch (error) {
      setCatalogError(error instanceof Error ? error.message : String(error));
    } finally {
      setSavingKey('');
    }
  }

  /** 换主选服务：该能力的模型重置为空（跟随新服务的默认绑定），备用顺序原样保留。 */
  function changePrimaryService(capability: AIServiceCapability, serviceId: string) {
    const entries = entriesFor(capability);
    if (!serviceId) {
      // 清空主选等于清空整条路由：没有主选的备用顺序没有意义。
      void writeRoutes(capability, []);
      return;
    }
    const rest = entries
      .slice(1)
      .filter(entry => !(entry.service_id === serviceId && entry.model === ''));
    void writeRoutes(capability, [{ service_id: serviceId, model: '' }, ...rest]);
  }

  function changePrimaryModel(capability: AIServiceCapability, model: string) {
    const entries = entriesFor(capability);
    const primary = entries[0];
    if (!primary) return;
    const rest = entries
      .slice(1)
      .filter(entry => !(entry.service_id === primary.service_id && entry.model === model));
    void writeRoutes(capability, [{ service_id: primary.service_id, model }, ...rest]);
  }

  function removeFallback(capability: AIServiceCapability, index: number) {
    void writeRoutes(capability, entriesFor(capability).filter((_, current) => current !== index));
  }

  function appendFallback(capability: AIServiceCapability, value: string) {
    if (!value) return;
    void writeRoutes(capability, [...entriesFor(capability), decodeEntry(value)]);
  }

  async function runTest(capability: AIServiceCapability, serviceId: string, model: string, confirmCost: boolean) {
    setTests(current => ({ ...current, [capability]: { ...current[capability], busy: true } }));
    try {
      const result = await agentApi.testAICapability({
        capability,
        service: serviceId ? `custom:${serviceId}` : '',
        model,
        confirm_cost: confirmCost,
        payload: testPayload(capability),
      });
      setTests(current => ({
        ...current,
        [capability]: {
          busy: false,
          result,
          needsConfirmation: result.state === 'confirmation_required',
        },
      }));
    } catch (error) {
      setTests(current => ({
        ...current,
        [capability]: {
          busy: false,
          result: {
            state: 'failed',
            capability,
            message: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    }
  }

  return (
    <section className="ai-routes-view" aria-labelledby="ai-routes-title">
      <header className="ai-routes-header">
        <div>
          {/* 页签已经写了「能力路由」，页头不再重复一遍 eyebrow；
              说明句收进标题的 title：它是解释，不是每次都要读的正文。 */}
          <h3 id="ai-routes-title" title="每个能力可以分别指定服务与模型；仅在同能力内降级">默认服务与备用顺序</h3>
        </div>
        <button className="btn" onClick={onOpenServices}><Settings2 size={14} />管理 AI 服务</button>
      </header>
      {catalogError ? <div className="inline-feedback visible" role="alert">路由诊断读取失败：{catalogError}</div> : null}

      <div className="ai-route-table" role="table" aria-label="AI 能力路由">
        <div className="ai-route-row ai-route-row-head" role="row">
          <span role="columnheader">能力</span>
          <span role="columnheader">默认服务与模型</span>
          <span role="columnheader">备用顺序</span>
          <span role="columnheader">状态与操作</span>
        </div>

        {CAPABILITIES.map(capability => {
          const plan = planFor(capability);
          const candidates = candidatesFor(capability);
          const entries = entriesFor(capability);
          const primaryEntry = entries[0];
          const primaryService = primaryEntry ? serviceById(primaryEntry.service_id) : undefined;
          const primaryModels = primaryService ? capabilityModels(primaryService, capability) : [];
          const fallbacks = entries.slice(1).map((entry, offset) => ({
            entry,
            index: offset + 1,
            service: serviceById(entry.service_id),
          }));
          // 备用下拉里不重复已经用过的 (服务, 模型) 组合。
          const usedPairs = new Set(entries.map(encodeEntry));
          const fallbackOptions = candidates
            .map(service => {
              const models = capabilityModels(service, capability);
              // 空模型是单独一项：它保存空串，服务默认模型改了会跟着变。
              // 下拉里只写「跟随服务默认」，具体模型在选中后由 chip 显示。
              const options = ['', ...models]
                .map(model => ({ model, encoded: encodeEntry({ service_id: service.id, model }) }))
                .filter(option => !usedPairs.has(option.encoded));
              return { service, options };
            })
            .filter(group => group.options.length > 0);
          const status = planStatus(plan);
          const test = tests[capability];
          const testText = test?.result?.message || (test?.result?.state === 'healthy' ? '连接正常' : '');
          const resolvedModel = plan?.selected?.model ?? (primaryService ? capabilityModel(primaryService, capability) : '');
          const modelUnavailable = Boolean(primaryEntry) && plan?.state !== 'resolved' && plan?.reason === 'model_unavailable';
          const busy = savingKey === capability;
          // 服务只有一个模型时，第二个下拉里只有「跟随服务默认」一个选项：它没有选择权，
          // 只占高度。已有钉住的模型（哪怕是目录外的旧值）仍要显示，否则用户没法清掉它。
          const showModelSelect = primaryModels.length > 1 || Boolean(primaryEntry?.model);
          const canEditFallbacks = Boolean(primaryEntry) && (fallbacks.length > 0 || fallbackOptions.length > 0);
          // 这一行剩下的不可见细节都收进服务下拉的 title：生效模型（模型下拉没渲染时才需要，
          // 否则那个下拉的 title 已经给了）、失效告警、执行方式。它们要么逐行重复，要么
          // 是模型名那种长到必然被裁的文本，常驻显示只能换来一行被截断的字。
          const primaryTitle = [
            primaryService?.display_name ?? '自动选择',
            !showModelSelect && resolvedModel ? resolvedModel : '',
            modelUnavailable ? `${primaryEntry?.model}（已不可用）` : '',
            plan?.selected?.execution_mode ? `执行：${plan.selected.execution_mode}` : '',
          ].filter(Boolean).join(' · ');
          return (
            <div className="ai-route-row" role="row" key={capability}>
              <div className="ai-route-capability" role="cell">
                <Route size={16} />
                <strong>{AI_SERVICE_CAPABILITY_LABELS[capability]}</strong>
                <small>{CAPABILITY_HINTS[capability]}</small>
              </div>
              <div className="ai-route-primary" role="cell">
                <select
                  className="ai-service-route-select"
                  aria-label={`${AI_SERVICE_CAPABILITY_LABELS[capability]}默认服务`}
                  title={primaryTitle}
                  value={primaryEntry?.service_id ?? ''}
                  disabled={busy}
                  onChange={event => changePrimaryService(capability, event.target.value)}
                >
                  <option value="">自动选择</option>
                  {candidates.map(service => <option key={service.id} value={service.id}>{service.display_name}</option>)}
                  {capability === 'chat' && managed.available ? <option value="" disabled>工作台模型服务（自动优先）</option> : null}
                </select>
                {showModelSelect ? <select
                  className="ai-service-route-select"
                  aria-label={`${AI_SERVICE_CAPABILITY_LABELS[capability]}默认模型`}
                  // 模型名常比列宽长，下拉闭合时只能显示一段；完整值放 title。
                  title={primaryEntry?.model || (primaryService ? capabilityModel(primaryService, capability) : '') || '跟随服务默认'}
                  value={primaryEntry?.model ?? ''}
                  disabled={busy || !primaryService}
                  onChange={event => changePrimaryModel(capability, event.target.value)}
                >
                  {/* 选项只写「跟随服务默认」：每次都在每行里重复一遍最长的那串模型名，
                      只会把下拉撑出裁切。真正生效的模型走这里的 title。 */}
                  <option value="">跟随服务默认</option>
                  {primaryModels.map(model => <option key={model} value={model}>{model}</option>)}
                </select> : null}
              </div>
              <div className="ai-route-fallbacks" role="cell">
                {/* 备用顺序是状态，不是控件：只读展示，增删都收进状态列的操作菜单。
                    没有备用时给一个等宽占位符，而不是「无其它候选」的下拉——那会让整列
                    看起来像还要配置。 */}
                {fallbacks.length > 0 ? fallbacks.map(item => {
                  const parts = entryParts(item.service, capability, item.entry.model, item.entry.service_id);
                  return (
                    <span
                      className="ai-route-fallback-chip"
                      // 备用列宽固定，长服务名 + 长模型名会被省略号截断；title 保证完整值可读。
                      title={`${item.index}. ${parts.service} · ${parts.model}`}
                      key={`${item.entry.service_id}${ENTRY_SEPARATOR}${item.entry.model}${ENTRY_SEPARATOR}${item.index}`}
                    >
                      <b>{item.index}. {parts.service}</b>
                      <em>{parts.model}</em>
                    </span>
                  );
                }) : <span className="ai-route-axis" aria-label="没有备用服务">—</span>}
              </div>
              <div className="ai-route-status" role="cell">
                {/* 状态用圆点 + 文字，和 AI 服务行、扩展列表同一套表达；
                    药丸留给「类别」和筛选，不再留给每一次行内状态。 */}
                <span className={`ai-route-state is-${status.kind}`}><span className="status-dot" aria-hidden="true" />{status.text}</span>
                {/* 没有可用服务时，备用顺序和自检都无从谈起：唯一有意义的动作是去配置。
                    留两个永远禁用的图标按钮只会让人以为它们坏了，也把状态列撑宽。 */}
                {plan?.state === 'missing' ? <button className="btn ai-service-tool-btn" onClick={onOpenServices}>去配置</button> : <>
                  {/* 备用顺序的增删是低频、破坏性动作：常驻一个下拉会在每行都占一格，
                      而绝大多数行没有备用。收进图标菜单，需要时才展开。 */}
                  <ActionMenu
                    variant="icon"
                    icon={<Layers size={13} />}
                    title={canEditFallbacks ? `${AI_SERVICE_CAPABILITY_LABELS[capability]}备用顺序` : '没有可用的备用候选'}
                    disabled={busy || !canEditFallbacks}
                    panelClassName="app-menu-dropdown ai-route-menu"
                    panelWidth={280}
                  >
                    {close => <>
                      {fallbacks.length > 0 ? <div className="app-menu-section-label">当前备用</div> : null}
                      {fallbacks.map(item => {
                        const label = entryLabel(item.service, capability, item.entry.model, item.entry.service_id);
                        return (
                          <ActionMenuItem
                            key={`remove-${item.index}-${item.entry.service_id}`}
                            danger
                            icon={<X size={15} />}
                            // 只写服务名：菜单宽度容不下「服务 · 模型」全名，而右侧的备用 chip
                            // 已经按序号列出了完整的服务与模型，这里的序号足以对应上。
                            label={`移除 ${item.index}. ${item.service?.display_name ?? item.entry.service_id}`}
                            title={`移除 ${item.index}. ${label}`}
                            ariaLabel={`移除备用 ${item.index}. ${label}`}
                            disabled={busy}
                            onClick={() => { close(); removeFallback(capability, item.index); }}
                          />
                        );
                      })}
                      {/* 候选按服务分组：一个服务可能挂几十个模型，平铺成一条长菜单
                          没法扫读。组名本身就是用户找模型的第一个坐标（ADR 0023）。 */}
                      {fallbackOptions.map(group => <div key={group.service.id}>
                        <div className="app-menu-section-label">{group.service.display_name}</div>
                        {group.options.map(option => (
                          <ActionMenuItem
                            key={`add-${option.encoded}`}
                            icon={<Plus size={15} />}
                            label={option.model || '跟随服务默认'}
                            title={`添加 ${group.service.display_name} · ${option.model || '跟随服务默认'}`}
                            // 分组标题是视觉信息，读屏遍历菜单项时听不到；补进名称里。
                            ariaLabel={`添加 ${group.service.display_name} · ${option.model || '跟随服务默认'} 作为备用`}
                            disabled={busy}
                            onClick={() => { close(); appendFallback(capability, option.encoded); }}
                          />
                        ))}
                      </div>)}
                    </>}
                  </ActionMenu>
                  <button
                    type="button"
                    className="btn btn-icon ai-route-icon-btn"
                    disabled={!primaryService || test?.busy}
                    title="发一次最小请求验证这条路由"
                    aria-label={`自检${AI_SERVICE_CAPABILITY_LABELS[capability]}`}
                    onClick={() => void runTest(capability, primaryEntry?.service_id ?? '', primaryEntry?.model ?? '', false)}
                  >
                    {test?.busy ? <BusyIndicator size={12} /> : <Activity size={13} />}
                  </button>
                </>}
                {test?.needsConfirmation ? <button
                  type="button"
                  className="btn btn-primary ai-service-tool-btn"
                  disabled={test.busy}
                  onClick={() => void runTest(capability, primaryEntry?.service_id ?? '', primaryEntry?.model ?? '', true)}
                ><ShieldCheck size={12} />确认费用并自检</button> : null}
                {testText ? <small className={test?.result?.state === 'healthy' || test?.result?.state === 'accepted' ? 'ai-route-test ok' : 'ai-route-test'}>{testText}</small> : null}
                {busy ? <BusyIndicator size={12} /> : null}
              </div>
            </div>
          );
        })}
      </div>
    </section>
  );
}
