import { useEffect, useState, type ReactNode } from 'react';
import { agentApi, type ApprovalSettings, type LoginState, type SvnConnection, type SvnConnectionInput } from '../services/agentApi';
import { Database, ExternalLink, FolderOpen, KeyRound, RotateCcw, Save, ShieldCheck, X } from 'lucide-react';
import { IconButton, PageHeader, Pill } from '../components/Common';

export function SettingsPage({
  settings,
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
  onRuleChange,
  onTimeoutChange,
  onAutoStartChange,
  svnConnections,
  svnModalOpen,
  svnDraft,
  onOpenSvnModal,
  onCloseSvnModal,
  onSvnDraftChange,
  onSaveSvnConnection,
  onTestSvnConnection,
  onRemoveSvnConnection,
}: {
  settings: ApprovalSettings | null;
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
  onRuleChange: (requestType: string, mode: string) => void;
  onTimeoutChange: (seconds: number) => void;
  onAutoStartChange: (enabled: boolean) => void;
  svnConnections: SvnConnection[];
  svnModalOpen: boolean;
  svnDraft: SvnConnectionInput;
  onOpenSvnModal: () => void;
  onCloseSvnModal: () => void;
  onSvnDraftChange: (draft: SvnConnectionInput) => void;
  onSaveSvnConnection: () => void;
  onTestSvnConnection: () => void;
  onRemoveSvnConnection: () => void;
}) {
  const [unityEditorPath, setUnityEditorPath] = useState('');
  const [editorFeedback, setEditorFeedback] = useState('');
  const [editorSaving, setEditorSaving] = useState(false);
  useEffect(() => setUnityEditorPath(settings?.editors?.unity_editor_path || ''), [settings?.editors?.unity_editor_path]);

  async function chooseUnityEditor() {
    const result = await agentApi.pickUnityEditor();
    if (result.path) setUnityEditorPath(result.path);
  }

  async function saveUnityEditor(path = unityEditorPath) {
    setEditorSaving(true);
    setEditorFeedback('');
    try {
      await agentApi.saveUnityEditor(path);
      setUnityEditorPath(path);
      setEditorFeedback(path ? '默认 Unity 编辑器已保存' : '已恢复环境变量或自动发现');
    } catch (error) {
      setEditorFeedback(error instanceof Error ? error.message : String(error));
    } finally {
      setEditorSaving(false);
    }
  }

  if (!settings || !loginState) return <div className="page-loading"><span className="spinner" />正在读取 Agent 配置</div>;
  const configured = loginState.status === 'credentials_configured';
  return (
    <>
      <PageHeader title="设置" description="管理本机账号、操作确认和启动设置。" />
      <div className="settings-layout">
        <section className="card settings-section">
          <div className="card-header"><span>内网账号</span><Pill kind={configured ? 'success' : 'warn'}>{configured ? '已配置' : '待配置'}</Pill></div>
          <div className="card-body">
            <div className="account-row">
              <div className="account-icon"><KeyRound size={18} /></div>
              <div><span>当前账号</span><strong>{loginState.account || '未保存账号'}</strong></div>
              <button className="btn" onClick={onOpenLoginModal}>{configured ? '更新凭据' : '配置账号'}</button>
            </div>
            <div className="security-note compact"><ShieldCheck size={16} /><span>账号信息已加密保存在本机，不会写入 HiMind 工作台或日志。</span></div>
          </div>
        </section>
        <section className="card settings-section">
          <div className="card-header"><span>SVN 账号</span><Pill kind={svnConnections[0]?.status === 'ready' ? 'success' : 'warn'}>{!svnConnections.length ? '待配置' : svnConnections[0]?.status === 'ready' ? '已验证' : svnConnections[0]?.status === 'invalid' ? '凭据失效' : svnConnections[0]?.status === 'unreachable' ? '服务不可达' : '待验证'}</Pill></div>
          <div className="card-body">
            {svnConnections[0] ? <div className="svn-connection-list">
              <div className="svn-connection-row">
                <div className="account-icon"><Database size={17} /></div>
                <div className="svn-connection-main"><strong>{svnConnections[0].username}</strong><span>http://svn.andcrane.com/</span><small>{svnConnections[0].status === 'invalid' ? '请更新账号并重新测试' : svnConnections[0].status === 'unreachable' ? '请检查 SVN 服务和本机客户端' : '项目仓库地址由项目自动生成'}</small></div>
                <div className="actions-row"><button className="btn" onClick={onTestSvnConnection}>测试</button><button className="btn" onClick={onOpenSvnModal}>更新账号</button><button className="btn btn-danger-quiet" onClick={onRemoveSvnConnection}>清除</button></div>
              </div>
            </div> : <div className="account-row"><div className="account-icon"><Database size={17} /></div><div><span>公司 SVN</span><strong>尚未配置个人账号</strong></div><button className="btn btn-primary" onClick={onOpenSvnModal}>配置账号</button></div>}
            <div className="security-note compact"><ShieldCheck size={16} /><span>密码只保存在当前 Windows 用户的本地加密存储中，不会发送给 HiMind 工作台或 AI 工具。</span></div>
          </div>
        </section>
        <section className="card settings-section">
          <div className="card-header">审批策略</div>
          <div className="card-body setting-list">
            <SettingRow title="远程连接" description="收到远程协助请求时的处理方式"><select aria-label="远程连接审批模式" value={settings.rules?.remote_connect || 'manual'} onChange={event => onRuleChange('remote_connect', event.target.value)}><option value="manual">每次询问</option><option value="auto_approve">自动允许</option><option value="auto_deny">自动拒绝</option></select></SettingRow>
            <SettingRow title="文件上传" description="收到代码或制品上传请求时的处理方式"><select aria-label="文件上传审批模式" value={settings.rules?.upload_code || 'manual'} onChange={event => onRuleChange('upload_code', event.target.value)}><option value="manual">每次询问</option><option value="auto_approve">自动允许</option><option value="auto_deny">自动拒绝</option></select></SettingRow>
            <SettingRow title="审批超时" description="未响应时自动拒绝请求"><select aria-label="审批超时" value={settings.timeout_seconds} onChange={event => onTimeoutChange(Number(event.target.value))}><option value="15">15 秒</option><option value="30">30 秒</option><option value="60">60 秒</option><option value="120">120 秒</option></select></SettingRow>
          </div>
        </section>
        <section className="card settings-section">
          <div className="card-header"><span>Unity 编辑器</span><Pill kind={settings.editors?.valid ? 'success' : 'warn'}>{settings.editors?.valid ? '可用' : '待配置'}</Pill></div>
          <div className="card-body setting-list">
            <SettingRow title="默认编辑器" description={settings.editors?.source === 'agent' ? 'Agent 本地配置' : settings.editors?.source === 'environment' ? 'unity_art_editor 环境变量' : '自动发现已安装版本'}>
              <div className="editor-path-control">
                <input aria-label="默认 Unity 编辑器路径" value={unityEditorPath} onChange={event => setUnityEditorPath(event.target.value)} placeholder="选择 Unity.exe" />
                <div className="actions-row">
                  <button className="icon-button" title="选择 Unity.exe" aria-label="选择 Unity.exe" onClick={chooseUnityEditor}><FolderOpen size={16} /></button>
                  <button className="icon-button" title="保存默认编辑器" aria-label="保存默认编辑器" disabled={editorSaving} onClick={() => saveUnityEditor()}><Save size={16} /></button>
                  <button className="icon-button" title="恢复环境变量或自动发现" aria-label="恢复环境变量或自动发现" disabled={editorSaving} onClick={() => saveUnityEditor('')}><RotateCcw size={16} /></button>
                </div>
              </div>
            </SettingRow>
            {editorFeedback ? <div className="inline-feedback">{editorFeedback}</div> : null}
          </div>
        </section>
        <section className="card settings-section">
          <div className="card-header">系统启动</div>
          <div className="card-body setting-list">
            <SettingRow title="开机自启" description="登录 Windows 后自动启动 HiMind Agent"><label className="toggle"><input type="checkbox" checked={settings.auto_start} onChange={event => onAutoStartChange(event.target.checked)} /><span className="slider"></span></label></SettingRow>
          </div>
        </section>
      </div>
      {loginModalOpen ? <LoginModal configured={configured} username={loginUsername} password={loginPassword} onClose={onCloseLoginModal} onUsernameChange={onUsernameChange} onPasswordChange={onPasswordChange} onSave={onSaveLogin} onLogout={onLogoutLogin} onOpenInnerAdmin={onOpenInnerAdmin} /> : null}
      {svnModalOpen ? <SvnConnectionModal draft={svnDraft} exists={svnConnections.length > 0} onClose={onCloseSvnModal} onChange={onSvnDraftChange} onSave={onSaveSvnConnection} /> : null}
    </>
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
