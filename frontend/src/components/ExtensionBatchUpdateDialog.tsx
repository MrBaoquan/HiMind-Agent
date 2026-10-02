import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { CircleAlert, Download, RotateCw, ShieldCheck, X } from 'lucide-react';
import { BusyIndicator } from './BusyIndicator';
import { extensionKindLabels, type ExtensionKind } from '../data/extensionKinds';
import type {
  ExtensionBatchUpdateReport,
  ExtensionUpdateCandidate,
  ExtensionUpdateOutcome,
  ExtensionUpdateProgress,
  ExtensionUpdateTarget,
} from '../services/agentApi';

/**
 * 一键批量更新。
 *
 * 市场与「我的能力」都按版本号判断「可更新」，但真正能不能安全地自动更新，取决于
 * 本机安装台账里这个扩展当初是从哪个来源装的 —— 这件事只有后端知道，所以候选与
 * 分组都由 `plan_extension_updates` 给出，这里只负责让「哪些会被更新」在动手之前
 * 就说清楚：
 *   可直接更新 —— 来源与台账一致，默认勾选；
 *   需要确认   —— 没有台账或来源已变更，逐条给出原因，用户勾选才更新；
 *   跟随组织   —— 版本由组织推进，这里只展示、不参与。
 */

const GROUP_ORDER = ['ready', 'review', 'managed'] as const;

const GROUP_LABELS: Record<string, { title: string; hint: string }> = {
  ready: { title: '可直接更新', hint: '安装来源与本次更新来源一致' },
  review: { title: '需要确认', hint: '安装来源与台账对不上，勾选后才更新' },
  managed: { title: '跟随组织策略', hint: '版本由组织统一推进，不在批量更新范围内' },
};

/// 进度行的状态口径与后端 `ExtensionUpdateProgress.status` 对齐，只多一个
/// 「还没轮到」的 pending，好让整份清单在第一个事件到达前就有形状。
type RowStatus = 'pending' | 'running' | 'updated' | 'failed' | 'cancelled';

type UpdateRow = {
  identity: string;
  asset_kind: string;
  asset_id: string;
  name: string;
  from_version: string;
  to_version: string;
  status: RowStatus;
  message: string;
};

function rowIdentity(kind: string, id: string) {
  return `${kind}:${id}`;
}

function kindLabel(kind: string) {
  return extensionKindLabels[kind as ExtensionKind] || kind;
}

function toTarget(candidate: ExtensionUpdateCandidate): ExtensionUpdateTarget {
  return {
    asset_kind: candidate.asset_kind,
    asset_id: candidate.asset_id,
    version: candidate.target_version,
    source_id: candidate.source_id,
    sha256: candidate.sha256,
    artifact_id: candidate.artifact_id,
  };
}

