import { useEffect, useState } from 'react';
import { Bot, CircleAlert, CircleCheck, Pencil, Plus, Power, RefreshCw, Save, Trash2, X } from 'lucide-react';
import { Pill } from '../components/Common';
import type { AcpRuntimeProfile, AcpRuntimeProfileInput, AcpRuntimeProfileSnapshot } from '../services/agentApi';

type AcpProfilesPanelProps = {
  snapshot: AcpRuntimeProfileSnapshot | null;
  busyAction: string | null;
  onRefresh: () => void;
  onSave: (input: AcpRuntimeProfileInput) => Promise<void>;
  onSetEnabled: (providerId: string, enabled: boolean) => Promise<void>;
  onRemove: (providerId: string) => Promise<void>;
};

type ProfileDraft = {
  providerId: string;
  displayName: string;
  executable: string;
  argsText: string;
  version: string;
  permissionPolicy: 'deny' | 'allow_once' | 'prompt';
  enabled: boolean;
};

const emptyDraft: ProfileDraft = {
  providerId: '',
  displayName: '',
  executable: '',
  argsText: '[]',
  version: '',
  permissionPolicy: 'deny',
  enabled: true,
};

const recommendedProfiles: Array<{ label: string; draft: ProfileDraft }> = [
  {
    label: 'Codex ACP',
    draft: {
      providerId: 'codex',
      displayName: 'Codex ACP',
      executable: 'npx',
      argsText: '[\n  "-y",\n  "@agentclientprotocol/codex-acp@1.12.0"\n]',
      version: '1.12.0',
      permissionPolicy: 'prompt',
      enabled: true,
    },
  },
  {
    label: 'Claude Agent',
    draft: {
      providerId: 'claude',
      displayName: 'Claude Agent',
      executable: 'npx',
      argsText: '[\n  "-y",\n  "@agentclientprotocol/claude-agent-acp@0.78.0"\n]',
      version: '0.78.0',
      permissionPolicy: 'prompt',
      enabled: true,
    },
  },
  {
    label: 'OpenCode',
    draft: {
      providerId: 'opencode',
      displayName: 'OpenCode',
      executable: 'opencode',
      argsText: '[\n  "acp"\n]',
      version: '1.18.30',
      permissionPolicy: 'prompt',
      enabled: true,
    },
  },
  {
    label: 'GitHub Copilot',
    draft: {
      providerId: 'github-copilot',
      displayName: 'GitHub Copilot ACP',
      executable: 'npx',
      argsText: '[\n  "-y",\n  "@github/copilot@1.0.83",\n  "--acp"\n]',
      version: '1.0.83',
      permissionPolicy: 'prompt',
      enabled: true,
    },
  },
];

