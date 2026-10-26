import { useEffect, useState, type KeyboardEvent } from 'react';
import { PageHeader } from '../components/Common';
import { AI_CONNECTIONS_DESCRIPTION, AI_CONNECTIONS_TAB_LABELS, AI_CONNECTIONS_TAB_ORDER, type AiConnectionsTab } from './aiConnectionsTabs';
import { AcpProfilesPanel } from './AcpProfilesPanel';
import { AiToolsPanel } from './AiToolsPanel';
import { AiProvidersPanel } from './AiProvidersPanel';
import { AiServiceRoutesPanel } from './AiServiceRoutesPanel';
import type {
  AIServiceProtocol,
  AIServiceBalanceSnapshot,
  AIServiceListResult,
  AIServiceTemplateListResult,
  AcpRuntimeProfileInput,
  AcpRuntimeProfileSnapshot,
  DashboardIdentityStatus,
  McpConnectionTestResult,
  McpTargetDescriptor,
  InferenceGatewayStatus,
} from '../services/agentApi';

type AiConnectionsPageProps = {
  initialTab?: AiConnectionsTab;
  identity: DashboardIdentityStatus | null;
  dashboardEnabled: boolean;
  targets: McpTargetDescriptor[];
  testResult: McpConnectionTestResult | null;
  busyAction: string | null;
  aiServices: AIServiceListResult | null;
  aiServiceTemplates: AIServiceTemplateListResult | null;
  gatewayStatus: InferenceGatewayStatus | null;
  onSetBindingMode: (target: string, mode: 'gateway' | 'direct', service?: string) => Promise<void>;
  gatewayBusy: boolean;
  onRestartGateway: () => void;
  onStopGateway: () => void;
  onSetGatewayPort: (port: number) => Promise<void>;
  acpProfiles: AcpRuntimeProfileSnapshot | null;
  onOpenAccount: () => void;
  onRefresh: () => void;
  onApplyTarget: (targetId: string, resetInvalid?: boolean) => void;
  onApplyAll: () => void;
  onRemoveAll: () => void;
  onRemoveTarget: (targetId: string) => void;
  onOpenDirectory: (path: string) => void;
  onTest: () => void;
  onSaveAIService: (input: {
    id: string;
    display_name: string;
    base_url: string;
    protocol: AIServiceProtocol;
    model: string;
    models: string[];
    image_model: string;
    image_adapter: string;
    image_edit_model: string;
    image_edit_adapter: string;
    video_model: string;
    video_adapter: string;
    speech_model: string;
    speech_adapter: string;
    transcribe_model: string;
    transcribe_adapter: string;
    balance_profile_id: string;
    balance_access_token: string;
    balance_user_id: string;
    api_key: string;
  }) => Promise<void>;
  onSetActiveAIService: (id: string) => Promise<void>;
  onSetImageAIService: (id: string, capability?: string) => Promise<void>;
  onRemoveAIService: (id: string) => Promise<void>;
  onImportAIClient: (target: string, service?: string, replace?: boolean) => Promise<void>;
  onRemoveAIClient: (target: string) => Promise<void>;
  onFetchModels: (input: { base_url: string; api_key: string; protocol: AIServiceProtocol }) => Promise<string[]>;
  onFetchSavedModels: (id: string, base_url: string) => Promise<string[]>;
  onFetchBalance: (id: string) => Promise<AIServiceBalanceSnapshot>;
  onSaveAcpProfile: (input: AcpRuntimeProfileInput) => Promise<void>;
  onSetAcpProfileEnabled: (providerId: string, enabled: boolean) => Promise<void>;
  onRemoveAcpProfile: (providerId: string) => Promise<void>;
};

/**
 * AI 连接按「用户要做什么」分三个 tab，而不是按 HiMind 的内部子系统分。
 * 同一个工具只在「工具接入」里出现一次：展开即见它的接入（MCP）、模型、执行三条接线。
 * 模型「来源」单独成页只管定义与对话默认；运行环境保留原样。
 * tab id 保持稳定，外部脚本与深链依赖它（initialTab 的 mcp / services / acp 键不变）；
 * 第一个 tab 对外叫「工具接入」，避开左栏「本机工具与技能」的同词撞车。
 * 标签与页头说明放在 aiConnectionsTabs：页签、说明和对话页的指路文案共用一份，
 * 同一个入口才不会在报错里换个名字。
 */
const TAB_ORDER = AI_CONNECTIONS_TAB_ORDER;
const TAB_LABELS = AI_CONNECTIONS_TAB_LABELS;

