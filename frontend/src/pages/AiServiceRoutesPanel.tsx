import { useEffect, useState } from 'react';
import { Activity, CircleAlert, Plus, Route, Settings2, ShieldCheck, X } from 'lucide-react';
import { Pill } from '../components/Common';
import { BusyIndicator } from '../components/BusyIndicator';
import { agentApi, type AIServiceListResult, type AICapabilityTestResult, type CapabilityRoutePlan, type MediaProviderCatalogResult } from '../services/agentApi';
import {
  AI_SERVICE_CAPABILITY_LABELS,
  MEDIA_CAPABILITIES,
  capabilityModel,
  serviceSupportsCapability,
  type AIServiceCapability,
} from './aiServiceCapabilities';

type AiServiceRoutesPanelProps = {
  aiServices: AIServiceListResult | null;
  onOpenServices: () => void;
  onSetActiveAIService: (id: string) => Promise<void>;
  onSetMediaAIService: (id: string, capability: string) => Promise<void>;
};

const CAPABILITIES: AIServiceCapability[] = ['chat', ...MEDIA_CAPABILITIES];

const CAPABILITY_HINTS: Record<AIServiceCapability, string> = {
  chat: 'HiMind AI 对话与工作流文本推理',
  'image.generate': '生图；与对话模型分开',
  'image.edit': '改图/图生图',
  'video.generate': '文生视频/图生视频，异步任务',
  'audio.speech': '单次语音合成',
  'audio.transcribe': '单次语音转写',
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
  }
}

function planStatus(plan: CapabilityRoutePlan | undefined): { kind: 'success' | 'warn' | 'neutral'; text: string } {
  if (!plan) return { kind: 'neutral', text: '等待解析' };
  if (plan.state === 'resolved') return { kind: 'success', text: plan.fallbacks.length ? `可用 · ${plan.fallbacks.length + 1} 级` : '可用' };
  if (plan.state === 'ambiguous') return { kind: 'warn', text: '多个候选，需指定' };
  return { kind: 'neutral', text: '无可用服务' };
}

