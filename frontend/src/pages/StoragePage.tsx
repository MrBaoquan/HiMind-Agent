import { useEffect, useRef, useState } from 'react';
import { FolderOpen, HardDriveDownload, RefreshCw, Trash2 } from 'lucide-react';
import { agentApi, type AgentStorageCleanup, type AgentStorageCleanupResult, type AgentStorageMigrationPlan, type AgentStorageReport } from '../services/agentApi';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { Pill } from '../components/Common';

/** 「保留最近 N 个版本」的可选值。上限与后端 `DRAFT_KEEP_MAX` 对齐。 */
const KEEP_OPTIONS = [1, 2, 3];
/** 低于这个剩余空间就提示先清理：装一次拓展或运行时都要几百 MB。 */
const LOW_SPACE_BYTES = 5 * 1024 ** 3;

/**
 * 上一次统计结果。切页签会卸载/重挂这个面板，而一次统计要扫十几 GB；
 * 直接用上次结果先画出来，再在后台刷新，比每次空白几秒更像桌面工具。
 */
let cachedReport: AgentStorageReport | null = null;

/**
 * 数据与诊断 → 存储占用。
 *
 * 这一屏回答三个问题：空间被谁占了、哪些能安全删掉、用户数据能不能换个位置。
 * 迁移只改数据根，程序仍留在安装目录；复制在重启后的启动早期完成，所以界面
 * 只负责「排队 + 提示重启」，不在运行中搬库。
 */