export function AiConnectionsPage({
  initialTab = 'mcp',
  identity,
  dashboardEnabled,
  targets,
  testResult,
  busyAction,
  aiServices,
  aiServiceTemplates,
  gatewayStatus,
  onSetBindingMode,
  gatewayBusy,
  onRestartGateway,
  onStopGateway,
  onSetGatewayPort,
  acpProfiles,
  onOpenAccount,
  onRefresh,
  onApplyTarget,
  onApplyAll,
  onRemoveAll,
  onRemoveTarget,
  onOpenDirectory,
  onTest,
  onSaveAIService,
  onSetActiveAIService,
  onSetImageAIService,
  onRemoveAIService,
  onImportAIClient,
  onRemoveAIClient,
  onFetchModels,
  onFetchSavedModels,
  onFetchBalance,
  onSaveAcpProfile,
  onSetAcpProfileEnabled,
  onRemoveAcpProfile,
}: AiConnectionsPageProps) {
  const [activeTab, setActiveTab] = useState<AiConnectionsTab>(initialTab);

  useEffect(() => {
    setActiveTab(initialTab);
  }, [initialTab]);

  // WAI-ARIA tabs 键盘约定：左右/上下键循环、Home/End 跳首尾，焦点跟着切换走。
  const selectTab = (index: number) => {
    const next = TAB_ORDER[(index + TAB_ORDER.length) % TAB_ORDER.length];
    setActiveTab(next);
    requestAnimationFrame(() => document.getElementById(`ai-tab-${next}`)?.focus());
  };
  const onTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') { event.preventDefault(); selectTab(index + 1); }
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') { event.preventDefault(); selectTab(index - 1); }
    else if (event.key === 'Home') { event.preventDefault(); selectTab(0); }
    else if (event.key === 'End') { event.preventDefault(); selectTab(TAB_ORDER.length - 1); }
  };

  return (
    <div className="ai-page">
      <PageHeader title="AI 连接" description={AI_CONNECTIONS_DESCRIPTION} />

      <div className="ai-tabs" role="tablist" aria-label="AI 连接分类">
        {TAB_ORDER.map((tab, index) => (
          <button
            key={tab}
            type="button"
            id={`ai-tab-${tab}`}
            role="tab"
            aria-selected={activeTab === tab}
            aria-controls={`ai-panel-${tab}`}
            tabIndex={activeTab === tab ? 0 : -1}
            className={`ai-tab${activeTab === tab ? ' active' : ''}`}
            onClick={() => setActiveTab(tab)}
            onKeyDown={(event) => onTabKeyDown(event, index)}
          >{TAB_LABELS[tab]}</button>
        ))}
      </div>

      {activeTab === 'acp' ? (
        <div id="ai-panel-acp" role="tabpanel" aria-labelledby="ai-tab-acp">
          <AcpProfilesPanel
            snapshot={acpProfiles}
            busyAction={busyAction}
            onSave={onSaveAcpProfile}
            onSetEnabled={onSetAcpProfileEnabled}
            onRemove={onRemoveAcpProfile}
          />
        </div>
      ) : activeTab === 'services' ? (
        <div id="ai-panel-services" role="tabpanel" aria-labelledby="ai-tab-services">
          <AiProvidersPanel
            aiServices={aiServices}
            templates={aiServiceTemplates}
            onRefresh={onRefresh}
            onSaveAIService={onSaveAIService}
            onRemoveAIService={onRemoveAIService}
            onOpenAccount={onOpenAccount}
            onFetchModels={onFetchModels}
            onFetchSavedModels={onFetchSavedModels}
            onFetchBalance={onFetchBalance}
          />
        </div>
      ) : activeTab === 'routes' ? (
        <div id="ai-panel-routes" role="tabpanel" aria-labelledby="ai-tab-routes">
          <AiServiceRoutesPanel
            aiServices={aiServices}
            onOpenServices={() => setActiveTab('services')}
            onSetActiveAIService={onSetActiveAIService}
            onSetMediaAIService={onSetImageAIService}
          />
        </div>
      ) : (
        <div id="ai-panel-mcp" role="tabpanel" aria-labelledby="ai-tab-mcp">
          <AiToolsPanel
            targets={targets}
            aiServices={aiServices}
            acpProfiles={acpProfiles}
            gatewayStatus={gatewayStatus}
            busyAction={busyAction}
            testResult={testResult}
            dashboardEnabled={dashboardEnabled}
            identity={identity}
            onRefresh={onRefresh}
            onApplyTarget={onApplyTarget}
            onApplyAll={onApplyAll}
            onRemoveAll={onRemoveAll}
            onRemoveTarget={onRemoveTarget}
            onOpenDirectory={onOpenDirectory}
            onTest={onTest}
            onOpenAccount={onOpenAccount}
            onImportAIClient={onImportAIClient}
            onRemoveAIClient={onRemoveAIClient}
            onOpenModelSources={() => setActiveTab('services')}
            onSetBindingMode={onSetBindingMode}
            gatewayBusy={gatewayBusy}
            onRestartGateway={onRestartGateway}
            onStopGateway={onStopGateway}
            onSetGatewayPort={onSetGatewayPort}
            onSaveAcpProfile={onSaveAcpProfile}
            onSetAcpProfileEnabled={onSetAcpProfileEnabled}
            onRemoveAcpProfile={onRemoveAcpProfile}
          />
        </div>
      )}
    </div>
  );
}