export function AcpProfilesPanel({ snapshot, busyAction, onRefresh, onSave, onSetEnabled, onRemove }: AcpProfilesPanelProps) {
  const [draft, setDraft] = useState<ProfileDraft | null>(null);
  const [error, setError] = useState('');
  const profiles = snapshot?.profiles ?? [];
  const providers = snapshot?.providers ?? [];
  const readyCount = profiles.filter(profile => providers.some(provider => provider.provider === profile.provider_id && provider.status === 'ready')).length;

  useEffect(() => {
    setError('');
  }, [snapshot]);

  function editProfile(profile?: AcpRuntimeProfile) {
    setError('');
    setDraft(profile ? {
      providerId: profile.provider_id,
      displayName: profile.display_name,
      executable: profile.executable,
      argsText: JSON.stringify(profile.args, null, 2),
      version: profile.version,
      permissionPolicy: profile.permission_policy === 'allow_once'
        ? 'allow_once'
        : profile.permission_policy === 'prompt' ? 'prompt' : 'deny',
      enabled: profile.enabled,
    } : emptyDraft);
  }

  async function saveDraft() {
    if (!draft) return;
    let args: unknown;
    try {
      args = JSON.parse(draft.argsText || '[]');
    } catch {
      setError('参数必须是 JSON 字符串数组');
      return;
    }
    if (!Array.isArray(args) || args.some(value => typeof value !== 'string')) {
      setError('参数必须是 JSON 字符串数组');
      return;
    }
    if (!draft.providerId.trim() || !draft.executable.trim()) {
      setError('客户端 ID 和可执行文件不能为空');
      return;
    }
    setError('');
    try {
      await onSave({
        providerId: draft.providerId.trim(),
        displayName: draft.displayName.trim() || draft.providerId.trim(),
        executable: draft.executable.trim(),
        args,
        version: draft.version.trim(),
        permissionPolicy: draft.permissionPolicy,
        enabled: draft.enabled,
      });
      setDraft(null);
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : String(saveError));
    }
  }

  return (
    <div id="ai-panel-acp" role="tabpanel" aria-labelledby="ai-tab-acp">
      <section className="ai-overview ready">
        <div className="ai-overview-main">
          <div className="ai-overview-icon"><Bot size={20} /></div>
          <div className="ai-overview-copy">
            <span className="ai-overview-eyebrow">AI 客户端</span>
            <strong>{profiles.length ? `${readyCount}/${profiles.length} 个客户端就绪` : '尚未添加 AI 客户端'}</strong>
            <span>添加可由工作流调用的本地 AI 客户端。</span>
          </div>
        </div>
        <div className="ai-overview-stats" aria-label="AI 客户端统计">
          <div><span>已配置</span><strong>{profiles.length}</strong></div>
          <div><span>已就绪</span><strong>{readyCount}</strong></div>
          <div><span>已停用</span><strong>{profiles.filter(profile => !profile.enabled).length}</strong></div>
        </div>
        <div className="ai-overview-actions">
          <button className="btn btn-primary" onClick={() => editProfile()}><Plus size={15} />添加客户端</button>
          <button className="btn btn-icon" title="刷新 AI 客户端" aria-label="刷新 AI 客户端" disabled={Boolean(busyAction)} onClick={onRefresh}><RefreshCw size={16} /></button>
        </div>
      </section>

      {draft ? (
        <section className="acp-profile-form" aria-label="AI 客户端配置">
          <div className="ai-section-heading">
            <div><h3>{profiles.some(profile => profile.provider_id === (draft.providerId.startsWith('acp.') ? draft.providerId : `acp.${draft.providerId}`)) ? '编辑 AI 客户端' : '添加 AI 客户端'}</h3><span>配置启动命令、参数和权限策略。</span></div>
            <button className="btn btn-icon" title="关闭编辑器" aria-label="关闭编辑器" onClick={() => setDraft(null)}><X size={15} /></button>
          </div>
          <div className="acp-profile-grid">
            <label className="field-group"><span className="field-label">客户端 ID</span><input value={draft.providerId} placeholder="codex-acp" onChange={event => setDraft({ ...draft, providerId: event.target.value })} /></label>
            <label className="field-group"><span className="field-label">显示名称</span><input value={draft.displayName} placeholder="Codex ACP" onChange={event => setDraft({ ...draft, displayName: event.target.value })} /></label>
            <label className="field-group"><span className="field-label">可执行文件</span><input value={draft.executable} placeholder="codex-acp" onChange={event => setDraft({ ...draft, executable: event.target.value })} /></label>
            <label className="field-group"><span className="field-label">版本</span><input value={draft.version} placeholder="1.0.0" onChange={event => setDraft({ ...draft, version: event.target.value })} /></label>
            <label className="field-group"><span className="field-label">权限策略</span><select value={draft.permissionPolicy} onChange={event => setDraft({ ...draft, permissionPolicy: event.target.value === 'allow_once' ? 'allow_once' : event.target.value === 'prompt' ? 'prompt' : 'deny' })}><option value="deny">拒绝敏感操作</option><option value="allow_once">允许一次性授权</option><option value="prompt">询问本地审批</option></select></label>
            <label className="field-group acp-profile-enabled"><span className="field-label">状态</span><span><input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled: event.target.checked })} />启用</span></label>
            <label className="field-group acp-profile-args"><span className="field-label">参数 JSON</span><textarea value={draft.argsText} spellCheck={false} onChange={event => setDraft({ ...draft, argsText: event.target.value })} /></label>
          </div>
          {error ? <div className="blocker"><CircleAlert size={16} /><span>{error}</span></div> : null}
          <div className="acp-profile-form-actions"><button className="btn" onClick={() => setDraft(null)}>取消</button><button className="btn btn-primary" disabled={Boolean(busyAction)} onClick={saveDraft}><Save size={15} />保存</button></div>
        </section>
      ) : null}

      <section className="ai-client-section">
        <div className="ai-section-heading"><div><h3>推荐配置</h3><span>快速添加常用客户端。</span></div><Pill kind="neutral">{recommendedProfiles.length}</Pill></div>
        <div className="acp-preset-list">
          {recommendedProfiles.map(preset => (
            <button
              key={preset.draft.providerId}
              type="button"
              className="btn"
              onClick={() => {
                setError('');
                setDraft(preset.draft);
              }}
            >
              <Plus size={14} />{preset.label}
            </button>
          ))}
        </div>
      </section>

      <section className="ai-client-section">
        <div className="ai-section-heading"><div><h3>已配置客户端</h3><span>这些客户端可执行工作流中的 AI 步骤。</span></div><Pill kind="neutral">{profiles.length}</Pill></div>
        <div className="ai-client-list">
          {profiles.map(profile => {
            const provider = providers.find(candidate => candidate.provider === profile.provider_id);
            const ready = profile.enabled && provider?.status === 'ready';
            return <article className="ai-client-row acp-profile-row" key={profile.provider_id}>
              <div className="ai-client-icon code"><Bot size={18} /></div>
              <div className="ai-client-copy"><strong>{profile.display_name}</strong><span>{profile.version ? `v${profile.version}` : '本地客户端'}</span></div>
              <Pill kind={ready ? 'success' : profile.enabled ? 'warn' : 'neutral'}>{ready ? '已就绪' : profile.enabled ? provider?.status === 'unsupported' ? '配置异常' : '不可用' : '已停用'}</Pill>
              <div className="ai-client-registration-actions">
                <button className="btn btn-icon" title="编辑客户端" aria-label={`编辑 ${profile.display_name}`} onClick={() => editProfile(profile)}><Pencil size={15} /></button>
                <button className="btn btn-icon" title={profile.enabled ? '停用客户端' : '启用客户端'} aria-label={profile.enabled ? `停用 ${profile.display_name}` : `启用 ${profile.display_name}`} disabled={busyAction === `acp:${profile.provider_id}`} onClick={() => onSetEnabled(profile.provider_id, !profile.enabled)}><Power size={15} /></button>
                <button className="btn btn-icon ai-row-remove" title="删除客户端" aria-label={`删除 ${profile.display_name}`} disabled={busyAction === `acp:${profile.provider_id}`} onClick={() => { if (window.confirm(`确认删除 AI 客户端“${profile.display_name}”？`)) void onRemove(profile.provider_id); }}><Trash2 size={15} /></button>
              </div>
            </article>;
          })}
          {!profiles.length ? <div className="ai-empty-row"><span className="ai-empty-icon"><CircleCheck size={14} /></span><span>添加 AI 客户端后，即可在工作流中使用</span></div> : null}
        </div>
      </section>
    </div>
  );
}
