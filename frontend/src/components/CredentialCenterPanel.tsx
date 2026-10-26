import { useEffect, useMemo, useRef, useState } from 'react';
import { CheckCircle2, CircleAlert, Copy, FileKey2, KeyRound, RefreshCw, Search, ShieldCheck, Trash2, X } from 'lucide-react';
import { agentApi, type CredentialCatalogItem, type CredentialCatalogSnapshot } from '../services/agentApi';
import { BusyIndicator } from './BusyIndicator';
import { IconButton, Pill } from './Common';
import { CredentialFieldsEditor } from './CredentialFieldsEditor';
import { copyText } from '../lib/clipboard';
import { useModalFocus } from './useModalFocus';

type Filter = 'all' | 'issues' | 'core' | 'builtin' | 'plugin' | 'workflow';

const FILTERS: Array<{ id: Filter; label: string }> = [
  { id: 'all', label: '全部' },
  { id: 'issues', label: '需处理' },
  { id: 'core', label: '系统服务' },
  { id: 'builtin', label: '内置服务' },
  { id: 'plugin', label: '插件' },
  { id: 'workflow', label: '工作流' },
];

const ATTENTION_STATUSES = new Set(['missing', 'invalid', 'unavailable', 'needs_review', 'unverified']);
const SEVERITY: Record<string, number> = {
  invalid: 0,
  missing: 1,
  needs_review: 2,
  unavailable: 3,
  unverified: 4,
  valid: 5,
  optional: 6,
  disabled: 7,
};

function statusView(item: CredentialCatalogItem): { label: string; kind: 'success' | 'warn' | 'danger' | 'neutral' } {
  switch (item.status) {
    case 'valid': return { label: '可用', kind: 'success' };
    case 'unverified': return { label: '已配置 · 待验证', kind: 'warn' };
    case 'missing': return { label: '未配置', kind: item.required ? 'warn' : 'neutral' };
    case 'invalid': return { label: '凭据失效', kind: 'danger' };
    case 'unavailable': return { label: '不可用', kind: 'danger' };
    case 'disabled': return { label: '已停用', kind: 'neutral' };
    case 'needs_review': return { label: '声明已更新 · 需确认', kind: 'warn' };
    case 'optional': return { label: '未配置 · 可选', kind: 'neutral' };
    default: return { label: item.configured ? '已配置' : '未配置', kind: 'neutral' };
  }
}

function timeLabel(value: string) {
  if (!value.trim()) return '尚未使用';
  const date = new Date(/^\d+$/.test(value) ? Number(value) : value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false });
}

function ownerLabel(kind: string) {
  if (kind === 'core') return '系统';
  if (kind === 'builtin') return '内置服务';
  if (kind === 'plugin') return '插件';
  if (kind === 'workflow') return '工作流';
  return kind;
}

function warningSummary(warning: string) {
  const separator = warning.indexOf('：');
  return separator > 0 ? warning.slice(0, separator) : warning;
}

function isReadableError(message: string) {
  const lower = message.toLowerCase();
  const technicalMarkers = ['os error', 'errno', 'error:', 'failed to', 'not found', 'permission denied', 'access is denied', 'timeout', 'http'];
  const looksLikePath = /[a-z]:\\|\\\\|\/(?:users|home|var|tmp)\//i.test(message);
  const looksLikeStatusCode = /\b[45]\d{2}\b/.test(message);
  return /[\u4e00-\u9fff]/.test(message) && message.length <= 48 && !technicalMarkers.some(marker => lower.includes(marker)) && !looksLikePath && !looksLikeStatusCode;
}

function credentialErrorSummary(message: string, fallback: string) {
  return isReadableError(message) ? message : fallback;
}

function DiagnosticDetails({
  detail,
  summary = '查看技术详情',
  format = 'text',
}: {
  detail: string;
  summary?: string;
  format?: 'text' | 'pre';
}) {
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const copy = async () => {
    const copied = await copyText(detail);
    setCopyState(copied ? 'copied' : 'failed');
  };
  return <details className="credential-diagnostic-details">
    <summary>{summary}</summary>
    {format === 'pre' ? <pre>{detail}</pre> : <p>{detail}</p>}
    <div className="credential-diagnostic-actions">
      <button type="button" className="btn btn-quiet" onClick={() => void copy()}>{copyState === 'copied' ? <CheckCircle2 size={14} /> : <Copy size={14} />}{copyState === 'copied' ? '已复制' : '复制诊断信息'}</button>
      {copyState === 'failed' ? <span role="status">复制失败，请手动选择详情。</span> : null}
    </div>
  </details>;
}