export function StoragePage() {
  const confirm = useConfirm();
  const [report, setReport] = useState<AgentStorageReport | null>(cachedReport);
  const [loading, setLoading] = useState(!cachedReport);
  const [busy, setBusy] = useState('');
  const [keep, setKeep] = useState(cachedReport?.draft_keep ?? 2);
  const [feedback, setFeedback] = useState('');
  const [plan, setPlan] = useState<AgentStorageMigrationPlan | null>(null);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    agentApi.storageReport()
      .then(result => {
        cachedReport = result.report;
        if (alive.current) { setReport(result.report); setKeep(result.report.draft_keep); }
      })
      .catch(error => { if (alive.current) setFeedback(`读取存储占用失败：${failureText(error)}`); })
      .finally(() => { if (alive.current) setLoading(false); });
    return () => { alive.current = false; };
  }, []);

  async function reload(nextKeep = keep) {
    setBusy('reload');
    setFeedback('');
    try {
      const result = await agentApi.storageReport(nextKeep);
      if (!alive.current) return;
      cachedReport = result.report;
      setReport(result.report);
      setKeep(result.report.draft_keep);
    } catch (error) {
      setFeedback(`读取存储占用失败：${failureText(error)}`);
    } finally {
      setBusy('');
    }
  }

  async function changeKeep(nextKeep: number) {
    setKeep(nextKeep);
    await reload(nextKeep);
  }

  async function chooseLocation() {
    setBusy('location');
    setFeedback('');
    try {
      const picked = await agentApi.pickStorageLocation();
      if (picked.canceled) return;
      const result = await agentApi.planStorageMigration(picked.path);
      setPlan(result.plan);
    } catch (error) {
      setFeedback(`无法校验目标位置：${failureText(error)}`);
    } finally {
      setBusy('');
    }
  }

  async function queueMigration() {
    if (!plan) return;
    const accepted = await confirm({
      title: '迁移到新的数据位置？',
      description: `将复制 ${formatBytes(plan.bytes)} 到目标；重启后开始，旧数据保留在原位置。`,
      confirmText: '迁移并重启',
    });
    if (!accepted) return;
    setBusy('queue');
    setFeedback('');
    try {
      await agentApi.queueStorageMigration(plan.target);
      setPlan(null);
      await reload();
      setFeedback('已排队：Agent 重启后开始迁移。');
    } catch (error) {
      setFeedback(`无法开始迁移：${failureText(error)}`);
    } finally {
      setBusy('');
    }
  }

  async function restartNow() {
    const accepted = await confirm({
      title: '现在重启并迁移？',
      description: '窗口会关闭并重新打开；迁移期间请不要关闭 Agent。',
      confirmText: '重启',
    });
    if (!accepted) return;
    setBusy('restart');
    await agentApi.restartAgent();
  }

  async function cancelMigration() {
    setBusy('cancel');
    try {
      await agentApi.cancelStorageMigration();
      await reload();
      setFeedback('已取消排队中的迁移。');
    } catch (error) {
      setFeedback(`取消失败：${failureText(error)}`);
    } finally {
      setBusy('');
    }
  }

  async function dismissResult() {
    setBusy('dismiss');
    try {
      await agentApi.dismissStorageResult();
      await reload();
    } catch (error) {
      setFeedback(`清除结果失败：${failureText(error)}`);
    } finally {
      setBusy('');
    }
  }

  async function runCleanup(option: AgentStorageCleanup) {
    const accepted = await confirm({
      title: `清理${option.label}？`,
      description: `将删除 ${option.entries} 项，释放约 ${formatBytes(option.bytes)}；${option.note}。`,
      confirmText: '清理',
    });
    if (!accepted) return;
    setBusy(option.key);
    setFeedback('');
    try {
      const result = option.key === 'drafts'
        ? await agentApi.cleanupDrafts(keep)
        : option.key === 'versions'
          ? await agentApi.cleanupVersions()
          : await agentApi.cleanupCache();
      if (!alive.current) return;
      cachedReport = result.report;
      setReport(result.report);
      setKeep(result.report.draft_keep);
      setFeedback(cleanupFeedback(result));
    } catch (error) {
      setFeedback(`清理失败：${failureText(error)}`);
    } finally {
      setBusy('');
    }
  }

  if (loading) {
    return <div className="page-loading"><BusyIndicator size={15} />正在统计存储占用</div>;
  }
  if (!report) {
    return <section className="card settings-section"><div className="card-body"><div className="inline-feedback visible" role="status">{feedback || '读取存储占用失败。'}</div></div></section>;
  }

  const location = report.location;
  const reclaimable = report.cleanup.reduce((sum, option) => sum + option.bytes, 0);
  const drafts = report.cleanup.find(option => option.key === 'drafts');

  return <>
    <section className="card settings-section">
      <div className="card-header"><span>数据位置</span><Pill kind={location.custom ? 'success' : 'neutral'}>{location.custom ? '已更换' : '默认位置'}</Pill></div>
      <div className="storage-summary">
        <div><span>数据目录</span><strong title={location.data_root}>{location.data_root}</strong></div>
        <div><span>程序位置</span><strong title={location.install_root}>{location.install_root}</strong></div>
      </div>
      <div className="card-body storage-toolbar">
        <small>程序与更新始终留在程序位置，只有用户数据会搬到新目录。</small>
        <div className="actions-row">
          <button type="button" className="btn" disabled={!location.can_move || Boolean(busy)} onClick={() => void chooseLocation()}>
            {busy === 'location' ? <BusyIndicator size={15} /> : <HardDriveDownload size={15} />}更改位置…
          </button>
        </div>
      </div>
      {!location.can_move ? <div className="card-body"><div className="storage-warning" role="status">{location.blocked_reason}</div></div> : null}

      {location.pending ? <div className="card-body storage-pending">
        <div>
          <strong>已排队迁移</strong>
          <small title={location.pending.target}>重启后复制到 {location.pending.target}</small>
        </div>
        <div className="actions-row">
          <button type="button" className="btn btn-primary" disabled={Boolean(busy)} onClick={() => void restartNow()}>{busy === 'restart' ? <BusyIndicator size={15} /> : <RefreshCw size={15} />}现在重启</button>
          <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => void cancelMigration()}>取消迁移</button>
        </div>
      </div> : null}

      {location.last_result ? <div className="card-body storage-pending">
        <div>
          <strong>{location.last_result.status === 'done' ? '上次迁移已完成' : '上次迁移未完成'}</strong>
          <small>{location.last_result.status === 'done'
            ? `数据现在位于 ${location.last_result.target}；旧目录没有删除。`
            : location.last_result.error || '未写入原因。'}</small>
        </div>
        <div className="actions-row">
          <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => void dismissResult()}>知道了</button>
        </div>
      </div> : null}

      {feedback ? <div className="card-body"><div className="inline-feedback visible" role="status">{feedback}</div></div> : null}
    </section>

    {plan ? <section className="card settings-section">
      <div className="card-header"><span>迁移到目标</span><Pill kind={plan.blocker ? 'danger' : 'neutral'}>{formatBytes(plan.bytes)}</Pill></div>
      <div className="storage-summary">
        <div><span>目标</span><strong title={plan.target}>{plan.target}</strong></div>
        <div><span>项目</span><strong>{plan.entries} 项</strong></div>
      </div>
      <div className="card-body storage-toolbar">
        <small>需要重启；失败时保留原数据位置，旧目录不会被删除。</small>
        <div className="actions-row">
          <button type="button" className="btn btn-primary" disabled={Boolean(busy) || Boolean(plan.blocker)} onClick={() => void queueMigration()}>{busy === 'queue' ? <BusyIndicator size={15} /> : <HardDriveDownload size={15} />}开始迁移</button>
          <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => setPlan(null)}>取消</button>
        </div>
      </div>
      {plan.blocker ? <div className="card-body"><div className="storage-warning" role="status">{plan.blocker}</div></div> : null}
      {plan.warnings.map(warning => <div className="card-body" key={warning}><div className="storage-warning" role="status">{warning}</div></div>)}
    </section> : null}

    <section className="card settings-section">
      <div className="card-header"><span>存储占用</span><Pill kind={reclaimable > 0 ? 'warn' : 'neutral'}>{formatBytes(report.total_bytes)}</Pill></div>
      <div className="storage-summary">
        <div><span>当前 profile</span><strong>{report.profile}</strong></div>
        <div><span>磁盘剩余</span><strong>{report.free_bytes > 0 ? formatBytes(report.free_bytes) : '未知'}</strong></div>
        <div><span>可清理</span><strong>{formatBytes(reclaimable)}</strong></div>
        <div><span>数据目录</span><strong title={report.root}>{report.root}</strong></div>
      </div>
      <div className="card-body storage-toolbar">
        <small>只清理当前 profile（{report.profile}）的数据，其它 profile 只统计。</small>
        <div className="actions-row">
          <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => void reload()}>
            {busy === 'reload' ? <BusyIndicator size={15} /> : <RefreshCw size={15} />}重新统计
          </button>
          <button type="button" className="btn" onClick={() => void agentApi.openAgentDirectory()}>
            <FolderOpen size={15} />打开数据目录
          </button>
        </div>
      </div>
      {report.free_bytes > 0 && report.free_bytes < LOW_SPACE_BYTES
        ? <div className="card-body"><div className="storage-warning" role="status">磁盘剩余不足 5 GB，建议先清理历史内容。</div></div>
        : null}
    </section>

    <section className="card settings-section">
      <div className="card-header">占用明细</div>
      <div className="storage-breakdown">
        {report.entries.map(entry => (
          <div className="storage-row" key={entry.key}>
            <div><strong>{entry.label}</strong><small title={entry.path}>{entry.note}</small></div>
            <span>{formatBytes(entry.bytes)}</span>
          </div>
        ))}
      </div>
    </section>

    <section className="card settings-section">
      <div className="card-header">清理历史内容</div>
      <div className="card-body setting-list">
        <div className="setting-row storage-cleanup-row">
          <div>
            <div className="label-text">拓展开发草稿</div>
            <div className="label-desc">{drafts && drafts.entries > 0 ? `每个插件保留最近 ${keep} 个版本，正在使用的保留。` : '没有可清理的内容。'}</div>
          </div>
          <div className="setting-control storage-cleanup-control">
            <label className="storage-keep">
              <span>保留</span>
              <select aria-label="每个插件保留的草稿版本数" value={keep} disabled={Boolean(busy)} onChange={event => void changeKeep(Number(event.target.value))}>
                {KEEP_OPTIONS.map(value => <option key={value} value={value}>{value} 个版本</option>)}
              </select>
            </label>
            <CleanupButton option={drafts} busy={busy} onCleanup={runCleanup} />
          </div>
        </div>
        {report.cleanup.filter(option => option.key !== 'drafts').map(option => (
          <div className="setting-row storage-cleanup-row" key={option.key}>
            <div>
              <div className="label-text">{option.label}</div>
              <div className="label-desc">{option.key === 'versions' ? '保留当前版本和上一版本。' : '只删 10 分钟内没有改动的内容。'}</div>
            </div>
            <div className="setting-control storage-cleanup-control">
              <CleanupButton option={option} busy={busy} onCleanup={runCleanup} />
            </div>
          </div>
        ))}
      </div>
    </section>
  </>;
}

function CleanupButton({ option, busy, onCleanup }: { option?: AgentStorageCleanup; busy: string; onCleanup: (option: AgentStorageCleanup) => void }) {
  if (!option || option.entries === 0) {
    return <span className="pill neutral">没有可清理的内容</span>;
  }
  return (
    <button type="button" className="btn btn-danger-quiet" disabled={Boolean(busy)} onClick={() => onCleanup(option)}>
      {busy === option.key ? <BusyIndicator size={15} /> : <Trash2 size={15} />}清理 {formatBytes(option.bytes)}
    </button>
  );
}

function cleanupFeedback(result: AgentStorageCleanupResult) {
  const freed = `已清理 ${result.removed} 项，释放 ${formatBytes(result.freed_bytes)}。`;
  return result.failures.length
    ? `${freed}${result.failures.length} 项删除失败，可能被占用。`
    : freed;
}

/** Tauri 拒绝时抛出的一般是字符串，先取出可读原因再补上下文。 */
function failureText(error: unknown) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error.trim()) return error.trim();
  return '未知原因';
}

function formatBytes(bytes: number) {
  if (!bytes || bytes < 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${index === 0 || value >= 10 ? Math.round(value) : value.toFixed(1)} ${units[index]}`;
}
