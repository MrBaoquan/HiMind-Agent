import { useState } from 'react';
import { Cloud, MoreHorizontal, PencilLine, Plus, RefreshCw, Server, ShieldAlert, ShieldCheck, Trash2, X } from 'lucide-react';
import type { DashboardAuthorizationProgress, DashboardIdentityStatus, WorkbenchConnection, WorkbenchConnectionsSnapshot, WorkbenchProbe } from '../services/agentApi';
import { ActionMenu, ActionMenuItem } from './ActionMenu';
import { BusyIndicator } from './BusyIndicator';
import { IconButton, Pill } from './Common';

export type WorkbenchConnectionDraft = {
  displayName: string;
  apiBase: string;
  purpose: string;
  enrollmentToken: string;
};

const PURPOSE_PRESETS = ['生产环境', '开发环境'];

/** 连接状态只说事实：本机有没有这台工作台的 Agent 身份、有没有登录账号。 */
function connectionState(connection: WorkbenchConnection): { label: string; kind: 'success' | 'warn' | 'neutral' } {
  if (connection.authorized) return { label: '已授权', kind: 'success' };
  if (connection.registered) return { label: '待授权', kind: 'warn' };
  return { label: '未登记', kind: 'neutral' };
}

function failureText(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error.trim()) return error.trim();
  return fallback;
}

type WorkbenchConnectionsPanelProps = {
  snapshot: WorkbenchConnectionsSnapshot | null;
  /** 读取连接清单失败的原因：有值时显示错误态，而不是一直停在「读取中」。 */
  error?: string;
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  /** 正在进行中的操作标识：`identity:<id>` / `switch:<id>` / `enroll:<id>` / `remove:<id>` / `rename:<id>` */
  busyId: string;
  onRefresh: () => void;
  onAdd: (draft: WorkbenchConnectionDraft) => Promise<void>;
  onRename: (id: string, displayName: string, purpose: string) => Promise<void>;
  onRemove: (connection: WorkbenchConnection) => void;
  onSwitch: (connection: WorkbenchConnection) => void;
  onEnroll: (id: string, enrollmentToken: string) => Promise<void>;
  onProbe: (apiBase: string) => Promise<WorkbenchProbe>;
  onAuthorize: (connection: WorkbenchConnection) => void;
  onRevoke: () => void;
};

/**
 * HiMind 账号 = 本机已登记的多个工作台连接。
 *
 * 一台工作台一条记录，各自带自己的身份；同一时刻只有一条在工作。
 * 这里的动作顺序刻意跟着「登记 → 探活 → 授权」走：任何一步没成，用户都能
 * 在行内看到卡在哪，不需要去别处找状态。
 */
