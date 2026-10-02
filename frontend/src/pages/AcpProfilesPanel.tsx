import { useEffect, useState } from 'react';
import { Bot, CircleAlert, CircleCheck, Code2, Cpu, Github, Pencil, Plus, Power, Save, Terminal, Trash2, X } from 'lucide-react';
import { Pill } from '../components/Common';
import { useConfirm } from '../components/ConfirmDialog';
import {
  acpPolicyOptions,
  acpPresets,
  acpProfileStatus,
  bareProviderId,
  formatCommand,
  normalizePolicy,
  presetCommand,
  presetVersion,
  resolveProviderId,
  splitCommandLine,
  validProviderId,
  type AcpPermissionPolicy,
  type AcpPreset,
} from './acpProfileView';
import { isAcpProvider, localRuntimeMeta, localRuntimeStatus, type LocalRuntimeIconName } from './runtimeProviderView';
import type { AcpRuntimeProfile, AcpRuntimeProfileInput, AcpRuntimeProfileSnapshot } from '../services/agentApi';

type AcpProfilesPanelProps = {
  snapshot: AcpRuntimeProfileSnapshot | null;
  busyAction: string | null;
  onSave: (input: AcpRuntimeProfileInput) => Promise<void>;
  onSetEnabled: (providerId: string, enabled: boolean) => Promise<void>;
  onRemove: (providerId: string) => Promise<void>;
};

type ProfileDraft = {
  mode: 'create' | 'edit';
  providerId: string;
  displayName: string;
  command: string;
  version: string;
  permissionPolicy: AcpPermissionPolicy;
  enabled: boolean;
};

// 本机执行后端是探测结果，不是可增删的登记。名字、状态、图标口径都来自
// runtimeProviderView：和工作流里「运行环境」那一栏显示的是同一批执行方。
// ACP 接入进来的客户端走下面的列表，两者拼起来才是完整的可选清单——原先只列 ACP，
// 用户在工作流里看到 Codex 时以为走错了地方。
function LocalRuntimeIcon({ icon }: { icon: LocalRuntimeIconName }) {
  if (icon === 'code') return <Code2 size={18} />;
  if (icon === 'github') return <Github size={18} />;
  return <Cpu size={18} />;
}

