import { useEffect, useState, type ReactNode } from 'react';
import { agentApi, type AgentUpdateStatus, type ApprovalSettings, type LoginState, type OpenHandsRuntimeStatus, type RemoteExecutionSettings, type SvnConnection, type SvnConnectionInput, type UnityEditorSettings } from '../services/agentApi';
import { Bot, Database, Download, ExternalLink, FolderOpen, KeyRound, LoaderCircle, Power, RefreshCw, RotateCcw, Save, ShieldAlert, ShieldCheck, Wrench, X } from 'lucide-react';
import { IconButton, PageHeader, Pill } from '../components/Common';

type SettingsSection = 'remote' | 'accounts' | 'tools' | 'general';

const SETTINGS_SECTIONS = [
  { key: 'remote', label: '远程任务与安全', description: '访问范围与审批', icon: ShieldCheck },
  { key: 'accounts', label: '账号', description: '内网和 SVN', icon: KeyRound },
  { key: 'tools', label: '开发工具', description: '本机编辑器', icon: Wrench },
  { key: 'general', label: '通用', description: '启动与更新', icon: Power },
] satisfies { key: SettingsSection; label: string; description: string; icon: typeof ShieldCheck }[];

export function SettingsPage({
  settings,
  remoteExecutionSettings,
  openHandsRuntimeStatus,
  openHandsRuntimeBusy,
  onRefreshOpenHandsRuntime,
  onInstallOpenHandsRuntime,
  loginState,
  loginModalOpen,
  loginUsername,
  loginPassword,
  onOpenLoginModal,
  onCloseLoginModal,
  onUsernameChange,
  onPasswordChange,
  onSaveLogin,
  onLogoutLogin,
  onOpenInnerAdmin,
  onRemoteExecutionChange,
  onRuleChange,
  onTimeoutChange,
  onAutoStartChange,
  onUnityEditorSettingsChange,
  svnConnections,
  svnModalOpen,
  svnDraft,
  onOpenSvnModal,
  onCloseSvnModal,
  onSvnDraftChange,
  onSaveSvnConnection,
  onTestSvnConnection,
  svnTesting,
  onRemoveSvnConnection,
  updateStatus,
  updateBusy,
  onCheckUpdate,
  onDownloadUpdate,
  onCancelUpdateDownload,
  onInstallUpdate,
  onUpdatePreferences,
}: {
  settings: ApprovalSettings | null;
  remoteExecutionSettings: RemoteExecutionSettings | null;
  openHandsRuntimeStatus: OpenHandsRuntimeStatus | null;
  openHandsRuntimeBusy: boolean;
  onRefreshOpenHandsRuntime: () => void;
  onInstallOpenHandsRuntime: () => void;
  loginState: LoginState | null;
  loginModalOpen: boolean;
  loginUsername: string;
  loginPassword: string;
  onOpenLoginModal: () => void;
  onCloseLoginModal: () => void;
  onUsernameChange: (value: string) => void;
  onPasswordChange: (value: string) => void;
  onSaveLogin: () => void;
  onLogoutLogin: () => void;
  onOpenInnerAdmin: () => void;
  onRemoteExecutionChange: (settings: RemoteExecutionSettings, fullAccessConfirmed?: boolean) => void;
  onRuleChange: (requestType: string, mode: string) => void;
  onTimeoutChange: (seconds: number) => void;
  onAutoStartChange: (enabled: boolean) => void;
  onUnityEditorSettingsChange: (settings: UnityEditorSettings) => void;
  svnConnections: SvnConnection[];
  svnModalOpen: boolean;
  svnDraft: SvnConnectionInput;
  onOpenSvnModal: () => void;
  onCloseSvnModal: () => void;
  onSvnDraftChange: (draft: SvnConnectionInput) => void;
  onSaveSvnConnection: () => void;
  onTestSvnConnection: () => void;
  svnTesting: boolean;
  onRemoveSvnConnection: () => void;
  updateStatus: AgentUpdateStatus | null;
  updateBusy: boolean;
  onCheckUpdate: () => void;
  onDownloadUpdate: () => void;
  onCancelUpdateDownload: () => void;
  onInstallUpdate: () => void;
  onUpdatePreferences: (autoCheck: boolean, autoDownload: boolean) => void;
}) {
  const [unityEditorPath, setUnityEditorPath] = useState('');
  const [unityEditorSettings, setUnityEditorSettings] = useState<UnityEditorSettings | null>(null);
  const [editorFeedback, setEditorFeedback] = useState('');
  const [editorSaving, setEditorSaving] = useState(false);
  const [pendingFullAccess, setPendingFullAccess] = useState<RemoteExecutionSettings | null>(null);
  const [section, setSection] = useState<SettingsSection>('remote');
  useEffect(() => {
    setUnityEditorSettings(settings?.editors || null);
    setUnityEditorPath(settings?.editors?.unity_editor_path || '');
  }, [settings?.editors]);

  async function chooseUnityEditor() {
    const result = await agentApi.pickUnityEditor();
    if (result.path) {
      setUnityEditorPath(result.path);
      setEditorFeedback('');
    }
  }

  async function saveUnityEditor(path = unityEditorPath) {
    setEditorSaving(true);
    setEditorFeedback('');
    try {
      const result = await agentApi.saveUnityEditor(path);
      onUnityEditorSettingsChange(result);
      setUnityEditorSettings(result);
      setUnityEditorPath(result.unity_editor_path);
      setEditorFeedback(path ? '已保存' : result.source === 'environment' ? '已恢复团队默认设置' : '已清除设置，当前没有团队默认编辑器');
    } catch {
      setEditorFeedback('无法保存，请确认 Unity.exe 路径后重试');
    } finally {
      setEditorSaving(false);
    }
  }

  if (!settings || !remoteExecutionSettings || !loginState) return <div className="page-loading"><span className="spinner" />正在读取 Agent 配置</div>;
  const configured = loginState.status === 'credentials_configured';
  const editorState = unityEditorSettings || settings.editors;
  const editorDirty = unityEditorPath.trim() !== (editorState?.unity_editor_path || '');
  const editorStatus = editorState?.valid ? '可用' : editorState?.source === 'unset' ? '未配置' : '路径不可用';
  const editorSource = editorState?.source === 'agent' ? '自定义' : editorState?.source === 'environment' ? '团队默认' : '未设置';
  const updateRemoteExecution = (patch: Partial<RemoteExecutionSettings>) => {
    const next = { ...remoteExecutionSettings, ...patch };
    const enteringFullAccess = next.access_mode === 'full_access'
      && (remoteExecutionSettings.access_mode !== 'full_access' || (!remoteExecutionSettings.enabled && next.enabled));
    if (enteringFullAccess) setPendingFullAccess(next);
    else onRemoteExecutionChange(next);
  };
  return (
    <>
      <PageHeader title="设置" description="管理远程任务、账号、开发工具和启动设置。" />
      <div className="settings-workspace">
        <label className="settings-section-select">
          <span>设置分类</span>
          <select value={section} onChange={event => setSection(event.target.value as SettingsSection)}>
            {SETTINGS_SECTIONS.map(item => <option key={item.key} value={item.key}>{item.label} · {item.description}</option>)}
          </select>
        </label>
        <nav className="settings-nav" aria-label="设置分类">
          {SETTINGS_SECTIONS.map(item => (
            <button key={item.key} className={section === item.key ? 'active' : ''} onClick={() => setSection(item.key)} aria-current={section === item.key ? 'page' : undefined}>
              <item.icon size={16} />
              <span><strong>{item.label}</strong><small>{item.description}</small></span>
            </button>
          ))}
        </nav>
        <div className="settings-content">
          {section === 'remote' ? <>
            <section className="card settings-section">
              <div className="card-header"><span>远程任务</span><Pill kind={remoteExecutionSettings.enabled ? 'success' : 'neutral'}>{remoteExecutionSettings.enabled ? '已启用' : '已关闭'}</Pill></div>
              <div className="card-body setting-list">
                <SettingRow title="接受远程任务" description="只接收当前工作台账号发给本机 Agent 的任务"><label className="toggle"><input type="checkbox" checked={remoteExecutionSettings.enabled} onChange={event => updateRemoteExecution({ enabled: event.target.checked })} /><span className="slider"></span></label></SettingRow>
                <SettingRow title="访问范围" description={remoteExecutionSettings.enabled ? '控制远程任务可操作的本机目录' : '启用远程任务后生效'}>
                  <select aria-label="远程任务访问范围" disabled={!remoteExecutionSettings.enabled} value={remoteExecutionSettings.access_mode} onChange={event => updateRemoteExecution({ access_mode: event.target.value as RemoteExecutionSettings['access_mode'] })}>
                    <option value="exhibit_linked">仅限展项关联目录（推荐）</option>
                    <option value="full_access">允许访问此电脑（高风险）</option>
                  </select>
                </SettingRow>
                <SettingRow title="执行工具" description={remoteExecutionSettings.enabled ? '自动模式会选择本机可用的 AI 工具' : '启用远程任务后生效'}>
                  <select aria-label="远程任务执行工具" disabled={!remoteExecutionSettings.enabled} value={remoteExecutionSettings.default_provider} onChange={event => updateRemoteExecution({ default_provider: event.target.value as RemoteExecutionSettings['default_provider'] })}>
                    <option value="auto">自动选择（推荐）</option><option value="personal.codex">Codex</option><option value="personal.github-copilot">GitHub Copilot</option><option value="himind.openhands" disabled={openHandsRuntimeStatus?.status !== 'ready'}>OpenHands{openHandsRuntimeStatus?.status === 'ready' ? '' : '（未安装）'}</option>
                  </select>
                </SettingRow>
              </div>
            </section>
            <section className="card settings-section openhands-runtime-card">
              <div className="card-header">
                <span>OpenHands Runtime</span>
                <Pill kind={openHandsRuntimeStatus?.status === 'ready' ? 'success' : openHandsRuntimeStatus ? 'warn' : 'neutral'}>
                  {openHandsRuntimeStatus?.status === 'ready' ? '已安装' : openHandsRuntimeStatus ? '未安装' : '检测中'}
                </Pill>
              </div>
              <div className="runtime-summary">
                <div className="runtime-summary-main">
                  <div>
                    <strong>{openHandsRuntimeStatus?.version || 'OpenHands 可选运行时'}</strong>
                    <span>{openHandsRuntimeStatus?.message || '正在检查本机 OpenHands、uv 与 Python 3.12。'}</span>
                  </div>
                  <div className="actions-row runtime-actions">
                    <button className="btn" disabled={openHandsRuntimeBusy} onClick={onRefreshOpenHandsRuntime}>
                      {openHandsRuntimeBusy ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />}重新检测
                    </button>
                    <button className="btn btn-primary" disabled={openHandsRuntimeBusy} onClick={onInstallOpenHandsRuntime}>
                      {openHandsRuntimeBusy ? <LoaderCircle className="spin" size={15} /> : <Download size={15} />}
                      {openHandsRuntimeStatus?.status === 'ready' ? '修复安装' : '安装 OpenHands'}
                    </button>
                  </div>
                </div>
                <details className="runtime-details">
                  <summary>安装详情</summary>
                  <div className="runtime-facts">
                    <div><span>uv</span><strong>{openHandsRuntimeStatus?.uv_available ? openHandsRuntimeStatus.uv_version : '未检测到'}</strong></div>
                    <div><span>CLI 参数预检</span><strong>{openHandsRuntimeStatus?.cli_compatible ? '通过' : '未通过'}</strong></div>
                    <div><span>Python 3.12</span><strong>{openHandsRuntimeStatus?.python_available ? openHandsRuntimeStatus.python_version : '未检测到（uv 会按需安装）'}</strong></div>
                    <div><span>命令</span><code>{openHandsRuntimeStatus?.executable_path || 'openhands'}</code></div>
                  </div>
                </details>
                {openHandsRuntimeStatus && openHandsRuntimeStatus.status === 'error' ? <div className="runtime-prerequisite"><ShieldAlert size={16} /><span>{openHandsRuntimeStatus.message}</span></div> : null}
              </div>
            </section>
            <section className="card settings-section">
              <div className="card-header">操作审批</div>
              <div className="card-body setting-list">
                <SettingRow title="远程协助" description="收到远程控制或协助请求时的处理方式"><select aria-label="远程协助审批模式" value={settings.rules?.remote_connect || 'manual'} onChange={event => onRuleChange('remote_connect', event.target.value)}><option value="manual">每次询问</option><option value="auto_approve">自动允许</option><option value="auto_deny">自动拒绝</option></select></SettingRow>
                <SettingRow title="文件上传" description="收到代码或制品上传请求时的处理方式"><select aria-label="文件上传审批模式" value={settings.rules?.upload_code || 'manual'} onChange={event => onRuleChange('upload_code', event.target.value)}><option value="manual">每次询问</option><option value="auto_approve">自动允许</option><option value="auto_deny">自动拒绝</option></select></SettingRow>
                <SettingRow title="审批超时" description="未响应时自动拒绝请求"><select aria-label="审批超时" value={settings.timeout_seconds} onChange={event => onTimeoutChange(Number(event.target.value))}><option value="15">15 秒</option><option value="30">30 秒</option><option value="60">60 秒</option><option value="120">120 秒</option></select></SettingRow>
              </div>
            </section>
          </> : null}

          {section === 'accounts' ? <section className="card settings-section settings-credentials">
            <div className="card-header">账号</div>
            <div className="credential-section">
              <div className="credential-heading"><span>内网账号</span><Pill kind={configured ? 'success' : 'warn'}>{configured ? '已配置' : '待配置'}</Pill></div>
              <div className="account-row">
                <div className="account-icon"><KeyRound size={18} /></div>
                <div><span>当前账号</span><strong>{loginState.account || '未保存账号'}</strong></div>
                <button className="btn" onClick={onOpenLoginModal}>{configured ? '更新账号' : '配置账号'}</button>
              </div>
              <div className="security-note compact"><ShieldCheck size={16} /><span>账号信息加密保存在本机，不会写入工作台或日志。</span></div>
            </div>
            <div className="credential-section">
              <div className="credential-heading"><span>SVN 账号</span><Pill kind={svnConnections[0]?.status === 'ready' ? 'success' : 'warn'}>{!svnConnections.length ? '待配置' : svnConnections[0]?.status === 'ready' ? '已验证' : svnConnections[0]?.status === 'invalid' ? '凭据失效' : svnConnections[0]?.status === 'unreachable' ? '服务不可达' : '待验证'}</Pill></div>
              {svnConnections[0] ? <div className="svn-connection-row">
                <div className="account-icon"><Database size={17} /></div>
                <div className="svn-connection-main"><strong>{svnConnections[0].username}</strong><span>{svnConnections[0].base_url}</span><small>{svnConnections[0].status === 'invalid' ? '请更新账号并重新测试' : svnConnections[0].status === 'unreachable' ? '请检查 SVN 服务和本机客户端' : '项目仓库地址由项目自动生成'}</small></div>
                <div className="actions-row svn-connection-actions"><button className="btn" disabled={svnTesting} onClick={onTestSvnConnection}>{svnTesting ? <><LoaderCircle size={14} className="spin" />测试中</> : '测试'}</button><button className="btn" disabled={svnTesting} onClick={onOpenSvnModal}>更新账号</button><button className="btn btn-danger-quiet" disabled={svnTesting} onClick={onRemoveSvnConnection}>清除</button></div>
              </div> : <div className="account-row"><div className="account-icon"><Database size={17} /></div><div><span>公司 SVN</span><strong>尚未配置个人账号</strong></div><button className="btn btn-primary" onClick={onOpenSvnModal}>配置账号</button></div>}
              <div className="security-note compact"><ShieldCheck size={16} /><span>密码只保存在当前 Windows 用户的本地加密存储中。</span></div>
            </div>
          </section> : null}

          {section === 'tools' ? <section className="card settings-section unity-editor-card">
            <div className="card-header"><span>Unity 编辑器</span><Pill kind={editorState?.valid ? 'success' : 'warn'}>{editorStatus}</Pill></div>
            <div className="unity-editor-body">
              <div className="unity-editor-source">
                <div><span>当前来源</span><strong>{editorSource}</strong></div>
                <small>{editorState?.source === 'environment' ? '当前使用团队提供的默认编辑器。' : '没有团队默认编辑器时，可以在此选择 Unity.exe。'}</small>
              </div>
              <label className="unity-editor-field" htmlFor="unity-editor-path">
                <span>Unity.exe 路径</span>
                <div className="unity-editor-input">
                  <input id="unity-editor-path" value={unityEditorPath} onChange={event => { setUnityEditorPath(event.target.value); setEditorFeedback(''); }} placeholder="请选择 Unity.exe" />
                  <button className="btn btn-icon" title="选择 Unity.exe" aria-label="选择 Unity.exe" onClick={chooseUnityEditor}><FolderOpen size={16} /></button>
                </div>
              </label>
              <div className="unity-editor-footer">
                <span className={editorFeedback ? 'inline-feedback visible' : 'inline-feedback'}>{editorFeedback || (editorDirty ? '路径尚未保存' : '')}</span>
                <div className="unity-editor-actions">
                  <button className="btn" disabled={editorSaving || (!editorDirty && editorState?.source !== 'agent')} onClick={() => saveUnityEditor('')}><RotateCcw size={15} />恢复默认</button>
                  <button className="btn btn-primary" disabled={editorSaving || !editorDirty || !unityEditorPath.trim()} onClick={() => saveUnityEditor()}>{editorSaving ? <LoaderCircle className="spin" size={15} /> : <Save size={15} />}保存</button>
                </div>
              </div>
            </div>
          </section> : null}

          {section === 'general' ? <>
            <section className="card settings-section">
              <div className="card-header">软件更新</div>
              <div className="software-update-summary">
                <div><span>当前版本</span><strong>v{updateStatus?.current_version || '—'}</strong></div>
                <div><span>最近检查</span><strong>{formatUpdateTime(updateStatus?.last_checked_at)}</strong></div>
              </div>
              <div className="card-body setting-list">
                <SettingRow title="自动检查更新" description="定期检查是否有新版本"><label className="toggle"><input type="checkbox" checked={updateStatus?.auto_check ?? true} onChange={event => onUpdatePreferences(event.target.checked, event.target.checked && (updateStatus?.auto_download ?? true))} /><span className="slider"></span></label></SettingRow>
                <SettingRow title="自动下载更新" description="有新版本时在后台下载，安装前会通知你"><label className="toggle"><input type="checkbox" disabled={!updateStatus?.auto_check} checked={updateStatus?.auto_download ?? true} onChange={event => onUpdatePreferences(updateStatus?.auto_check ?? true, event.target.checked)} /><span className="slider"></span></label></SettingRow>
              </div>
              <div className="software-update-state">
                <div>
                  <strong>{describeUpdateState(updateStatus)}</strong>
                  <span>{describeUpdateMessage(updateStatus)}</span>
                  {updateStatus?.status === 'downloading' ? <div className="agent-update-progress"><span style={{ width: `${updateStatus.progress_percent}%` }} /></div> : null}
                </div>
                <div className="actions-row">
                  {updateStatus?.status === 'downloading' ? <button className="btn" onClick={onCancelUpdateDownload}>取消下载</button> : null}
                  {updateStatus?.available_version && !['downloading', 'ready', 'installing'].includes(updateStatus.status) ? <button className="btn" disabled={updateBusy} onClick={onDownloadUpdate}><Download size={15} />下载更新</button> : null}
                  {updateStatus?.status === 'ready' ? <button className="btn btn-primary" disabled={updateBusy} onClick={onInstallUpdate}><RefreshCw size={15} />重启并更新</button> : null}
                  <button className="btn" disabled={updateBusy || ['checking', 'downloading', 'installing'].includes(updateStatus?.status || '')} onClick={onCheckUpdate}>{updateBusy || updateStatus?.status === 'checking' ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />}检查更新</button>
                </div>
              </div>
            </section>
            <section className="card settings-section">
              <div className="card-header">Agent 启动</div>
              <div className="card-body setting-list">
                <SettingRow title="开机自启" description="登录 Windows 后自动启动 HiMind Agent"><label className="toggle"><input type="checkbox" checked={settings.auto_start} onChange={event => onAutoStartChange(event.target.checked)} /><span className="slider"></span></label></SettingRow>
              </div>
            </section>
          </> : null}
        </div>
      </div>
      {loginModalOpen ? <LoginModal configured={configured} username={loginUsername} password={loginPassword} onClose={onCloseLoginModal} onUsernameChange={onUsernameChange} onPasswordChange={onPasswordChange} onSave={onSaveLogin} onLogout={onLogoutLogin} onOpenInnerAdmin={onOpenInnerAdmin} /> : null}
      {svnModalOpen ? <SvnConnectionModal draft={svnDraft} exists={svnConnections.length > 0} onClose={onCloseSvnModal} onChange={onSvnDraftChange} onSave={onSaveSvnConnection} /> : null}
      {pendingFullAccess ? <FullAccessConfirmation onClose={() => setPendingFullAccess(null)} onConfirm={() => { onRemoteExecutionChange(pendingFullAccess, true); setPendingFullAccess(null); }} /> : null}
    </>
  );
}