export function WorkbenchConnectionsPanel({
  snapshot,
  error = '',
  identity,
  authorization,
  busyId,
  onRefresh,
  onAdd,
  onRename,
  onRemove,
  onSwitch,
  onEnroll,
  onProbe,
  onAuthorize,
  onRevoke,
}: WorkbenchConnectionsPanelProps) {
  const [addOpen, setAddOpen] = useState(false);
  const [renameTarget, setRenameTarget] = useState<WorkbenchConnection | null>(null);
  const [enrollTarget, setEnrollTarget] = useState<WorkbenchConnection | null>(null);

  const connections = snapshot?.connections || [];
  const active = connections.find(connection => connection.active) || null;
  const flowActive = authorization?.state === 'starting' || authorization?.state === 'pending';
  // 三态分开：读取失败是错误，不是加载中，也不等于「还没连接工作台」。
  const headerState = error
    ? { label: '读取失败', kind: 'danger' as const }
    : active
      ? connectionState(active)
      : { label: snapshot ? '未连接' : '读取中', kind: 'neutral' as const };

  return (
    <section className="card settings-section workbench-connections" id="workbench-connections">
      <div className="card-header">
        <span>HiMind 账号</span>
        <Pill kind={headerState.kind}>{headerState.label}</Pill>
      </div>
      <div className="credential-section">
        <div className="credential-heading">
          <span>工作台连接</span>
          <span className="credential-heading-note">本机能力只有一份，账号按工作台各存一份</span>
        </div>
        {error ? (
          // 读不出来时给出可执行的下一步，而不是让「读取中」一直转下去。
          <div className="blocker account-blocker" role="alert">
            <ShieldAlert size={18} />
            <div><strong>工作台连接读取失败</strong><span>{error}</span></div>
            <button type="button" className="btn" onClick={onRefresh}><RefreshCw size={15} />重新读取</button>
          </div>
        ) : connections.length ? (
          <div className="workbench-connection-list">
            {connections.map(connection => (
              <ConnectionRow
                key={connection.id}
                connection={connection}
                busyId={busyId}
                flowActive={flowActive}
                authorizeDisabled={identity?.state === 'disabled'}
                onSwitch={onSwitch}
                onAuthorize={onAuthorize}
                onRevoke={onRevoke}
                onEnroll={() => setEnrollTarget(connection)}
                onRename={() => setRenameTarget(connection)}
                onRemove={() => onRemove(connection)}
              />
            ))}
          </div>
        ) : (
          <div className="workbench-connection-empty">
            <Cloud size={18} />
            <div>
              <strong>{snapshot ? '还没有连接工作台' : '正在读取工作台连接'}</strong>
              <span>不连接工作台也能正常使用本机 AI、技能、插件和工作流。</span>
            </div>
          </div>
        )}
        {error ? null : (
          <div className="workbench-connection-footer">
            <button type="button" className="btn" onClick={() => setAddOpen(true)}><Plus size={15} />连接工作台</button>
          </div>
        )}
        <div className="security-note compact">
          <ShieldCheck size={16} />
          <span>连接后接收该工作台派发的任务并同步运行记录；切换不影响本机能力与凭据。</span>
        </div>
      </div>
      {addOpen ? <AddConnectionDialog onClose={() => setAddOpen(false)} onProbe={onProbe} onSubmit={onAdd} /> : null}
      {renameTarget ? <RenameConnectionDialog connection={renameTarget} onClose={() => setRenameTarget(null)} onSubmit={onRename} /> : null}
      {enrollTarget ? <EnrollConnectionDialog connection={enrollTarget} onClose={() => setEnrollTarget(null)} onSubmit={onEnroll} /> : null}
    </section>
  );
}

function ConnectionRow({
  connection,
  busyId,
  flowActive,
  authorizeDisabled,
  onSwitch,
  onAuthorize,
  onRevoke,
  onEnroll,
  onRename,
  onRemove,
}: {
  connection: WorkbenchConnection;
  busyId: string;
  flowActive: boolean;
  authorizeDisabled: boolean;
  onSwitch: (connection: WorkbenchConnection) => void;
  onAuthorize: (connection: WorkbenchConnection) => void;
  onRevoke: () => void;
  onEnroll: () => void;
  onRename: () => void;
  onRemove: () => void;
}) {
  const state = connectionState(connection);
  const rowBusy = busyId.endsWith(`:${connection.id}`);
  const disabled = Boolean(busyId);
  const detail = connection.authorized && connection.user_name
    ? `${connection.user_name} · ${connection.api_base}`
    : connection.api_base;

  return (
    <div className={`workbench-connection${connection.active ? ' active' : ''}`}>
      <div className="account-icon"><Server size={17} /></div>
      <div className="workbench-connection-main">
        <div className="workbench-connection-title">
          <strong>{connection.display_name || connection.api_base}</strong>
          {connection.active ? <span className="workbench-connection-active">当前</span> : null}
          {connection.purpose ? <span className="workbench-connection-purpose">{connection.purpose}</span> : null}
        </div>
        <small title={detail}>{detail}</small>
      </div>
      <Pill kind={state.kind}>{state.label}</Pill>
      <div className="workbench-connection-actions">
        {connection.active ? (
          connection.authorized
            ? <button type="button" className="btn btn-danger-quiet" disabled={disabled || flowActive} onClick={onRevoke}>{rowBusy || flowActive ? <BusyIndicator size={15} /> : null}取消授权</button>
            : connection.registered
              ? <button type="button" className="btn btn-primary" disabled={disabled || flowActive || authorizeDisabled} onClick={() => onAuthorize(connection)}>{rowBusy ? <BusyIndicator size={15} /> : null}授权</button>
              : <button type="button" className="btn btn-primary" disabled={disabled} onClick={onEnroll}>登记</button>
        ) : connection.registered ? (
          <button type="button" className="btn" disabled={disabled} onClick={() => onSwitch(connection)}>{rowBusy ? <BusyIndicator size={15} /> : null}切换</button>
        ) : (
          <button type="button" className="btn" disabled={disabled} onClick={onEnroll}>登记</button>
        )}
        <ActionMenu icon={<MoreHorizontal size={16} />} title={`${connection.display_name || connection.api_base} 的更多操作`} disabled={disabled}>
          {close => (
            <>
              <ActionMenuItem icon={<PencilLine size={15} />} label="重命名" onClick={() => { close(); onRename(); }} />
              <ActionMenuItem
                icon={<Trash2 size={15} />}
                label="移除"
                danger
                disabled={connection.active}
                title={connection.active ? '当前使用中的工作台不能移除' : undefined}
                onClick={() => { close(); onRemove(); }}
              />
            </>
          )}
        </ActionMenu>
      </div>
    </div>
  );
}