export function AcpProfilesPanel({ snapshot, busyAction, onSave, onSetEnabled, onRemove }: AcpProfilesPanelProps) {
  const confirm = useConfirm();
  const [draft, setDraft] = useState<ProfileDraft | null>(null);
  const [error, setError] = useState('');
  const [pending, setPending] = useState('');

  const profiles = snapshot?.profiles ?? [];
  const providers = snapshot?.providers ?? [];
  const executables = snapshot?.executables ?? {};
  const providerStatus = (providerId: string) => providers.find(candidate => candidate.provider === providerId);
  // acp.* 是上面那份登记的探测结果，跟着 profiles 一起显示；其余是本机自带的执行后端。
  const localProviders = providers.filter(candidate => !isAcpProvider(candidate.provider));
  const readyCount = localProviders.filter(candidate => candidate.status === 'ready').length
    + profiles.filter(profile => profile.enabled && providerStatus(profile.provider_id)?.status === 'ready').length;

  useEffect(() => {
    setError('');
  }, [snapshot]);

  function editProfile(profile?: AcpRuntimeProfile) {
    setError('');
    setDraft(profile ? {
      mode: 'edit',
      providerId: bareProviderId(profile.provider_id),
      displayName: profile.display_name,
      command: formatCommand(profile.executable, profile.args),
      version: profile.version,
      permissionPolicy: normalizePolicy(profile.permission_policy),
      enabled: profile.enabled,
    } : {
      mode: 'create',
      providerId: '',
      displayName: '',
      command: '',
      version: '',
      permissionPolicy: 'prompt',
      enabled: true,
    });
  }

  async function connectPreset(preset: AcpPreset, command: string) {
    const tokens = splitCommandLine(command);
    if (!tokens || !tokens.length) {
      setError('内置命令不可用，请改用自定义客户端。');
      return;
    }
    setError('');
    setPending(`preset:${preset.providerId}`);
    try {
      await onSave({
        providerId: preset.providerId,
        displayName: preset.name,
        executable: tokens[0],
        args: tokens.slice(1),
        version: presetVersion(preset, executables),
        permissionPolicy: preset.permissionPolicy,
        enabled: true,
      });
    } catch (connectError) {
      setError(connectError instanceof Error ? connectError.message : String(connectError));
    } finally {
      setPending('');
    }
  }

  async function runRowAction(providerId: string, action: () => Promise<void>) {
    setPending(`row:${providerId}`);
    try {
      await action();
    } finally {
      setPending('');
    }
  }

  async function saveDraft() {
    if (!draft) return;
    if (!draft.command.trim()) {
      setError('请填写启动命令。');
      return;
    }
    const tokens = splitCommandLine(draft.command);
    if (!tokens || !tokens.length) {
      setError('启动命令格式不对，请检查双引号是否配对。');
      return;
    }
    const providerId = resolveProviderId({
      mode: draft.mode,
      typedId: draft.providerId,
      displayName: draft.displayName,
      command: draft.command,
    });
    if (!providerId || !validProviderId(providerId)) {
      setError('客户端 ID 只能是字母、数字、点、下划线和横线，且不超过 64 个字符。');
      return;
    }
    if (draft.mode === 'create' && profiles.some(profile => profile.provider_id === `acp.${providerId}`)) {
      setError(`已存在客户端 acp.${providerId}，请在高级设置里换一个 ID。`);
      return;
    }
    setError('');
    try {
      await onSave({
        providerId,
        displayName: draft.displayName.trim() || providerId,
        executable: tokens[0],
        args: tokens.slice(1),
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
            <span className="ai-overview-eyebrow">运行环境</span>
            <strong>{readyCount ? `${readyCount} 个可用` : '还没有可用的运行环境'}</strong>
            <span>工作流的 AI 步骤在这里挑执行方。</span>
          </div>
        </div>
        <div className="ai-overview-stats" aria-label="运行环境统计">
          <div><span>本机</span><strong>{localProviders.length}</strong></div>
          <div><span>已接入</span><strong>{profiles.length}</strong></div>
          <div><span>已就绪</span><strong>{readyCount}</strong></div>
        </div>
        <div className="ai-overview-actions">
          <button className="btn" disabled={Boolean(busyAction) || Boolean(pending)} onClick={() => editProfile()}><Plus size={15} />自定义客户端</button>
        </div>
      </section>

      {error && !draft ? <div className="blocker"><CircleAlert size={16} /><span>{error}</span></div> : null}

      {localProviders.length ? (
        <section className="ai-client-section">
          <div className="ai-section-heading"><div><h3>本机已安装</h3><span>自动探测，未安装的不能选为运行环境。</span></div><Pill kind="neutral">{localProviders.length}</Pill></div>
          <div className="ai-client-list">
            {localProviders.map(provider => {
              const meta = localRuntimeMeta(provider.provider);
              const status = localRuntimeStatus(provider.status);
              return <article className="ai-client-row" key={provider.provider}>
                <div className={`ai-client-icon ${meta.icon}`}><LocalRuntimeIcon icon={meta.icon} /></div>
                <div className="ai-client-copy">
                  <strong>{meta.name}</strong>
                  <span title={provider.version || undefined}>{provider.version ? `${meta.detail} · ${provider.version}` : meta.detail}</span>
                </div>
                <Pill kind={status.kind}>{status.label}</Pill>
              </article>;
            })}
          </div>
        </section>
      ) : null}

      {draft ? (
        <section className="acp-profile-form" aria-label="客户端配置">
          <div className="ai-section-heading">
            <div>
              <h3>{draft.mode === 'edit' ? `编辑 ${draft.displayName || '客户端'}` : '自定义客户端'}</h3>
              <span>填好启动命令即可，其余可留空。</span>
            </div>
            <button className="btn btn-icon" title="关闭编辑器" aria-label="关闭编辑器" onClick={() => setDraft(null)}><X size={15} /></button>
          </div>
          <div className="acp-profile-grid">
            <label className="field-group">
              <span className="field-label">名称</span>
              <input value={draft.displayName} placeholder="例如 我的 Codex" onChange={event => setDraft({ ...draft, displayName: event.target.value })} />
            </label>
            <label className="field-group">
              <span className="field-label">版本（可选）</span>
              <input value={draft.version} placeholder="1.0.0" onChange={event => setDraft({ ...draft, version: event.target.value })} />
            </label>
            <label className="field-group acp-profile-wide">
              <span className="field-label">启动命令</span>
              <input className="acp-command-input" value={draft.command} spellCheck={false} placeholder="npx -y @agentclientprotocol/codex-acp@1.12.0" onChange={event => setDraft({ ...draft, command: event.target.value })} />
              <span className="field-hint">和终端里一样，按空格切分；路径含空格时用双引号包起来。</span>
            </label>
            <div className="field-group acp-profile-wide">
              <span className="field-label">权限策略</span>
              <div className="acp-policy-options">
                {acpPolicyOptions.map(option => (
                  <label className={`acp-policy-option${draft.permissionPolicy === option.value ? ' active' : ''}`} key={option.value}>
                    <input
                      type="radio"
                      name="acp-permission-policy"
                      checked={draft.permissionPolicy === option.value}
                      onChange={() => setDraft({ ...draft, permissionPolicy: option.value })}
                    />
                    <span><strong>{option.label}</strong><small>{option.description}</small></span>
                  </label>
                ))}
              </div>
            </div>
            <details className="acp-profile-advanced acp-profile-wide">
              <summary>高级设置（客户端 ID、启用状态）</summary>
              <div className="acp-profile-grid">
                <label className="field-group">
                  <span className="field-label">客户端 ID</span>
                  <input value={draft.providerId} placeholder="留空则按名称生成" onChange={event => setDraft({ ...draft, providerId: event.target.value })} />
                  <span className="field-hint">工作流按 acp.&lt;ID&gt; 引用，留空则按名称生成。</span>
                </label>
                <label className="field-group acp-profile-enabled">
                  <span className="field-label">状态</span>
                  <span><input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled: event.target.checked })} />接入后立即启用</span>
                </label>
              </div>
            </details>
          </div>
          {error ? <div className="blocker"><CircleAlert size={16} /><span>{error}</span></div> : null}
          <div className="acp-profile-form-actions">
            <button className="btn" onClick={() => setDraft(null)}>取消</button>
            <button className="btn btn-primary" disabled={Boolean(busyAction)} onClick={saveDraft}><Save size={15} />保存</button>
          </div>
        </section>
      ) : null}

      <section className="ai-client-section">
        <div className="ai-section-heading"><div><h3>可接入的运行环境</h3><span>命令已内置，点一下即可接入。</span></div><Pill kind="neutral">{acpPresets.length}</Pill></div>
        <div className="acp-client-cards">
          {acpPresets.map(preset => {
            const installed = profiles.find(profile => profile.provider_id === `acp.${preset.providerId}`);
            const requirement = preset.requires ? executables[preset.requires] : undefined;
            const missing = Boolean(preset.requires && requirement && requirement.available === false);
            // 装了桌面版 OpenCode 却不带 CLI 进 PATH 是常态：命令名解析不到、但本机有
            // 已知安装位置时，用探测到的绝对路径接入，别把用户堵在「未安装」上。
            const command = presetCommand(preset, executables);
            const foundOnDisk = command !== preset.command;
            const actionBusy = pending === `preset:${preset.providerId}`;
            return (
              <article className="acp-client-card" key={preset.providerId}>
                <div className="acp-client-card-head">
                  <strong>{preset.name}</strong>
                  {installed ? <Pill kind="success">已接入</Pill> : null}
                </div>
                <p className="acp-client-card-summary">{preset.summary}</p>
                <code className="acp-client-card-command" title={command}>{command}</code>
                {!installed && foundOnDisk ? <span className="acp-client-card-note">已检测到本机安装，接入后使用该路径</span> : null}
                <div className="acp-client-card-actions">
                  {installed ? (
                    <button className="btn" onClick={() => editProfile(installed)}><Pencil size={14} />编辑</button>
                  ) : missing ? (
                    <span className="acp-client-card-hint">
                      {requirement?.config_dir
                        ? `检测到 ${preset.requiresLabel} 已安装，但命令不在 PATH`
                        : `未找到 ${preset.requiresLabel} 命令`}
                    </span>
                  ) : (
                    <button className="btn btn-primary" disabled={actionBusy} onClick={() => void connectPreset(preset, command)}><Plus size={14} />接入</button>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      </section>

      <section className="ai-client-section">
        <div className="ai-section-heading"><div><h3>已接入的运行环境</h3><span>工作流的 AI 步骤可以选它们执行。</span></div><Pill kind="neutral">{profiles.length}</Pill></div>
        <div className="ai-client-list">
          {profiles.map(profile => {
            const status = acpProfileStatus(profile, providerStatus(profile.provider_id));
            const command = formatCommand(profile.executable, profile.args);
            const rowBusy = pending === `row:${profile.provider_id}`;
            return <article className="ai-client-row acp-profile-row" key={profile.provider_id}>
              <div className="ai-client-icon code"><Bot size={18} /></div>
              <div className="ai-client-copy">
                <strong>{profile.display_name}</strong>
                <span className="acp-profile-command" title={command}><Terminal size={12} /><span>{command}</span></span>
                {status.reason ? <small className="acp-profile-reason">{status.reason}</small> : null}
              </div>
              <Pill kind={status.tone}>{status.label}</Pill>
              <div className="ai-client-registration-actions">
                <button className="btn" aria-label={`编辑 ${profile.display_name}`} disabled={rowBusy} onClick={() => editProfile(profile)}><Pencil size={15} />编辑</button>
                <button className="btn" aria-label={profile.enabled ? `停用 ${profile.display_name}` : `启用 ${profile.display_name}`} disabled={rowBusy} onClick={() => void runRowAction(profile.provider_id, () => onSetEnabled(profile.provider_id, !profile.enabled))}><Power size={15} />{profile.enabled ? '停用' : '启用'}</button>
              <button className="btn ai-row-remove" aria-label={`删除 ${profile.display_name}`} disabled={rowBusy} onClick={() => { void confirm({ title: `删除客户端「${profile.display_name}」？`, description: '移除后，工作流不能再选它执行。', confirmText: '删除' }).then(accepted => { if (accepted) void runRowAction(profile.provider_id, () => onRemove(profile.provider_id)); }); }}><Trash2 size={15} />删除</button>
              </div>
            </article>;
          })}
          {!profiles.length ? <div className="ai-empty-row"><span className="ai-empty-icon"><CircleCheck size={14} /></span><span>接入运行环境后，工作流的 AI 步骤就能选它执行</span></div> : null}
        </div>
      </section>
    </div>
  );
}