function readError(error: unknown, fallback: string) {
  if (typeof error === 'string' && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  return fallback;
}

function statusLabel(status: RowStatus) {
  if (status === 'updated') return '已更新';
  if (status === 'failed') return '更新失败';
  if (status === 'cancelled') return '已取消';
  if (status === 'running') return '正在更新';
  return '等待中';
}

function statusTone(status: RowStatus) {
  if (status === 'updated') return 'success';
  if (status === 'failed') return 'danger';
  if (status === 'cancelled') return 'warn';
  return '';
}

export function ExtensionBatchUpdateDialog({ open, onClose, onFinished }: {
  open: boolean;
  onClose: () => void;
  /// 只要有扩展真的更新成功就回调一次，让市场 / 我的能力重新取数。
  onFinished: () => void | Promise<void>;
}) {
  const [planning, setPlanning] = useState(false);
  const [planError, setPlanError] = useState('');
  const [candidates, setCandidates] = useState<ExtensionUpdateCandidate[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const [rows, setRows] = useState<UpdateRow[]>([]);
  const [running, setRunning] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [report, setReport] = useState<ExtensionBatchUpdateReport | null>(null);
  const [runError, setRunError] = useState('');
  const [current, setCurrent] = useState<{ index: number; total: number; name: string } | null>(null);
  const appliedRef = useRef<ExtensionUpdateTarget[]>([]);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const activeRef = useRef(false);
  const bodyRef = useRef<HTMLDivElement | null>(null);

  const loadPlan = useCallback(async () => {
    setPlanning(true);
    setPlanError('');
    try {
      const list = await invoke<ExtensionUpdateCandidate[]>('plan_extension_updates');
      if (!activeRef.current) return;
      setCandidates(list);
      // 「可直接更新」的来源已经被台账证明过，默认勾上；其余都要用户自己点。
      setSelected(list.filter(item => item.group === 'ready').map(item => rowIdentity(item.asset_kind, item.asset_id)));
    } catch (error) {
      if (!activeRef.current) return;
      setCandidates([]);
      setSelected([]);
      setPlanError(readError(error, '暂时无法读取可更新的扩展，请稍后重试。'));
    } finally {
      if (activeRef.current) setPlanning(false);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    activeRef.current = true;
    setCandidates([]);
    setSelected([]);
    setRows([]);
    setReport(null);
    setRunError('');
    setCurrent(null);
    setRunning(false);
    setCancelling(false);
    appliedRef.current = [];
    void loadPlan();
    return () => {
      activeRef.current = false;
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
  }, [loadPlan, open]);

  const grouped = useMemo(() => {
    const buckets = new Map<string, ExtensionUpdateCandidate[]>();
    for (const candidate of candidates) {
      const bucket = buckets.get(candidate.group) || [];
      bucket.push(candidate);
      buckets.set(candidate.group, bucket);
    }
    return GROUP_ORDER
      .map(group => ({ group, items: buckets.get(group) || [] }))
      .filter(section => section.items.length > 0);
  }, [candidates]);

  // 清单换内容就回到顶部。WebView 会在内容变高时保持自己的滚动锚点，如果不管它，
  // 打开弹窗看到的第一屏就是清单中段 —— 上面那一组像是不存在。
  const started = rows.length > 0;
  useEffect(() => {
    if (bodyRef.current) bodyRef.current.scrollTop = 0;
  }, [candidates, started]);

  const managedCount = candidates.filter(item => item.group === 'managed').length;
  const reviewCount = candidates.filter(item => item.group === 'review').length;
  const readyCount = candidates.filter(item => item.group === 'ready').length;
  const selectedCount = selected.length;
  const failedRows = report ? report.outcomes.filter(outcome => outcome.status === 'failed') : [];

  const runUpdate = useCallback(async (targets: ExtensionUpdateTarget[]) => {
    if (!targets.length) return;
    appliedRef.current = targets;
    const byCandidate = new Map(candidates.map(item => [rowIdentity(item.asset_kind, item.asset_id), item]));
    setRows(targets.map(target => {
      const identity = rowIdentity(target.asset_kind, target.asset_id);
      const candidate = byCandidate.get(identity);
      return {
        identity,
        asset_kind: target.asset_kind,
        asset_id: target.asset_id,
        name: candidate?.name || target.asset_id,
        from_version: candidate?.installed_version || '',
        to_version: target.version,
        status: 'pending' as RowStatus,
        message: '',
      };
    }));
    setCurrent(null);
    setReport(null);
    setRunError('');
    setCancelling(false);
    setRunning(true);

    unlistenRef.current?.();
    try {
      unlistenRef.current = await listen<ExtensionUpdateProgress>('himind:extension-update-progress', event => {
        const progress = event.payload;
        const identity = rowIdentity(progress.asset_kind, progress.asset_id);
        setCurrent({ index: progress.index, total: progress.total, name: progress.name || progress.asset_id });
        setRows(list => list.map(row => row.identity === identity
          ? {
            ...row,
            name: progress.name || row.name,
            from_version: progress.from_version || row.from_version,
            to_version: progress.to_version || row.to_version,
            status: (progress.status || 'running') as RowStatus,
            message: progress.message,
          }
          : row));
      });
    } catch {
      // 进度事件只是长任务的可见性；订阅失败不影响更新本身，整份结果照旧由
      // 返回值给出，所以这里不把用户挡在门外。
      unlistenRef.current = null;
    }

    try {
      const result = await invoke<ExtensionBatchUpdateReport>('apply_extension_updates', { targets });
      if (!activeRef.current) return;
      setReport(result);
      setRows(list => list.map(row => {
        const outcome = result.outcomes.find(item => rowIdentity(item.asset_kind, item.asset_id) === row.identity);
        if (!outcome) return row;
        return {
          ...row,
          name: outcome.name || row.name,
          from_version: outcome.from_version || row.from_version,
          to_version: outcome.to_version || row.to_version,
          status: outcome.status as RowStatus,
          message: outcome.message,
        };
      }));
      if (result.updated_count > 0) await onFinished();
    } catch (error) {
      if (!activeRef.current) return;
      setRunError(readError(error, '批量更新没有完成，请稍后重试。'));
    } finally {
      if (activeRef.current) {
        setRunning(false);
        setCancelling(false);
      }
    }
  }, [candidates, onFinished]);

  const startUpdate = useCallback(() => {
    const chosen = new Set(selected);
    const targets = candidates
      .filter(item => item.group !== 'managed' && chosen.has(rowIdentity(item.asset_kind, item.asset_id)))
      .map(toTarget);
    void runUpdate(targets);
  }, [candidates, runUpdate, selected]);

  const retryFailed = useCallback(() => {
    const failed = new Set(failedRows.map(outcome => rowIdentity(outcome.asset_kind, outcome.asset_id)));
    const targets = appliedRef.current.filter(target => failed.has(rowIdentity(target.asset_kind, target.asset_id)));
    void runUpdate(targets);
  }, [failedRows, runUpdate]);

  const cancel = useCallback(async () => {
    setCancelling(true);
    try {
      await invoke('cancel_extension_updates');
    } catch {
      // 取消失败说明这一批已经跑完了，下一步的返回值会给出真实结果。
      setCancelling(false);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      // 更新已经在写盘的时候不许用 Esc 关窗：关掉只会让人以为任务被取消了。
      if (running) return;
      onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [onClose, open, running]);

  if (!open) return null;

  const title = running ? '正在更新扩展' : report ? '更新结果' : '更新扩展';

  return (
    <div className="skill-dialog-backdrop">
      <div className="skill-dialog extension-update-dialog" role="dialog" aria-modal="true" aria-labelledby="extension-update-title">
        <div className="skill-dialog-head">
          <strong id="extension-update-title">{title}</strong>
          <button className="btn btn-icon" onClick={onClose} disabled={running} aria-label="关闭"><X size={16} /></button>
        </div>

        {planning ? <div className="extension-update-loading"><BusyIndicator size={15} />正在核对安装来源</div> : null}
        {planError ? <div className="skill-dialog-warning"><CircleAlert size={16} />{planError}</div> : null}
        {runError ? <div className="skill-dialog-warning"><CircleAlert size={16} />{runError}</div> : null}

        {!planning && !planError && !candidates.length ? (
          <p className="extension-update-intro">当前没有可更新的扩展。</p>
        ) : null}

        {!planning && !planError && candidates.length && !rows.length ? (
          <p className="extension-update-intro">
            {readyCount ? `${readyCount} 项可直接更新` : '没有可直接更新的项'}
            {reviewCount ? `，${reviewCount} 项需要确认` : ''}
            {managedCount ? `，${managedCount} 项跟随组织策略` : ''}。
          </p>
        ) : null}

        {!planning && !planError && !rows.length ? (
          <div className="extension-update-body" ref={bodyRef}>
            {grouped.map(section => {
              const meta = GROUP_LABELS[section.group] || { title: section.group, hint: '' };
              const managed = section.group === 'managed';
              return (
                <section className="extension-update-group" key={section.group}>
                  <div className="extension-update-group-head">
                    {managed ? <ShieldCheck size={13} /> : null}
                    <strong>{meta.title}</strong>
                    <span>{section.items.length} 项</span>
                    {meta.hint ? <span className="extension-update-group-hint">{meta.hint}</span> : null}
                  </div>
                  {section.items.map(candidate => {
                    const identity = rowIdentity(candidate.asset_kind, candidate.asset_id);
                    const body = (
                      <>
                        {managed
                          ? <span className="extension-update-mark" aria-hidden="true" />
                          : <input
                            type="checkbox"
                            checked={selected.includes(identity)}
                            onChange={event => setSelected(list => event.target.checked ? [...list, identity] : list.filter(item => item !== identity))}
                          />}
                        <span className="extension-update-copy">
                          <strong>{candidate.name}</strong>
                          <small>{kindLabel(candidate.asset_kind)} · 来源 {candidate.source_name || candidate.source_id || '未知'}</small>
                          {candidate.reason ? <small className="extension-update-reason">{candidate.reason}</small> : null}
                        </span>
                        <code>v{candidate.installed_version} → v{candidate.target_version}</code>
                      </>
                    );
                    // 组织项的版本不是用户能改的，所以它不该长成一个可以点的复选框。
                    return managed
                      ? <div className="extension-update-row managed" key={identity}>{body}</div>
                      : <label className="extension-update-row" key={identity}>{body}</label>;
                  })}
                </section>
              );
            })}
          </div>
        ) : null}

        {rows.length ? (
          <>
            <div className="extension-update-progress-head">
              {running
                ? <><BusyIndicator size={14} /><span>{current ? `正在更新 ${current.index} / ${current.total} · ${current.name}` : '正在准备更新'}</span></>
                : <span>{report ? `已更新 ${report.updated_count} 项${report.failed_count ? `，${report.failed_count} 项未完成` : ''}` : '更新已结束'}</span>}
            </div>
            <div className="extension-update-body" ref={bodyRef}>
              {rows.map(row => (
                <div className="extension-update-row" key={row.identity}>
                  <span className={`status-dot ${row.status === 'updated' ? 'success' : row.status === 'failed' ? 'danger' : ''}`} />
                  <span className="extension-update-copy">
                    <strong>{row.name}</strong>
                    <small>{kindLabel(row.asset_kind)}{row.message ? ` · ${row.message}` : ''}</small>
                  </span>
                  <span className="extension-update-state">
                    <span className={`skill-state-label ${statusTone(row.status)}`}>
                      {row.status === 'running' ? <BusyIndicator size={11} /> : null}{statusLabel(row.status)}
                    </span>
                    <code>v{row.from_version || '--'} → v{row.to_version}</code>
                  </span>
                </div>
              ))}
            </div>
          </>
        ) : null}

        <div className="skill-dialog-actions">
          {running ? (
            <button className="btn" onClick={() => void cancel()} disabled={cancelling}>
              {cancelling ? <><BusyIndicator size={14} />正在取消</> : '取消剩余项'}
            </button>
          ) : (
            <>
              <button className="btn" onClick={onClose}>{report || !candidates.length ? '关闭' : '取消'}</button>
              {report && failedRows.length ? <button className="btn" onClick={retryFailed}><RotateCw size={15} />重试失败项（{failedRows.length}）</button> : null}
              {!report && candidates.length ? (
                <button className="btn btn-primary" onClick={startUpdate} disabled={!selectedCount}>
                  <Download size={15} />{selectedCount ? `更新 ${selectedCount} 项` : '更新选中项'}
                </button>
              ) : null}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