function AddConnectionDialog({
  onClose,
  onProbe,
  onSubmit,
}: {
  onClose: () => void;
  onProbe: (apiBase: string) => Promise<WorkbenchProbe>;
  onSubmit: (draft: WorkbenchConnectionDraft) => Promise<void>;
}) {
  const [apiBase, setApiBase] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [purpose, setPurpose] = useState(PURPOSE_PRESETS[0]);
  const [customPurpose, setCustomPurpose] = useState('');
  const [enrollmentToken, setEnrollmentToken] = useState('');
  const [probe, setProbe] = useState<WorkbenchProbe | null>(null);
  const [probing, setProbing] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');

  const address = apiBase.trim();
  const resolvedPurpose = purpose === '自定义' ? customPurpose.trim() : purpose;

  const test = async () => {
    if (!address) return;
    setProbing(true);
    setError('');
    try {
      setProbe(await onProbe(address));
    } catch (cause) {
      setProbe(null);
      setError(failureText(cause, '测试连接失败'));
    } finally {
      setProbing(false);
    }
  };

  const submit = async () => {
    if (!address || !displayName.trim()) return;
    setSubmitting(true);
    setError('');
    try {
      await onSubmit({
        apiBase: address,
        displayName: displayName.trim(),
        purpose: resolvedPurpose,
        enrollmentToken: enrollmentToken.trim(),
      });
      onClose();
    } catch (cause) {
      setError(failureText(cause, '连接工作台失败'));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal workbench-connection-modal" role="dialog" aria-modal="true" aria-labelledby="workbench-add-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header">
          <div><h3 id="workbench-add-title">连接工作台</h3><p>填地址 → 登记 → 登录账号；完成后本机开始接收该工作台的任务。</p></div>
          <IconButton icon={X} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body">
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-api-base">工作台地址</label>
            <input id="workbench-api-base" autoComplete="off" placeholder="http://localhost:8080" value={apiBase} disabled={submitting} onChange={event => { setApiBase(event.target.value); setProbe(null); }} />
            <p className="field-hint">工作台首页地址即可，不要带路径。</p>
            <div className="actions-row" style={{ marginTop: 8 }}>
              <button type="button" className="btn" disabled={!address || probing || submitting} onClick={() => void test()}>{probing ? <BusyIndicator size={15} /> : null}测试连接</button>
              {probe ? <span className={`workbench-probe ${probe.reachable ? 'ok' : 'fail'}`}>{probe.message}</span> : null}
            </div>
          </div>
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-display-name">名称</label>
            <input id="workbench-display-name" autoComplete="off" placeholder="例如：公司工作台" value={displayName} disabled={submitting} onChange={event => setDisplayName(event.target.value)} />
          </div>
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-purpose">用途</label>
            <div className="segmented-control" role="group" aria-label="工作台用途">
              {[...PURPOSE_PRESETS, '自定义'].map(option => (
                <button key={option} type="button" className={purpose === option ? 'active' : ''} disabled={submitting} onClick={() => setPurpose(option)}>{option}</button>
              ))}
            </div>
            {purpose === '自定义' ? <input style={{ marginTop: 8 }} autoComplete="off" placeholder="例如：测试环境" value={customPurpose} disabled={submitting} onChange={event => setCustomPurpose(event.target.value)} /> : null}
          </div>
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-enrollment-token">登记码（可选）</label>
            <input id="workbench-enrollment-token" autoComplete="off" placeholder="工作台生成的登记码" value={enrollmentToken} disabled={submitting} onChange={event => setEnrollmentToken(event.target.value)} />
            <p className="field-hint">填了就在连接时一并登记；留空先连上，之后再补登记也可以。</p>
          </div>
          {error ? <div className="inline-feedback visible workbench-connection-error" role="alert">{error}</div> : null}
          <div className="modal-actions">
            <span />
            <div className="actions-row">
              <button type="button" className="btn" disabled={submitting} onClick={onClose}>取消</button>
              <button type="button" className="btn btn-primary" disabled={!address || !displayName.trim() || submitting} onClick={() => void submit()}>{submitting ? <BusyIndicator size={15} /> : null}连接</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function RenameConnectionDialog({
  connection,
  onClose,
  onSubmit,
}: {
  connection: WorkbenchConnection;
  onClose: () => void;
  onSubmit: (id: string, displayName: string, purpose: string) => Promise<void>;
}) {
  const [displayName, setDisplayName] = useState(connection.display_name);
  const [purpose, setPurpose] = useState(connection.purpose);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');

  const submit = async () => {
    if (!displayName.trim()) return;
    setSubmitting(true);
    setError('');
    try {
      await onSubmit(connection.id, displayName.trim(), purpose.trim());
      onClose();
    } catch (cause) {
      setError(failureText(cause, '保存失败'));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal workbench-connection-modal" role="dialog" aria-modal="true" aria-labelledby="workbench-rename-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header">
          <div><h3 id="workbench-rename-title">重命名工作台</h3><p>{connection.api_base}</p></div>
          <IconButton icon={PencilLine} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body">
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-rename-name">名称</label>
            <input id="workbench-rename-name" autoComplete="off" value={displayName} disabled={submitting} onChange={event => setDisplayName(event.target.value)} />
          </div>
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-rename-purpose">用途</label>
            <input id="workbench-rename-purpose" autoComplete="off" placeholder="例如：生产环境" value={purpose} disabled={submitting} onChange={event => setPurpose(event.target.value)} />
          </div>
          {error ? <div className="inline-feedback visible workbench-connection-error" role="alert">{error}</div> : null}
          <div className="modal-actions">
            <span />
            <div className="actions-row">
              <button type="button" className="btn" disabled={submitting} onClick={onClose}>取消</button>
              <button type="button" className="btn btn-primary" disabled={!displayName.trim() || submitting} onClick={() => void submit()}>{submitting ? <BusyIndicator size={15} /> : null}保存</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function EnrollConnectionDialog({
  connection,
  onClose,
  onSubmit,
}: {
  connection: WorkbenchConnection;
  onClose: () => void;
  onSubmit: (id: string, enrollmentToken: string) => Promise<void>;
}) {
  const [token, setToken] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');

  const submit = async () => {
    if (!token.trim()) return;
    setSubmitting(true);
    setError('');
    try {
      await onSubmit(connection.id, token.trim());
      onClose();
    } catch (cause) {
      setError(failureText(cause, '登记失败'));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal workbench-connection-modal" role="dialog" aria-modal="true" aria-labelledby="workbench-enroll-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header">
          <div><h3 id="workbench-enroll-title">登记到工作台</h3><p>{connection.api_base}</p></div>
          <IconButton icon={Cloud} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body">
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-enroll-token">登记码</label>
            <input id="workbench-enroll-token" autoComplete="off" placeholder="工作台生成的登记码" value={token} disabled={submitting} onChange={event => setToken(event.target.value)} />
              <p className="field-hint">登记后工作台会记下本机身份；还需登录账号。</p>
          </div>
          {error ? <div className="inline-feedback visible workbench-connection-error" role="alert">{error}</div> : null}
          <div className="modal-actions">
            <span />
            <div className="actions-row">
              <button type="button" className="btn" disabled={submitting} onClick={onClose}>取消</button>
              <button type="button" className="btn btn-primary" disabled={!token.trim() || submitting} onClick={() => void submit()}>{submitting ? <BusyIndicator size={15} /> : null}登记</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