export function AiServiceRoutesPanel({
  aiServices,
  onOpenServices,
  onSetActiveAIService,
  onSetMediaAIService,
}: AiServiceRoutesPanelProps) {
  const [settingKey, setSettingKey] = useState('');
  const [fallbackBusy, setFallbackBusy] = useState('');
  const [catalog, setCatalog] = useState<MediaProviderCatalogResult | null>(null);
  const [catalogError, setCatalogError] = useState('');
  const [tests, setTests] = useState<Record<string, TestView>>({});
  const customServices = aiServices?.custom?.services ?? [];
  const activeServiceId = aiServices?.custom?.active_service_id ?? '';
  const mediaDefaults = aiServices?.custom?.media_services ?? {};
  const imageServiceId = aiServices?.custom?.image_service_id ?? '';
  const managed = aiServices?.managed ?? { available: false };

  const planFor = (capability: AIServiceCapability) => catalog?.routes?.find(route => route.capability === capability);
  const defaultFor = (capability: AIServiceCapability) => capability === 'chat'
    ? activeServiceId
    : mediaDefaults[capability] || (capability === 'image.generate' ? imageServiceId : '');
  const candidatesFor = (capability: AIServiceCapability) => customServices.filter(service => serviceSupportsCapability(service, capability));

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

  async function changeRoute(capability: AIServiceCapability, value: string) {
    setSettingKey(capability);
    try {
      if (capability === 'chat') await onSetActiveAIService(value);
      else await onSetMediaAIService(value, capability);
      await refreshCatalog();
    } finally {
      setSettingKey('');
    }
  }

  async function changeFallbacks(capability: AIServiceCapability, ids: string[]) {
    setFallbackBusy(capability);
    try {
      setCatalog(await agentApi.setAICapabilityFallbacks(capability, ids));
      setCatalogError('');
    } catch (error) {
      setCatalogError(error instanceof Error ? error.message : String(error));
    } finally {
      setFallbackBusy('');
    }
  }

  async function runTest(capability: AIServiceCapability, confirmCost: boolean) {
    setTests(current => ({ ...current, [capability]: { ...current[capability], busy: true } }));
    try {
      const result = await agentApi.testAICapability({
        capability,
        service: defaultFor(capability) ? `custom:${defaultFor(capability)}` : '',
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
          <span className="ai-overview-eyebrow">能力路由</span>
          <h3 id="ai-routes-title">默认服务 + 同能力备用顺序</h3>
          <p>仅同能力内降级；付费自检需再次确认。</p>
        </div>
        <button className="btn" onClick={onOpenServices}><Settings2 size={14} />管理 AI 服务</button>
      </header>
      {catalogError ? <div className="inline-feedback visible" role="alert">路由诊断读取失败：{catalogError}</div> : null}

      <div className="ai-route-table" role="table" aria-label="AI 能力路由">
        <div className="ai-route-row ai-route-row-head" role="row">
          <span role="columnheader">能力</span>
          <span role="columnheader">默认服务</span>
          <span role="columnheader">备用顺序</span>
          <span role="columnheader">解析结果</span>
          <span role="columnheader">状态与自检</span>
        </div>

        {CAPABILITIES.map(capability => {
          const plan = planFor(capability);
          const candidates = candidatesFor(capability);
          const selectedId = plan?.selected?.service_id ?? defaultFor(capability);
          const selectedService = candidates.find(service => service.id === selectedId);
          const fallbackIds = plan?.fallbacks.map(item => item.service_id) ?? catalog?.fallbacks?.[capability] ?? [];
          const fallbackServices = fallbackIds.map(id => ({ id, service: candidates.find(item => item.id === id) })).filter(item => item.service);
          const availableFallbacks = candidates.filter(service => service.id !== selectedId && !fallbackIds.includes(service.id));
          const status = planStatus(plan);
          const test = tests[capability];
          const testText = test?.result?.message || (test?.result?.state === 'healthy' ? '连接正常' : '');
          return (
            <div className="ai-route-row" role="row" key={capability}>
              <div className="ai-route-capability" role="cell">
                <Route size={16} />
                <strong>{AI_SERVICE_CAPABILITY_LABELS[capability]}</strong>
                <small>{CAPABILITY_HINTS[capability]}</small>
              </div>
              <div role="cell">
                <select className="ai-service-route-select" aria-label={`${AI_SERVICE_CAPABILITY_LABELS[capability]}默认服务`} value={defaultFor(capability)} disabled={settingKey === capability} onChange={event => void changeRoute(capability, event.target.value)}>
                  <option value="">自动选择</option>
                  {candidates.map(service => <option key={service.id} value={service.id}>{service.display_name}</option>)}
                  {capability === 'chat' && managed.available ? <option value="" disabled>工作台模型服务（自动优先）</option> : null}
                </select>
              </div>
              <div className="ai-route-fallbacks" role="cell">
                {fallbackServices.map((item, index) => item.service ? (
                  <span className="ai-route-fallback-chip" key={item.id}>
                    {index + 1}. {item.service.display_name}
                    <button type="button" aria-label={`移除备用服务 ${item.service.display_name}`} disabled={fallbackBusy === capability} onClick={() => void changeFallbacks(capability, fallbackIds.filter(id => id !== item.id))}><X size={11} /></button>
                  </span>
                ) : null)}
                <label className="ai-route-fallback-add">
                  <Plus size={12} />
                  <select aria-label={`添加${AI_SERVICE_CAPABILITY_LABELS[capability]}备用服务`} value="" disabled={!selectedId || fallbackBusy === capability || availableFallbacks.length === 0} onChange={event => { if (event.target.value) void changeFallbacks(capability, [...fallbackIds, event.target.value]); }}>
                    <option value="">{selectedId ? (availableFallbacks.length ? '添加备用' : '无其它候选') : '先选默认'}</option>
                    {availableFallbacks.map(service => <option key={service.id} value={service.id}>{service.display_name}</option>)}
                  </select>
                </label>
                {fallbackBusy === capability ? <BusyIndicator size={12} /> : null}
              </div>
              <div className="ai-route-resolution" role="cell">
                {selectedService ? <>
                  <strong>{selectedService.display_name}</strong>
                  <span>{capabilityModel(selectedService, capability)}</span>
                </> : <span className="ai-route-missing"><CircleAlert size={13} />{status.text}</span>}
                {plan?.selected?.execution_mode ? <small>执行：{plan.selected.execution_mode}</small> : null}
              </div>
              <div className="ai-route-status" role="cell">
                <Pill kind={status.kind}>{status.text}</Pill>
                {plan?.state === 'missing' ? <button className="btn ai-service-tool-btn" onClick={onOpenServices}>去配置</button> : null}
                <button type="button" className="btn ai-service-tool-btn" disabled={!selectedService || test?.busy} onClick={() => void runTest(capability, false)}>
                  {test?.busy ? <BusyIndicator size={12} /> : <Activity size={12} />}自检
                </button>
                {test?.needsConfirmation ? <button type="button" className="btn btn-primary ai-service-tool-btn" disabled={test.busy} onClick={() => void runTest(capability, true)}><ShieldCheck size={12} />确认费用并自检</button> : null}
                {testText ? <small className={test?.result?.state === 'healthy' || test?.result?.state === 'accepted' ? 'ai-route-test ok' : 'ai-route-test'}>{testText}</small> : null}
                {settingKey === capability ? <BusyIndicator size={12} /> : null}
              </div>
            </div>
          );
        })}
      </div>
    </section>
  );
}