export function CredentialCenterPanel({
  onOpenAiServices,
  onOpenInnerAccount,
  onOpenSvnAccount,
  onTestSvn,
  onRemoveSvn,
  svnBusy = false,
}: {
  onOpenAiServices: () => void;
  onOpenInnerAccount: () => void;
  onOpenSvnAccount: () => void;
  onTestSvn: () => void;
  onRemoveSvn: () => void;
  svnBusy?: boolean;
}) {
  const [snapshot, setSnapshot] = useState<CredentialCatalogSnapshot | null>(null);
  const [filter, setFilter] = useState<Filter>('all');
  const [query, setQuery] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [editing, setEditing] = useState<CredentialCatalogItem | null>(null);

  const load = () => {
    setLoading(true);
    setError('');
    return agentApi.credentialCatalog()
      .then(setSnapshot)
      .catch(reason => setError(reason instanceof Error ? reason.message : String(reason)))
      .finally(() => setLoading(false));
  };

  useEffect(() => { void load(); }, []);

  const isAttention = (item: CredentialCatalogItem) =>
    (item.required || item.configured) && ATTENTION_STATUSES.has(item.status);

  const items = useMemo(() => {
    const all = snapshot?.items || [];
    const normalizedQuery = query.trim().toLowerCase();
    const byFilter = filter === 'all'
      ? all
      : filter === 'issues'
        ? all.filter(isAttention)
        : all.filter(item => item.owner_kind === filter);
    const searched = normalizedQuery
      ? byFilter.filter(item => [item.label, item.owner_name, item.description, ...Object.values(item.public_fields)]
          .join(' ')
          .toLowerCase()
          .includes(normalizedQuery))
      : byFilter;
    return [...searched].sort((left, right) => (SEVERITY[left.status] ?? 99) - (SEVERITY[right.status] ?? 99));
  }, [snapshot, filter, query]);

  const issueCount = (snapshot?.items || []).filter(isAttention).length;
  const configuredCount = (snapshot?.items || []).filter(item => item.configured).length;

  const manage = (item: CredentialCatalogItem) => {
    if (item.manage_action === 'ai-services') onOpenAiServices();
    else if (item.manage_action === 'inner-account') onOpenInnerAccount();
    else if (item.manage_action === 'svn-account') onOpenSvnAccount();
    else if (item.manage_action === 'github-account') {
      const details = document.getElementById('github-account-card') as HTMLDetailsElement | null;
      if (details) {
        details.open = true;
        details.scrollIntoView({ behavior: 'smooth', block: 'start' });
        (details.querySelector('summary') as HTMLElement | null)?.focus({ preventScroll: true });
      }
    } else if (item.manage_action === 'workbench') {
      const target = document.getElementById('workbench-connections');
      target?.scrollIntoView({ behavior: 'smooth', block: 'start' });
      (target?.querySelector('.card-header h2') as HTMLElement | null)?.focus({ preventScroll: true });
    } else if (item.editable) setEditing(item);
  };

  const actionLabel = (item: CredentialCatalogItem) => {
    if (item.manage_action === 'ai-services') return '打开 AI 服务';
    if (item.status === 'needs_review') return '核对并更新';
    if (item.status === 'missing' || !item.configured) return item.editable ? '添加凭据' : '去配置';
    if (item.manage_action === 'github-account') return '管理 GitHub 账号';
    if (item.manage_action === 'workbench') return '管理工作台账号';
    return item.editable ? '更新凭据' : '管理';
  };

  return <section className="card settings-section credential-center" aria-labelledby="credential-center-title">
    <div className="card-header">
      <h2 id="credential-center-title">账号与凭据</h2>
      <Pill kind={error ? 'danger' : issueCount ? 'warn' : snapshot ? 'success' : 'neutral'}>
        {error ? '读取失败' : loading ? '读取中' : issueCount ? `${issueCount} 项需处理` : `${configuredCount} 项已配置`}
      </Pill>
    </div>
    <div className="credential-center-body">
      <div className="credential-center-toolbar">
        <label className="credential-center-search">
          <Search size={15} aria-hidden="true" />
          <input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索账号、服务、插件或工作流" aria-label="搜索账号与凭据" />
        </label>
        <button type="button" className="btn btn-icon" aria-label="刷新账号与凭据" title="刷新账号与凭据" disabled={loading} onClick={() => void load()}>
          {loading ? <BusyIndicator size={15} /> : <RefreshCw size={15} />}
        </button>
      </div>

      <div className="credential-center-filters" role="group" aria-label="筛选凭据">
        {FILTERS.map(option => <button key={option.id} type="button" aria-pressed={filter === option.id} className={filter === option.id ? 'active' : ''} onClick={() => setFilter(option.id)}>{option.label}</button>)}
      </div>

      {error ? <div className="blocker account-blocker credential-load-error" role="alert">
        <CircleAlert size={17} />
        <div>
          <strong>暂时无法读取账号与凭据</strong>
          <span>请重新读取；若仍然失败，可展开技术详情复制诊断信息。</span>
          <DiagnosticDetails detail={error} summary="查看错误详情" format="pre" />
        </div>
        <button className="btn" onClick={() => void load()}>重新读取</button>
      </div> : null}
      {snapshot?.warnings.length ? <div className="credential-center-warnings" role="status" aria-label="部分账号状态读取失败">
        {snapshot.warnings.map(warning => <details key={warning} className="credential-warning-details">
          <summary><CircleAlert size={14} /><span>{warningSummary(warning)}</span></summary>
          <div className="credential-warning-detail-body">
            <span>{warning}</span>
            <DiagnosticDetails detail={warning} summary="复制技术详情" format="pre" />
          </div>
        </details>)}
      </div> : null}

      {!loading && !error && !items.length ? <div className="workbench-connection-empty"><FileKey2 size={18} /><div><strong>{query || filter !== 'all' ? '没有匹配的账号或凭据' : '没有需要管理的凭据'}</strong><span>{query || filter !== 'all' ? '换一个关键词或筛选条件试试。' : '安装声明凭据的插件或工作流后会出现在这里。'}</span></div></div> : null}

      <div className="credential-catalog-list">
        {items.map(item => {
          const state = statusView(item);
          return <article className={`credential-catalog-row owner-${item.owner_kind}`} key={item.id}>
            <div className="credential-catalog-icon">{item.owner_kind === 'core' ? <ShieldCheck size={17} /> : <KeyRound size={17} />}</div>
            <div className="credential-catalog-main">
              <div className="credential-catalog-title">
                <strong>{item.label}</strong>
                <Pill kind={state.kind}>{state.label}</Pill>
              </div>
              <div className="credential-catalog-owner">{ownerLabel(item.owner_kind)} · {item.owner_name}</div>
              <div className="credential-catalog-meta">
                {item.fields.filter(field => !field.secret && field.value.trim()).slice(0, 2).map(field => <span key={field.key}>{field.label}：{field.value}</span>)}
                {item.last_used_at.trim() ? <span>最近使用：{timeLabel(item.last_used_at)}</span> : null}
              </div>
              {item.description ? <small>{item.description}</small> : null}
            </div>
            <div className="credential-catalog-actions">
              {item.manage_action === 'svn-account' && item.configured ? <button type="button" className="btn" disabled={svnBusy} onClick={onTestSvn}>{svnBusy ? <BusyIndicator size={14} /> : null}测试</button> : null}
              {item.manage_action === 'svn-account' && item.configured ? <button type="button" className="btn btn-danger-quiet" disabled={svnBusy} onClick={onRemoveSvn}><Trash2 size={14} />清除</button> : null}
              <button type="button" className="btn" aria-label={`${actionLabel(item)}：${item.label}`} onClick={() => manage(item)}>{actionLabel(item)}</button>
            </div>
          </article>;
        })}
      </div>

      <div className="security-note compact"><ShieldCheck size={16} /><span>敏感凭据仅加密保存在本机，密码、令牌和密钥不会显示原文。</span></div>
    </div>
    {editing ? <CredentialEditDialog item={editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void load(); }} /> : null}
  </section>;
}

