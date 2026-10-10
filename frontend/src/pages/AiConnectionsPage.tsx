import { useEffect, useState } from 'react';
import { PageHeader } from '../components/Common';
import { AcpProfilesPanel } from './AcpProfilesPanel';
import { AiToolsPanel } from './AiToolsPanel';
import { AiProvidersPanel } from './AiProvidersPanel';
import type {
  AIServiceProtocol,
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
  initialTab?: 'mcp' | 'services' | 'acp';
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
    api_key: string;
  }) => Promise<void>;
  onSetActiveAIService: (id: string) => Promise<void>;
  onRemoveAIService: (id: string) => Promise<void>;
  onImportAIClient: (target: string, service?: string, replace?: boolean) => Promise<void>;
  onRemoveAIClient: (target: string) => Promise<void>;
  onFetchModels: (input: { base_url: string; api_key: string; protocol: AIServiceProtocol }) => Promise<string[]>;
  onFetchSavedModels: (id: string, base_url: string) => Promise<string[]>;
  onSaveAcpProfile: (input: AcpRuntimeProfileInput) => Promise<void>;
  onSetAcpProfileEnabled: (providerId: string, enabled: boolean) => Promise<void>;
  onRemoveAcpProfile: (providerId: string) => Promise<void>;
};

/**
 * AI 连接按「用户要做什么」分三个 tab，而不是按 HiMind 的内部子系统分。
 * 同一个工具只在「本机工具」里出现一次：展开即见它的接入（MCP）、模型、执行三条接线。
 * 模型「来源」单独成页只管定义与对话默认；执行环境保留原样。
 * tab id 保持稳定，外部脚本与深链依赖它（initialTab 的 mcp / services / acp 键不变）。
 */
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
  onRemoveAIService,
  onImportAIClient,
  onRemoveAIClient,
  onFetchModels,
  onFetchSavedModels,
  onSaveAcpProfile,
  onSetAcpProfileEnabled,
  onRemoveAcpProfile,
}: AiConnectionsPageProps) {
  const [activeTab, setActiveTab] = useState<'mcp' | 'services' | 'acp'>(initialTab);

  useEffect(() => {
    setActiveTab(initialTab);
  }, [initialTab]);

  return (
    <div className="ai-page">
      <PageHeader title="AI 连接" />

      <div className="ai-tabs" role="tablist" aria-label="AI 连接分类">
        <button type="button" id="ai-tab-mcp" role="tab" aria-selected={activeTab === 'mcp'} aria-controls="ai-panel-mcp" className={`ai-tab${activeTab === 'mcp' ? ' active' : ''}`} onClick={() => setActiveTab('mcp')}>本机工具</button>
        <button type="button" id="ai-tab-services" role="tab" aria-selected={activeTab === 'services'} aria-controls="ai-panel-services" className={`ai-tab${activeTab === 'services' ? ' active' : ''}`} onClick={() => setActiveTab('services')}>模型来源</button>
        <button type="button" id="ai-tab-acp" role="tab" aria-selected={activeTab === 'acp'} aria-controls="ai-panel-acp" className={`ai-tab${activeTab === 'acp' ? ' active' : ''}`} onClick={() => setActiveTab('acp')}>执行环境</button>
      </div>

      {activeTab === 'acp' ? (
        <AcpProfilesPanel
          snapshot={acpProfiles}
          busyAction={busyAction}
          onSave={onSaveAcpProfile}
          onSetEnabled={onSetAcpProfileEnabled}
          onRemove={onRemoveAcpProfile}
        />
      ) : activeTab === 'services' ? (
        <div id="ai-panel-services" role="tabpanel" aria-labelledby="ai-tab-services">
          <AiProvidersPanel
            aiServices={aiServices}
            templates={aiServiceTemplates}
            onRefresh={onRefresh}
            onSaveAIService={onSaveAIService}
            onSetActiveAIService={onSetActiveAIService}
            onRemoveAIService={onRemoveAIService}
            onOpenAccount={onOpenAccount}
            onFetchModels={onFetchModels}
            onFetchSavedModels={onFetchSavedModels}
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