function formatUpdateTime(timestamp?: number) {
  if (!timestamp) return '尚未检查';
  return new Date(timestamp * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
}

function describeUpdateState(status: AgentUpdateStatus | null) {
  if (!status) return '正在读取更新状态';
  if (status.status === 'checking') return '正在检查更新';
  if (status.status === 'downloading') return `正在下载 v${status.available_version} · ${status.progress_percent}%`;
  if (status.status === 'ready') return `v${status.available_version} 更新已下载`;
  if (status.status === 'installing') return '正在重启并安装更新';
  if (status.status === 'failed') return '更新未完成';
  if (status.status === 'rolled_back') return '新版本未能启动，已恢复上一版本';
  if (status.available_version) return `可更新到 v${status.available_version}`;
  return '当前已是最新版本';
}

function describeUpdateMessage(status: AgentUpdateStatus | null) {
  if (!status) return '正在读取更新信息。';
  if (status.status === 'checking') return '请稍候。';
  if (status.status === 'downloading') return '下载完成后会通知你。';
  if (status.status === 'ready') return '重启 HiMind Agent 后完成安装。';
  if (status.status === 'installing') return '更新完成后会自动重新启动。';
  if (status.status === 'failed') return '暂时无法完成更新，请稍后重试。';
  if (status.status === 'rolled_back') return '更新没有完成，仍在使用上一版本。';
  return status.release_notes || 'HiMind Agent 会定期检查更新。';
}

function FullAccessConfirmation({ onClose, onConfirm }: { onClose: () => void; onConfirm: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="full-access-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="full-access-title">允许访问此电脑？</h3><p>仅在远程任务确实需要时开启。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="full-access-warning"><ShieldAlert size={20} /><div><strong>远程任务可以访问你有权限使用的文件和文件夹</strong><span>任务可能读取、创建或修改展项目录以外的内容。只对可信任务开启此权限。</span></div></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={onConfirm}><Bot size={15} />确认启用</button></div></div>
        </div>
      </div>
    </div>
  );
}

function SvnConnectionModal({ draft, exists, onClose, onChange, onSave }: { draft: SvnConnectionInput; exists: boolean; onClose: () => void; onChange: (draft: SvnConnectionInput) => void; onSave: () => void }) {
  const update = (field: keyof SvnConnectionInput, value: string) => onChange({ ...draft, [field]: value });
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="svn-modal-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="svn-modal-title">{exists ? '更新 SVN 账号' : '配置 SVN 账号'}</h3><p>用于访问公司 SVN 中当前账号有权限的项目仓库。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="field-group"><label className="field-label" htmlFor="svn-username">账号</label><input id="svn-username" autoComplete="username" value={draft.username} onChange={event => update('username', event.target.value)} /></div>
          <div className="field-group"><label className="field-label" htmlFor="svn-password">密码</label><input id="svn-password" autoComplete="current-password" type="password" value={draft.password} onChange={event => update('password', event.target.value)} placeholder={exists ? '留空以保留当前密码' : '输入 SVN 密码'} /></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" onClick={onSave} disabled={!draft.username.trim() || (!exists && !draft.password)}>保存账号</button></div></div>
        </div>
      </div>
    </div>
  );
}

