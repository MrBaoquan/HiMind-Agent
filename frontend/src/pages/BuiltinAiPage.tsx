import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { ArrowUpRight, Blocks, Bot, Check, CircleAlert, ChevronDown, LoaderCircle, LogIn, MessageCircle, RefreshCw, Settings } from 'lucide-react';
import { agentApi, type BuiltinAIModelOptions, type BuiltinAIToolContextSummary, type DashboardAuthorizationProgress, type DashboardIdentityStatus } from '../services/agentApi';
import { errorDetail } from '../types';
import { BuiltinAiExtensionsDialog } from '../components/BuiltinAiExtensionsDialog';

type BuiltinAiPageProps = {
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  authorizationBusy: boolean;
  onStartAuthorization: () => void;
  onCancelAuthorization: () => void;
  onOpenAuthorization: () => void;
  onOpenSettings: () => void;
  onOpenAiConnections: () => void;
  onOpenPlugins: () => void;
  onOpenSkills: () => void;
  onToolContextChanged: () => void;
  modelOptions: BuiltinAIModelOptions | null;
  modelOptionsLoading: boolean;
  toolSummary: BuiltinAIToolContextSummary;
};

export function BuiltinAiPage({
  identity,
  authorization,
  authorizationBusy,
  onStartAuthorization,
  onCancelAuthorization,
  onOpenAuthorization,
  onOpenSettings,
  onOpenAiConnections,
  onOpenPlugins,
  onOpenSkills,
  onToolContextChanged,
  modelOptions,
  modelOptionsLoading,
  toolSummary,
}: BuiltinAiPageProps) {
  const [sessionUrl, setSessionUrl] = useState('');
  const [connecting, setConnecting] = useState(false);
  const [frameLoaded, setFrameLoaded] = useState(false);
  const [connectionError, setConnectionError] = useState('');
  const [selectedModel, setSelectedModel] = useState('');
  const [extensionsOpen, setExtensionsOpen] = useState(false);
  const authorizationActive = authorization?.state === 'starting' || authorization?.state === 'pending';
  const activeModel = selectedModel || modelOptions?.selected_model || '';

  useEffect(() => {
    if (!modelOptions) return;
    setSelectedModel(current => {
      if (current && modelOptions.models.includes(current)) return current;
      return modelOptions.selected_model;
    });
  }, [modelOptions]);

  const connect = useCallback(async () => {
    if (connecting) return;
    setConnecting(true);
    setConnectionError('');
    setFrameLoaded(false);
    try {
      setSessionUrl(await agentApi.startBuiltinAiSession(selectedModel || undefined));
    } catch (error) {
      setSessionUrl('');
      setConnectionError(presentConnectionError(error));
    } finally {
      setConnecting(false);
    }
  }, [connecting, selectedModel]);

  const selectModel = useCallback(async (model: string) => {
    if (!model || model === activeModel) return;
    setSelectedModel(model);
    setConnectionError('');
    if (!sessionUrl) return;
    setSessionUrl('');
    setConnecting(true);
    setConnectionError('');
    setFrameLoaded(false);
    try {
      setSessionUrl(await agentApi.restartBuiltinAiSession(model));
    } catch (error) {
      setConnectionError(presentConnectionError(error));
    } finally {
      setConnecting(false);
    }
  }, [activeModel, sessionUrl]);

  useEffect(() => {
    if (!identity?.authorized || modelOptionsLoading || sessionUrl || connecting || connectionError) return;
    void connect();
  }, [connect, connecting, connectionError, identity?.authorized, modelOptionsLoading, sessionUrl]);

  return (
    <section className="builtin-ai-page" aria-label="HiMind AI">
      <header className="builtin-ai-toolbar">
        <div className="builtin-ai-title">
          <span className="builtin-ai-mark"><MessageCircle size={17} /></span>
          <div><h2>HiMind AI</h2><span>{sessionUrl ? '已连接' : '智能工作助手'}</span></div>
        </div>
        <div className="builtin-ai-toolbar-actions">
          {modelOptionsLoading ? <span className="builtin-ai-model-loading"><LoaderCircle className="spin" size={13} />正在读取模型</span> : null}
          {!modelOptionsLoading && modelOptions?.models.length ? (
            <ModelPicker options={modelOptions} selectedModel={activeModel} sessionActive={Boolean(sessionUrl)} disabled={connecting} onSelect={selectModel} onOpenAiConnections={onOpenAiConnections} />
          ) : null}
          <button type="button" className={`builtin-ai-tools-button ${extensionsOpen ? 'active' : ''}`} onClick={() => setExtensionsOpen(true)} title="管理扩展"><Blocks size={15} />扩展</button>
          {sessionUrl ? <span className="builtin-ai-online"><i />可用</span> : null}
        </div>
      </header>

      <div className="builtin-ai-workspace">
        {sessionUrl ? (
          <>
            {!frameLoaded ? <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在打开会话" description="马上就好" /> : null}
            <iframe
              className={frameLoaded ? 'loaded' : ''}
              title="HiMind AI 会话"
              src={sessionUrl}
              onLoad={() => setFrameLoaded(true)}
              referrerPolicy="no-referrer"
            />
          </>
        ) : authorizationActive ? (
          <WorkspaceStatus
            icon={<LogIn size={22} />}
            title={authorization?.state === 'starting' ? '正在打开登录页面' : '请在浏览器中确认登录'}
            description={authorization?.user_code ? `确认码 ${authorization.user_code}` : '确认后会自动返回并连接 HiMind AI'}
            actions={<>
              {authorization?.verification_uri_complete ? <button type="button" className="btn btn-primary" onClick={onOpenAuthorization}><ArrowUpRight size={15} />打开确认页面</button> : null}
              <button type="button" className="btn" onClick={onCancelAuthorization}>取消</button>
            </>}
          />
        ) : identity === null ? (
          <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在准备 HiMind AI" description="正在检查账号状态" />
        ) : !identity.authorized ? (
          <WorkspaceStatus
            icon={<LogIn size={22} />}
            title="登录后开始对话"
            description="使用组织提供的 AI 服务，无需单独配置账号或密钥"
            actions={<button type="button" className="btn btn-primary" disabled={authorizationBusy} onClick={onStartAuthorization}>{authorizationBusy ? <LoaderCircle className="spin" size={15} /> : <LogIn size={15} />}{authorizationBusy ? '正在连接' : '登录 HiMind'}</button>}
          />
        ) : connecting ? (
          <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在准备会话" description="首次启动可能需要一点时间" />
        ) : connectionError ? (
          <WorkspaceStatus
            tone="error"
            icon={<CircleAlert size={22} />}
            title="暂时无法连接 HiMind AI"
            description={connectionError}
            actions={<>
              <button type="button" className="btn btn-primary" onClick={() => void connect()}><RefreshCw size={15} />重新连接</button>
              {connectionError.includes('组件') ? <button type="button" className="btn" onClick={onOpenSettings}><Settings size={15} />打开设置</button> : null}
            </>}
          />
        ) : (
          <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在准备 HiMind AI" description="马上就好" />
        )}
      </div>
      <BuiltinAiExtensionsDialog
        open={extensionsOpen}
        toolSummary={toolSummary}
        onClose={() => setExtensionsOpen(false)}
        onRuntimeChanged={() => {
          setSessionUrl('');
          setFrameLoaded(false);
          setConnectionError('');
        }}
        onToolContextChanged={onToolContextChanged}
        onOpenPlugins={() => { setExtensionsOpen(false); onOpenPlugins(); }}
        onOpenSkills={() => { setExtensionsOpen(false); onOpenSkills(); }}
      />
    </section>
  );
}

function ModelPicker({ options, selectedModel, sessionActive, disabled, onSelect, onOpenAiConnections }: { options: BuiltinAIModelOptions; selectedModel: string; sessionActive: boolean; disabled: boolean; onSelect: (model: string) => void; onOpenAiConnections: () => void }) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const sourceTypeLabel = options.source_type === 'personal' ? '个人服务' : '组织服务';
  const sourceName = options.source_name || sourceTypeLabel;
  const sourceProvider = options.source_provider && options.source_provider !== sourceName ? options.source_provider : '';

  useEffect(() => {
    if (!open) return;
    const closeOnPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', closeOnPointerDown);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOnPointerDown);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [open]);

  return (
    <div className="builtin-ai-model-picker" ref={rootRef}>
      <button type="button" className={open ? 'builtin-ai-model-trigger active' : 'builtin-ai-model-trigger'} disabled={disabled} onClick={() => setOpen(current => !current)} aria-haspopup="listbox" aria-expanded={open} title="选择模型">
        <span className="builtin-ai-model-icon"><Bot size={15} /></span>
        <span className="builtin-ai-model-current"><small>{sourceTypeLabel} · {options.models.length} 个模型</small><strong>{selectedModel}</strong></span>
        {disabled ? <LoaderCircle className="spin" size={14} /> : <ChevronDown size={14} aria-hidden="true" />}
      </button>
      {open ? (
        <div className="builtin-ai-model-menu" role="listbox" aria-label="可用模型">
          <header><div><strong>选择模型</strong><span>{options.models.length} 个可用</span></div><span className={options.source_type === 'personal' ? 'personal' : ''}>{sourceTypeLabel}</span></header>
          <div className="builtin-ai-model-source"><span>来自</span><div><strong>{sourceName}</strong>{sourceProvider ? <small>{sourceProvider}</small> : null}</div></div>
          <div className="builtin-ai-model-options">
            {options.models.map(model => (
              <button type="button" role="option" aria-selected={model === selectedModel} className={model === selectedModel ? 'selected' : ''} key={model} onClick={() => { setOpen(false); onSelect(model); }}>
                <span><strong>{model}</strong>{model === options.selected_model ? <small>默认</small> : null}</span>
                {model === selectedModel ? <Check size={16} /> : null}
              </button>
            ))}
          </div>
          <footer><span>{sessionActive && options.models.length > 1 ? '切换模型会开始新会话' : options.models.length === 1 ? '当前服务仅提供此模型' : '模型由当前服务提供'}</span><button type="button" onClick={() => { setOpen(false); onOpenAiConnections(); }}>管理 AI 连接</button></footer>
        </div>
      ) : null}
    </div>
  );
}

function WorkspaceStatus({ icon, title, description, actions, tone = 'default' }: { icon: ReactNode; title: string; description: string; actions?: ReactNode; tone?: 'default' | 'error' }) {
  return <div className={`builtin-ai-state ${tone}`} role={tone === 'error' ? 'alert' : 'status'}><span className="builtin-ai-state-icon">{icon}</span><h3>{title}</h3><p>{description}</p>{actions ? <div className="builtin-ai-state-actions">{actions}</div> : null}</div>;
}

function presentConnectionError(error: unknown) {
  const detail = errorDetail(error);
  const normalized = detail.toLowerCase();
  if (normalized.includes('登录 himind')) return '当前登录状态已失效，请重新登录。';
  if (normalized.includes('ai 服务')) return '当前账号暂未分配可用的 AI 服务，请联系管理员。';
  if (normalized.includes('组件')) return '内置 AI 组件需要修复，请在设置中重新安装。';
  if (normalized.includes('正在启动')) return '会话仍在准备中，请稍后重新连接。';
  return '服务暂时不可用，请稍后重试。';
}
