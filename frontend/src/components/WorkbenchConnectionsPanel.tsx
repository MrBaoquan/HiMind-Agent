import { useEffect, useRef, useState } from 'react';
import { ArrowUpRight, Cloud, KeyRound, MoreHorizontal, PencilLine, Plus, RefreshCw, Server, ShieldAlert, ShieldCheck, Trash2, Unplug, X } from 'lucide-react';
import type { DashboardAuthorizationProgress, DashboardIdentityStatus, WorkbenchConnection, WorkbenchConnectionsSnapshot, WorkbenchProbe } from '../services/agentApi';
import { ActionMenu, ActionMenuItem } from './ActionMenu';
import { BusyIndicator } from './BusyIndicator';
import { IconButton, Pill } from './Common';
import { useModalFocus } from './useModalFocus';

export type WorkbenchConnectionDraft = {
  displayName: string;
  apiBase: string;
  enrollmentToken: string;
};

/** 连接状态只说事实：本机有没有这台工作台的 Agent 身份、有没有登录账号。 */
function connectionState(connection: WorkbenchConnection): { label: string; kind: 'success' | 'warn' | 'neutral' } {
  if (connection.authorized) return { label: '已授权', kind: 'success' };
  if (connection.registered) return { label: '待确认', kind: 'warn' };
  return { label: '未授权', kind: 'neutral' };
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
  onRename: (id: string, displayName: string) => Promise<void>;
  onRemove: (connection: WorkbenchConnection) => void;
  onSwitch: (connection: WorkbenchConnection) => void;
  onEnroll: (id: string, enrollmentToken: string) => Promise<void>;
  /** 打开某个连接的 Agent 绑定页：登记码由该工作台签发后直接送回本机，用户不需要看到它。 */
  onOpenBindingPage: (id: string) => void;
  /** 轮询刷新连接状态：绑定在浏览器里完成，本机要自己发现「已经登记上了」。 */
  onPoll: () => void;
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
  onOpenBindingPage,
  onPoll,
  onProbe,
  onAuthorize,
  onRevoke,
}: WorkbenchConnectionsPanelProps) {
  const [addOpen, setAddOpen] = useState(false);
  const [renameTarget, setRenameTarget] = useState<WorkbenchConnection | null>(null);
  const [enrollTarget, setEnrollTarget] = useState<WorkbenchConnection | null>(null);
  const enrollTargetRegistered = enrollTarget
    ? snapshot?.connections.find(item => item.id === enrollTarget.id)?.registered === true
    : false;
  // 绑定成功即自动收窗：用户不需要记得回来关掉它，关掉后行上就是「待授权」。
  useEffect(() => {
    if (enrollTargetRegistered) setEnrollTarget(null);
  }, [enrollTargetRegistered]);

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
    <section className="card settings-section workbench-connections" id="workbench-connections" aria-labelledby="workbench-account-title">
      <div className="card-header">
        <h2 id="workbench-account-title">工作台账号</h2>
        <Pill kind={headerState.kind}>{headerState.label}</Pill>
      </div>
      <div className="credential-section">
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
                onOpenBindingPage={onOpenBindingPage}
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
          <span>连接后可接收工作台任务并同步运行记录；切换不会影响本机 AI、技能和插件。</span>
        </div>
      </div>
      {addOpen ? <AddConnectionDialog onClose={() => setAddOpen(false)} onProbe={onProbe} onSubmit={onAdd} /> : null}
      {renameTarget ? <RenameConnectionDialog connection={renameTarget} onClose={() => setRenameTarget(null)} onSubmit={onRename} /> : null}
      {enrollTarget ? <EnrollConnectionDialog connection={enrollTarget} onClose={() => setEnrollTarget(null)} onSubmit={onEnroll} onOpenBindingPage={onOpenBindingPage} onPoll={onPoll} /> : null}
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
  onOpenBindingPage,
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
  onOpenBindingPage: (id: string) => void;
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
        </div>
        <small title={detail}>{detail}</small>
      </div>
      <Pill kind={state.kind}>{state.label}</Pill>
      <div className="workbench-connection-actions">
        {connection.active ? (
          connection.authorized
            ? <button type="button" className="btn btn-danger-quiet" disabled={disabled || flowActive} onClick={onRevoke}>{rowBusy || flowActive ? <BusyIndicator size={15} /> : null}取消授权</button>
            : <>
                <button type="button" className="btn btn-danger-quiet" title="停止对接 AI 工作台，保留本机能力" disabled={disabled || flowActive} onClick={onRevoke}><Unplug size={15} />停止对接</button>
                <button type="button" className="btn btn-primary" disabled={disabled || flowActive || (connection.registered && authorizeDisabled)} onClick={() => connection.registered ? onAuthorize(connection) : onOpenBindingPage(connection.id)}>{rowBusy ? <BusyIndicator size={15} /> : null}授权</button>
              </>
        ) : connection.registered ? (
          <button type="button" className="btn" disabled={disabled} onClick={() => onSwitch(connection)}>{rowBusy ? <BusyIndicator size={15} /> : null}切换</button>
        ) : (
          <button type="button" className="btn" disabled={disabled} onClick={() => onOpenBindingPage(connection.id)}>授权</button>
        )}
        <ActionMenu icon={<MoreHorizontal size={16} />} title={`${connection.display_name || connection.api_base} 的更多操作`} disabled={disabled}>
          {close => (
            <>
              <ActionMenuItem icon={<PencilLine size={15} />} label="重命名" onClick={() => { close(); onRename(); }} />
              <ActionMenuItem icon={<KeyRound size={15} />} label="手填授权码" onClick={() => { close(); onEnroll(); }} />
              <ActionMenuItem
                icon={<Trash2 size={15} />}
                label={connection.active ? '移除当前连接' : '移除'}
                danger
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
  const [enrollmentToken, setEnrollmentToken] = useState('');
  const [probe, setProbe] = useState<WorkbenchProbe | null>(null);
  const [probing, setProbing] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const dialogRef = useRef<HTMLDivElement>(null);
  const firstInputRef = useRef<HTMLInputElement>(null);
  useModalFocus(dialogRef, true, onClose, firstInputRef);

  const address = apiBase.trim();

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
    if (!address) return;
    setSubmitting(true);
    setError('');
    try {
      await onSubmit({
        apiBase: address,
        displayName: displayName.trim(),
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
      <div ref={dialogRef} className="modal workbench-connection-modal" role="dialog" aria-modal="true" tabIndex={-1} aria-labelledby="workbench-add-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header">
          <div><h3 id="workbench-add-title">连接工作台</h3><p>填地址连接，之后在工作台点一下完成授权。</p></div>
          <IconButton icon={X} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body">
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-api-base">工作台地址</label>
            <input ref={firstInputRef} id="workbench-api-base" autoComplete="off" placeholder="http://localhost:8080" value={apiBase} disabled={submitting} onChange={event => { setApiBase(event.target.value); setProbe(null); }} />
            <p className="field-hint">工作台首页地址即可，不要带路径。</p>
            <div className="actions-row" style={{ marginTop: 8 }}>
              <button type="button" className="btn" disabled={!address || probing || submitting} onClick={() => void test()}>{probing ? <BusyIndicator size={15} /> : null}测试连接</button>
              {probe ? <span className={`workbench-probe ${probe.reachable ? 'ok' : 'fail'}`}>{probe.message}</span> : null}
            </div>
          </div>
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-display-name">名称（可选）</label>
            <input id="workbench-display-name" autoComplete="off" placeholder="留空时使用工作台地址" value={displayName} disabled={submitting} onChange={event => setDisplayName(event.target.value)} />
          </div>
          <details className="field-group">
            <summary className="field-hint">有工作台提供的授权码</summary>
            <input id="workbench-enrollment-token" autoComplete="off" placeholder="由工作台或管理员签发" value={enrollmentToken} disabled={submitting} onChange={event => setEnrollmentToken(event.target.value)} />
            <p className="field-hint">留空即可，连接后在工作台点一下即可授权。</p>
          </details>
          {error ? <div className="inline-feedback visible workbench-connection-error" role="alert">{error}</div> : null}
          <div className="modal-actions">
            <span />
            <div className="actions-row">
              <button type="button" className="btn" disabled={submitting} onClick={onClose}>取消</button>
              <button type="button" className="btn btn-primary" disabled={!address || submitting} onClick={() => void submit()}>{submitting ? <BusyIndicator size={15} /> : null}连接</button>
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
  onSubmit: (id: string, displayName: string) => Promise<void>;
}) {
  const [displayName, setDisplayName] = useState(connection.display_name);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const dialogRef = useRef<HTMLDivElement>(null);
  const firstInputRef = useRef<HTMLInputElement>(null);
  useModalFocus(dialogRef, true, onClose, firstInputRef);

  const submit = async () => {
    if (!displayName.trim()) return;
    setSubmitting(true);
    setError('');
    try {
      await onSubmit(connection.id, displayName.trim());
      onClose();
    } catch (cause) {
      setError(failureText(cause, '保存失败'));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div ref={dialogRef} className="modal workbench-connection-modal" role="dialog" aria-modal="true" tabIndex={-1} aria-labelledby="workbench-rename-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header">
          <div><h3 id="workbench-rename-title">重命名工作台</h3><p>{connection.api_base}</p></div>
          <IconButton icon={PencilLine} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body">
          <div className="field-group">
            <label className="field-label" htmlFor="workbench-rename-name">名称</label>
            <input ref={firstInputRef} id="workbench-rename-name" autoComplete="off" value={displayName} disabled={submitting} onChange={event => setDisplayName(event.target.value)} />
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
  onOpenBindingPage,
  onPoll,
}: {
  connection: WorkbenchConnection;
  onClose: () => void;
  onSubmit: (id: string, enrollmentToken: string) => Promise<void>;
  onOpenBindingPage: (id: string) => void;
  onPoll: () => void;
}) {
  const [token, setToken] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const dialogRef = useRef<HTMLDivElement>(null);
  const primaryActionRef = useRef<HTMLButtonElement>(null);
  useModalFocus(dialogRef, true, onClose, primaryActionRef);

  // 绑定是在工作台页面里完成的，本机收不到本地事件 —— 弹窗打开期间定期刷新连接状态，
  // 用户在工作台点完就能看到这里翻成「已登记」。
  useEffect(() => {
    const timer = window.setInterval(onPoll, 3000);
    return () => window.clearInterval(timer);
  }, [onPoll]);

  const submit = async () => {
    if (!token.trim()) return;
    setSubmitting(true);
    setError('');
    try {
      await onSubmit(connection.id, token.trim());
      onClose();
    } catch (cause) {
      setError(failureText(cause, '授权失败'));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div ref={dialogRef} className="modal workbench-connection-modal" role="dialog" aria-modal="true" tabIndex={-1} aria-labelledby="workbench-enroll-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header">
          <div><h3 id="workbench-enroll-title">授权到工作台</h3><p>{connection.api_base}</p></div>
          <IconButton icon={Cloud} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body">
          <div className="field-group">
            <p className="field-hint">在工作台点一下即可，授权码会自动回到本机。</p>
            <p className="field-hint">绑定完成后这里会自动更新，无需刷新。</p>
            <div className="actions-row">
              <button ref={primaryActionRef} type="button" className="btn btn-primary" disabled={submitting} onClick={() => onOpenBindingPage(connection.id)}><ArrowUpRight size={15} />打开工作台绑定本机</button>
            </div>
          </div>
          <details className="field-group">
            <summary className="field-hint">手填授权码</summary>
            <label className="field-label" htmlFor="workbench-enroll-token">授权码</label>
            <input id="workbench-enroll-token" autoComplete="off" placeholder="由工作台或管理员签发" value={token} disabled={submitting} onChange={event => setToken(event.target.value)} />
            <p className="field-hint">由工作台或管理员签发，有有效期。</p>
          </details>
          {error ? <div className="inline-feedback visible workbench-connection-error" role="alert">{error}</div> : null}
          <div className="modal-actions">
            <span />
            <div className="actions-row">
              <button type="button" className="btn" disabled={submitting} onClick={onClose}>取消</button>
              <button type="button" className="btn btn-primary" disabled={!token.trim() || submitting} onClick={() => void submit()}>{submitting ? <BusyIndicator size={15} /> : null}授权</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