function SettingRow({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <div className="setting-row">
      <div><div className="label-text">{title}</div><div className="label-desc">{description}</div></div>
      <div className="setting-control">{children}</div>
    </div>
  );
}

function LoginModal({ configured, username, password, onClose, onUsernameChange, onPasswordChange, onSave, onLogout, onOpenInnerAdmin }: { configured: boolean; username: string; password: string; onClose: () => void; onUsernameChange: (value: string) => void; onPasswordChange: (value: string) => void; onSave: () => void; onLogout: () => void; onOpenInnerAdmin: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="login-modal-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="login-modal-title">配置内网账号</h3><p>凭据仅保存在当前 Windows Agent。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="field-group"><label className="field-label" htmlFor="login-username">内网账号</label><input id="login-username" autoComplete="username" value={username} onChange={event => onUsernameChange(event.target.value)} placeholder="输入内网平台用户名" /></div>
          <div className="field-group"><label className="field-label" htmlFor="login-password">内网密码</label><input id="login-password" autoComplete="current-password" type="password" value={password} onChange={event => onPasswordChange(event.target.value)} placeholder={configured ? '输入新密码以更新凭据' : '输入内网平台密码'} /></div>
          <button className="text-action" onClick={onOpenInnerAdmin}><ExternalLink size={14} />打开内网平台</button>
          <div className="modal-actions"><div>{configured ? <button className="btn btn-danger-quiet" onClick={onLogout}>清除凭据</button> : null}</div><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" onClick={onSave} disabled={!username.trim() || !password}>保存凭据</button></div></div>
        </div>
      </div>
    </div>
  );
}