function CredentialEditDialog({ item, onClose, onSaved }: { item: CredentialCatalogItem; onClose: () => void; onSaved: () => void }) {
  const initial = Object.fromEntries(item.fields.filter(field => !field.secret).map(field => [field.key, field.value]));
  const [drafts, setDrafts] = useState<Record<string, string>>(initial);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const dialogRef = useRef<HTMLDivElement>(null);
  const initialFocusRef = useRef<HTMLInputElement>(null);
  useModalFocus(dialogRef, true, onClose, initialFocusRef);

  const save = async () => {
    setBusy(true);
    setError('');
    try {
      if (item.owner_kind === 'plugin') {
        const publicFields: Record<string, string> = {};
        const secretFields: Record<string, string> = {};
        for (const field of item.fields) {
          const value = (drafts[field.key] ?? (field.secret ? '' : field.value)).trim();
          if (!value) continue;
          if (field.secret) secretFields[field.key] = value;
          else publicFields[field.key] = value;
        }
        await agentApi.savePluginCredentialProfile(item.owner_id, item.label === item.id ? item.id : item.id.split(':').slice(1).join(':'), publicFields, secretFields);
      } else if (item.owner_kind === 'workflow') {
        const connectorId = item.public_fields.connector || '';
        const handle = item.id.split(':').slice(1).join(':');
        const target = item.public_fields.target || item.fields[0]?.key || '';
        if (item.kind === 'file_path') {
          await agentApi.saveConnectorFileCredential(connectorId, handle);
        } else {
          const secret = (drafts[target] || '').trim();
          if (!secret) throw new Error('请输入新密钥');
          await agentApi.saveConnectorSecretCredential(connectorId, handle, secret);
        }
      }
      onSaved();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    setError('');
    try {
      if (item.owner_kind === 'plugin') {
        await agentApi.deletePluginCredential(item.owner_id, item.id.split(':').slice(1).join(':'));
      } else if (item.owner_kind === 'workflow') {
        await agentApi.removeConnectorCredential(item.id.split(':').slice(1).join(':'));
      }
      onSaved();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  return <div className="modal-backdrop" onClick={onClose} role="presentation">
    <div ref={dialogRef} className="modal credential-center-modal" role="dialog" aria-modal="true" aria-labelledby="credential-edit-title" tabIndex={-1} onClick={event => event.stopPropagation()}>
      <div className="modal-header">
        <div><h3 id="credential-edit-title">{item.configured ? '更新' : '添加'}{item.label}</h3><p>{item.owner_name} · {item.kind}</p></div>
        <IconButton icon={X} label="关闭" onClick={onClose} />
      </div>
      <div className="modal-body">
        {item.description ? <p className="field-hint">{item.description}</p> : null}
        {!item.descriptor_current ? <div className="inline-feedback visible" role="alert">插件更新了凭据声明，请核对字段后重新保存。</div> : null}
        <CredentialFieldsEditor fields={item.fields} drafts={drafts} disabled={busy} firstInputRef={initialFocusRef} onChange={(key, value) => setDrafts(current => ({ ...current, [key]: value }))} />
        {error ? <div className="inline-feedback visible credential-center-error" role="alert">
          <span>{credentialErrorSummary(error, '未能保存这次更改，请检查内容后重试。')}</span>
          <DiagnosticDetails detail={error} summary="查看错误详情" format="pre" />
        </div> : null}
        <div className="modal-actions">
          <div>{item.configured ? <button type="button" className="btn btn-danger-quiet" disabled={busy} onClick={() => void remove()}><Trash2 size={14} />移除凭据</button> : null}</div>
          <div className="actions-row">
            <button type="button" className="btn" disabled={busy} onClick={onClose}>取消</button>
            <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void save()}>{busy ? <BusyIndicator size={14} /> : <CheckCircle2 size={14} />}{item.kind === 'file_path' ? '选择文件' : '保存'}</button>
          </div>
        </div>
      </div>
    </div>
  </div>;
}
