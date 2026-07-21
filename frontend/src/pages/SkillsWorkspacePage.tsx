import { useEffect, useMemo, useState } from 'react';
import {
  BookOpen,
  BadgeCheck,
  Building2,
  CheckCircle2,
  CircleAlert,
  FolderOpen,
  Download,
  Edit3,
  Play,
  Plus,
  Send,
  RefreshCw,
  Search,
  ShieldCheck,
  Sparkles,
  Trash2,
  Wrench,
  X,
} from 'lucide-react';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import type { AuthoringSkillDraft, AuthoringSkillDraftInput, CodexSkillStatusItem, CodexSkillStatusResponse, OrganizationSkillCatalogItem, SkillCatalogResponse, SkillInstallPlan, SkillSubmissionStatus } from '../services/agentApi';

type SkillsWorkspacePageProps = {
  catalog: SkillCatalogResponse | null;
  status: CodexSkillStatusResponse | null;
  error: string | null;
  marketplace: OrganizationSkillCatalogItem[];
  marketplaceError: string | null;
  drafts: AuthoringSkillDraft[];
  submissions: SkillSubmissionStatus[];
  busyAction: string | null;
  onRefresh: () => void;
  onSyncAll: () => void;
  onSyncSkill: (skillId: string) => void;
  onPlanMarketplace: (skillId: string) => Promise<SkillInstallPlan>;
  onInstallMarketplace: (skillId: string, optionalPluginIds: string[]) => void;
  onSaveDraft: (input: AuthoringSkillDraftInput) => Promise<AuthoringSkillDraft>;
  onTestDraft: (skillId: string, version: string) => void;
  onConfirmDraft: (skillId: string, version: string) => void;
  onSubmitDraft: (skillId: string, version: string) => void;
  onRepair: (skillId: string) => void;
  onUninstall: (skillId: string) => void;
  onOpenDirectory: (path: string) => void;
};

type ViewKey = 'marketplace' | 'installed' | 'mine';

export function SkillsWorkspacePage({ catalog, status, error, marketplace, marketplaceError, drafts, submissions, busyAction, onRefresh, onSyncAll, onSyncSkill, onPlanMarketplace, onInstallMarketplace, onSaveDraft, onTestDraft, onConfirmDraft, onSubmitDraft, onRepair, onUninstall, onOpenDirectory }: SkillsWorkspacePageProps) {
  const [query, setQuery] = useState('');
  const [view, setView] = useState<ViewKey>('marketplace');
  const [selectedId, setSelectedId] = useState('');
  const [pendingUninstall, setPendingUninstall] = useState<CodexSkillStatusItem | null>(null);
  const [editingDraft, setEditingDraft] = useState<AuthoringSkillDraft | null>(null);
  const [draftEditorOpen, setDraftEditorOpen] = useState(false);
  const [installPlan, setInstallPlan] = useState<SkillInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [planLoading, setPlanLoading] = useState(false);
  const items = status?.items || [];
	const installedById = useMemo(() => new Map(items.map(item => [item.record.manifest.id, item])), [items]);
	const localItems = useMemo(() => items.filter(item => view === 'mine' ? item.record.manifest.scope === 'user' : item.client_state !== 'not_installed'), [items, view]);
	const visibleMarket = useMemo(() => {
	  const normalized = query.trim().toLowerCase();
	  return marketplace.filter(item => !normalized || [item.skill_id, item.name, item.description, item.author_name, ...item.capability_ids].join(' ').toLowerCase().includes(normalized));
	}, [marketplace, query]);
	const visibleDrafts = useMemo(() => {
	  const normalized = query.trim().toLowerCase();
	  return drafts.filter(draft => !normalized || [draft.manifest.id, draft.manifest.name, draft.manifest.description].join(' ').toLowerCase().includes(normalized));
	}, [drafts, query]);

  useEffect(() => {
    const selectableIds = view === 'marketplace' ? marketplace.map(item => item.skill_id) : view === 'mine' ? drafts.map(draftKey) : localItems.map(item => item.record.manifest.id);
    if (!selectableIds.length) {
      setSelectedId('');
    } else if (!selectableIds.includes(selectedId)) {
      setSelectedId(selectableIds[0]);
    }
  }, [view, marketplace, localItems, drafts, selectedId]);

  const filteredItems = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return localItems.filter(item => {
      if (!normalized) return true;
      const manifest = item.record.manifest;
      return [manifest.id, manifest.name, manifest.description, manifest.risk_summary, ...(manifest.capabilities || []).map(capability => capability.id)].join(' ').toLowerCase().includes(normalized);
    });
  }, [localItems, query]);

  const selected = filteredItems.find(item => item.record.manifest.id === selectedId) || filteredItems[0];
	const selectedMarket = visibleMarket.find(item => item.skill_id === selectedId) || visibleMarket[0];
	const selectedDraft = visibleDrafts.find(item => draftKey(item) === selectedId) || visibleDrafts[0];
	const selectedSubmission = selectedDraft ? submissions.find(item => item.id === selectedDraft.dashboard_draft_id || (item.product_key === selectedDraft.manifest.id && item.version === selectedDraft.manifest.version)) : undefined;
  const installedCount = items.filter(item => ['installed', 'outdated', 'modified'].includes(item.client_state)).length;
  const attentionCount = items.filter(item => ['outdated', 'modified', 'blocked', 'failed'].includes(item.client_state)).length;
  const isBusy = Boolean(busyAction);

  if (!catalog && !status && !error) return <div className="page-loading"><span className="spinner" />正在读取 Skill 数据</div>;

  return (
    <div className="skill-page skill-product-page">
      <PageHeader
        title="AI 技能"
        description="管理可供 AI 客户端使用的组织知识、操作指令和能力依赖。"
        actions={<>
          {view === 'mine' ? <button className="btn btn-primary" onClick={() => { setEditingDraft(null); setDraftEditorOpen(true); }} disabled={isBusy}><Plus size={16} />新建 Skill</button> : null}
          <button className="btn btn-primary" onClick={onSyncAll} disabled={isBusy || !items.length}><RefreshCw className={busyAction === 'sync-all' ? 'spin' : ''} size={16} />{busyAction === 'sync-all' ? '正在同步' : '同步全部'}</button>
          <button className="btn btn-icon" title="打开 Codex Skill 目录" aria-label="打开 Codex Skill 目录" onClick={() => status?.target_root && onOpenDirectory(status.target_root)} disabled={!status?.target_root}><FolderOpen size={16} /></button>
          <button className="btn btn-icon" title="刷新状态" aria-label="刷新状态" onClick={onRefresh} disabled={isBusy}><RefreshCw size={16} /></button>
        </>}
      />

      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>Skill 状态读取失败</strong><span>{error}</span></div></div> : null}

      <section className={`skill-adapter-bar ${status?.target_mode === 'preview' ? 'warning' : ''}`}>
        <div className="skill-adapter-identity"><span className="skill-client-mark">C</span><div><strong>Codex</strong><span>{targetModeLabel(status?.target_mode)}</span></div></div>
        <div className="skill-adapter-state"><span className={`status-dot ${status?.target_mode === 'preview' ? 'danger' : 'success'}`} /><div><small>适配器状态</small><strong>{status ? (status.target_mode === 'preview' ? '使用预览目录' : '已就绪') : '未检测'}</strong></div></div>
		<div className="skill-adapter-metric"><small>商城</small><strong>{marketplace.length}</strong></div>
        <div className="skill-adapter-metric"><small>已安装</small><strong>{installedCount}</strong></div>
        <div className="skill-adapter-metric"><small>需处理</small><strong className={attentionCount ? 'warning-text' : ''}>{attentionCount}</strong></div>
        <code title={status?.target_root}>{status?.target_root || '--'}</code>
      </section>

      {status?.target_mode === 'preview' ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>尚未检测到正式 Codex Skill 目录。当前同步只会写入 Agent 预览目录，请先配置 <code>HIMIND_CODEX_SKILL_DIR</code>。</span></div> : null}
	  {marketplaceError && view === 'marketplace' ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{marketplaceError}</span></div> : null}

      <section className="skill-workspace">
        <aside className="skill-browser">
          <div className="skill-browser-tools">
            <label className="skill-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索 Skill" /></label>
			<div className="skill-filter" role="tablist" aria-label="Skill 来源">
			  <button role="tab" aria-selected={view === 'marketplace'} className={view === 'marketplace' ? 'active' : ''} onClick={() => setView('marketplace')}>组织商城 <span>{marketplace.length}</span></button>
			  <button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>本机状态 <span>{installedCount}</span></button>
			  <button role="tab" aria-selected={view === 'mine'} className={view === 'mine' ? 'active' : ''} onClick={() => setView('mine')}>我的 Skill <span>{drafts.length}</span></button>
            </div>
          </div>
          <div className="skill-browser-list">
			{view === 'marketplace' ? visibleMarket.map(item => <MarketSkillListItem key={item.skill_id} item={item} installed={installedById.get(item.skill_id)} selected={item.skill_id === selectedMarket?.skill_id} onSelect={setSelectedId} />) : view === 'mine' ? visibleDrafts.map(item => <DraftListItem key={draftKey(item)} item={item} submission={submissions.find(status => status.id === item.dashboard_draft_id)} selected={draftKey(item) === draftKey(selectedDraft)} onSelect={setSelectedId} />) : filteredItems.map(item => <SkillListItem key={item.record.manifest.id} item={item} selected={item.record.manifest.id === selected?.record.manifest.id} onSelect={setSelectedId} />)}
			{(view === 'marketplace' ? !visibleMarket.length : view === 'mine' ? !visibleDrafts.length : !filteredItems.length) ? <EmptyState icon={view === 'marketplace' ? Building2 : BookOpen} title={view === 'marketplace' ? '商城暂无可用 Skill' : view === 'mine' ? '还没有本地 Skill' : '没有匹配的 Skill'} text={view === 'marketplace' ? '仅展示已审核、已签名并正式发布的组织 Skill。' : view === 'mine' ? '从本机创建并完成 Codex 测试后即可提交审核。' : '调整搜索内容或筛选条件。'} /> : null}
          </div>
        </aside>

        <main className="skill-detail">
		  {view === 'marketplace' ? (selectedMarket ? <MarketSkillDetail item={selectedMarket} installed={installedById.get(selectedMarket.skill_id)} busyAction={busyAction} onPlan={async () => { setPlanLoading(true); setPlanError(''); try { setInstallPlan(await onPlanMarketplace(selectedMarket.skill_id)); } catch (reason) { setPlanError(reason instanceof Error ? reason.message : String(reason)); } finally { setPlanLoading(false); } }} planLoading={planLoading} /> : <EmptyState icon={Sparkles} title="选择一个 Skill" text="查看发布版本、能力依赖和签名信息。" />) : view === 'mine' ? (selectedDraft ? <DraftDetail item={selectedDraft} submission={selectedSubmission} busyAction={busyAction} onEdit={() => { setEditingDraft(selectedDraft); setDraftEditorOpen(true); }} onTest={onTestDraft} onConfirm={onConfirmDraft} onSubmit={onSubmitDraft} onOpenDirectory={onOpenDirectory} /> : <EmptyState icon={Sparkles} title="创建本地 Skill" text="完成编辑、预检和 Codex 实测后再提交审核。" />) : (selected ? <SkillDetail item={selected} busyAction={busyAction} onSync={onSyncSkill} onRepair={onRepair} onUninstall={() => setPendingUninstall(selected)} onOpenDirectory={onOpenDirectory} /> : <EmptyState icon={Sparkles} title="选择一个 Skill" text="查看用途、能力依赖、安装状态和托管文件。" />)}
        </main>
      </section>

      {pendingUninstall ? <div className="skill-dialog-backdrop" role="presentation"><div className="skill-dialog" role="dialog" aria-modal="true" aria-labelledby="skill-uninstall-title">
        <div className="skill-dialog-head"><strong id="skill-uninstall-title">卸载 Skill</strong><button className="btn btn-icon" aria-label="关闭" onClick={() => setPendingUninstall(null)}><X size={16} /></button></div>
        <p>将从 Codex 目标目录移除 <strong>{pendingUninstall.record.manifest.name}</strong> 的托管文件，本地 Skill Store 中的源版本会保留。</p>
        {pendingUninstall.modified_files.length ? <div className="skill-dialog-warning"><CircleAlert size={16} />检测到用户修改。请先使用“修复并备份”保留当前文件。</div> : null}
        <div className="skill-dialog-actions"><button className="btn" onClick={() => setPendingUninstall(null)}>取消</button><button className="btn btn-danger" disabled={isBusy || pendingUninstall.modified_files.length > 0} onClick={() => { onUninstall(pendingUninstall.record.manifest.id); setPendingUninstall(null); }}><Trash2 size={15} />确认卸载</button></div>
      </div></div> : null}
	  {draftEditorOpen ? <DraftEditor draft={editingDraft} onClose={() => setDraftEditorOpen(false)} onSave={async input => { await onSaveDraft(input); setDraftEditorOpen(false); }} /> : null}
	  {installPlan || planError ? <InstallPlanDialog plan={installPlan} error={planError} busy={isBusy} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={(optionalIds) => { if (installPlan) onInstallMarketplace(installPlan.skill.skill_id, optionalIds); setInstallPlan(null); }} /> : null}
    </div>
  );
}

function MarketSkillListItem({ item, installed, selected, onSelect }: { item: OrganizationSkillCatalogItem; installed?: CodexSkillStatusItem; selected: boolean; onSelect: (id: string) => void }) {
  const update = installed ? compareSemanticVersions(item.version, installed.record.manifest.version) > 0 : false;
  const label = update ? '可更新' : installed ? '已安装' : '可安装';
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(item.skill_id)}><span className={`skill-state-rail ${update ? 'warn' : installed ? 'success' : 'neutral'}`} /><span className="skill-browser-item-copy"><strong>{item.name}</strong><small>{item.description || item.skill_id}</small></span><span className={`skill-state-label ${update ? 'warn' : installed ? 'success' : 'neutral'}`}>{label}</span></button>;
}

function MarketSkillDetail({ item, installed, busyAction, onPlan, planLoading }: { item: OrganizationSkillCatalogItem; installed?: CodexSkillStatusItem; busyAction: string | null; onPlan: () => void; planLoading: boolean }) {
  const update = installed ? compareSemanticVersions(item.version, installed.record.manifest.version) > 0 : false;
  const current = Boolean(installed) && !update;
  const busy = busyAction === `market:${item.skill_id}`;
  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><span className="skill-detail-mark">{item.name.slice(0, 1).toUpperCase()}</span><div><div className="skill-title-line"><h3>{item.name}</h3><Pill kind="success"><BadgeCheck size={12} />已签名</Pill></div><code>{item.skill_id}</code></div></div><div className="skill-detail-actions"><button className="btn btn-primary" disabled={busy || planLoading || current} onClick={onPlan}><Download className={busy || planLoading ? 'spin' : ''} size={15} />{busy ? '正在安装' : planLoading ? '检查依赖' : update ? '更新' : current ? '已是最新' : '安装'}</button></div></header>
    <p className="skill-detail-description">{item.description || '该 Skill 暂未提供用途说明。'}</p>
    <div className="skill-detail-meta"><div><span>商城版本</span><strong>v{item.version}</strong></div><div><span>本机版本</span><strong>{installed ? `v${installed.record.manifest.version}` : '--'}</strong></div><div><span>发布者</span><strong>{item.author_name || '组织成员'}</strong></div><div><span>风险</span><strong>{riskLabel(item.risk_summary)}</strong></div></div>
    <section className="skill-detail-section"><div className="skill-section-title"><div><ShieldCheck size={16} /><strong>Capability 声明</strong></div><span>{item.capability_ids.length}</span></div><div className="skill-dependency-list">{item.capability_ids.map(id => <div key={id}><span className="status-dot success" /><code>{id}</code><span>已声明</span><strong>安装不扩权</strong></div>)}{!item.capability_ids.length ? <span className="skill-section-empty">没有声明 Capability 依赖</span> : null}</div></section>
	<section className="skill-detail-section"><div className="skill-section-title"><div><Building2 size={16} /><strong>插件依赖</strong></div><span>{item.plugin_dependencies?.length || 0}</span></div><div className="skill-dependency-list">{(item.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><code>{dependency.plugin_id}</code><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `>= v${dependency.min_version}` : '任意版本'}</strong></div>)}{!item.plugin_dependencies?.length ? <span className="skill-section-empty">无需关联插件</span> : null}</div></section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><Building2 size={16} /><strong>发布与信任</strong></div><span>{item.channel}</span></div><div className="skill-release-grid"><div><small>最低 Agent</small><strong>v{item.min_agent_version || '--'}</strong></div><div><small>支持客户端</small><strong>{item.supported_clients.join(', ')}</strong></div><div><small>签名密钥</small><code>{item.signature_key_id}</code></div><div><small>SHA-256</small><code title={item.sha256}>{item.sha256.slice(0, 16)}...</code></div></div>{item.release_notes ? <p className="skill-release-notes">{item.release_notes}</p> : null}</section>
    <footer className="skill-detail-footer"><CheckCircle2 size={14} /><span>下载地址受同源约束，制品在写入 Store 前会完成大小、摘要、签名和文件清单校验。</span><code>{item.file_name}</code></footer>
  </>;
}

function DraftListItem({ item, submission, selected, onSelect }: { item: AuthoringSkillDraft; submission?: SkillSubmissionStatus; selected: boolean; onSelect: (id: string) => void }) {
  const state = draftState(item, submission);
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(draftKey(item))}><span className={`skill-state-rail ${state.tone}`} /><span className="skill-browser-item-copy"><strong>{item.manifest.name}</strong><small>{item.manifest.id} · v{item.manifest.version}</small></span><span className={`skill-state-label ${state.tone}`}>{state.label}</span></button>;
}

function DraftDetail({ item, submission, busyAction, onEdit, onTest, onConfirm, onSubmit, onOpenDirectory }: { item: AuthoringSkillDraft; submission?: SkillSubmissionStatus; busyAction: string | null; onEdit: () => void; onTest: (id: string, version: string) => void; onConfirm: (id: string, version: string) => void; onSubmit: (id: string, version: string) => void; onOpenDirectory: (path: string) => void }) {
  const manifest = item.manifest;
  const state = draftState(item, submission);
  const busy = Boolean(busyAction?.endsWith(manifest.id));
  const canRevise = !item.submitted_at || submission?.status === 'changes_requested' || submission?.status === 'rejected';
  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><span className="skill-detail-mark">{manifest.name.slice(0, 1).toUpperCase()}</span><div><div className="skill-title-line"><h3>{manifest.name}</h3><Pill kind={state.tone === 'success' ? 'success' : state.tone === 'warn' ? 'warn' : 'danger'}>{state.label}</Pill></div><code>{manifest.id} · v{manifest.version}</code></div></div><div className="skill-detail-actions">
      {canRevise ? <button className="btn" disabled={busy} onClick={onEdit}><Edit3 size={15} />编辑</button> : null}
      {canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onTest(manifest.id, manifest.version)}><Play className={busyAction === `test:${manifest.id}` ? 'spin' : ''} size={15} />部署测试</button> : null}
      {item.tested_at && !item.confirmed_at && canRevise ? <button className="btn" disabled={busy} onClick={() => onConfirm(manifest.id, manifest.version)}><CheckCircle2 size={15} />确认通过</button> : null}
      {item.confirmed_at && canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onSubmit(manifest.id, manifest.version)}><Send size={15} />提交审核</button> : null}
    </div></header>
    <p className="skill-detail-description">{manifest.description || '暂无用途说明。'}</p>
	{submission?.review_note ? <div className={`skill-detail-notice ${submission.status === 'approved' ? 'modified' : ''}`}><CircleAlert size={16} /><div><strong>审核意见</strong><span>{normalizeReviewNote(submission.review_note)}</span></div></div> : null}
    <div className="skill-authoring-pipeline"><DraftStage complete label="内容已保存" /><DraftStage complete={Boolean(item.tested_at)} label="Codex 已部署" /><DraftStage complete={Boolean(item.confirmed_at)} label="测试已确认" /><DraftStage complete={Boolean(item.submitted_at)} label="已提交审核" /></div>
    <div className="skill-detail-meta"><div><span>候选版本</span><strong>v{manifest.version}</strong></div><div><span>测试时间</span><strong>{formatAuthoringTime(item.tested_at)}</strong></div><div><span>插件依赖</span><strong>{manifest.plugin_dependencies?.length || 0}</strong></div><div><span>风险</span><strong>{riskLabel(manifest.risk_summary)}</strong></div></div>
    <section className="skill-detail-section"><div className="skill-section-title"><div><ShieldCheck size={16} /><strong>插件依赖</strong></div><span>{manifest.plugin_dependencies?.length || 0}</span></div><div className="skill-dependency-list">{(manifest.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><code>{dependency.plugin_id}</code><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `>= v${dependency.min_version}` : '任意版本'}</strong></div>)}{!manifest.plugin_dependencies?.length ? <span className="skill-section-empty">无需关联插件</span> : null}</div></section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><BookOpen size={16} /><strong>候选制品</strong></div><span>SHA-256</span></div><div className="skill-artifact-local"><code title={item.candidate_sha256}>{item.candidate_sha256}</code><button className="text-action" onClick={() => onOpenDirectory(item.candidate_path)}><FolderOpen size={14} />打开位置</button></div>{item.dashboard_draft_id ? <div className="skill-submission-id"><span>审核记录</span><code>{item.dashboard_draft_id}</code></div> : null}</section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><BookOpen size={16} /><strong>SKILL.md</strong></div><span>{item.readme.length} 字符</span></div><pre className="skill-draft-readme">{item.readme}</pre></section>
  </>;
}

function DraftStage({ complete, label }: { complete: boolean; label: string }) { return <div className={complete ? 'complete' : ''}><span>{complete ? <CheckCircle2 size={13} /> : null}</span><strong>{label}</strong></div>; }

function DraftEditor({ draft, onClose, onSave }: { draft: AuthoringSkillDraft | null; onClose: () => void; onSave: (input: AuthoringSkillDraftInput) => Promise<void> }) {
  const [input, setInput] = useState<AuthoringSkillDraftInput>(() => draftInputFrom(draft));
  const [capabilityText, setCapabilityText] = useState(() => (draft?.manifest.capabilities || []).map(item => `${item.id}|${item.min_version || ''}|${item.required ? 'required' : 'optional'}|${item.provider || ''}`).join('\n'));
  const [pluginText, setPluginText] = useState(() => (draft?.manifest.plugin_dependencies || []).map(item => `${item.plugin_id}|${item.min_version || ''}|${item.required ? 'required' : 'optional'}`).join('\n'));
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState('');
  const change = (field: keyof AuthoringSkillDraftInput, value: string) => setInput(current => ({ ...current, [field]: value }));
  async function save() {
    setSaving(true); setSaveError('');
    try { await onSave({ ...input, capabilities: parseCapabilities(capabilityText), plugin_dependencies: parsePluginDependencies(pluginText) }); }
    catch (reason) { setSaveError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setSaving(false); }
  }
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-editor-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>{draft ? '编辑 Skill' : '新建 Skill'}</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>{saveError ? <div className="skill-dialog-warning"><CircleAlert size={16} />{saveError}</div> : null}<div className="skill-editor-grid">
    <label><span>名称</span><input value={input.name} placeholder="例如：项目交付检查" onChange={event => change('name', event.target.value)} /></label><label className="wide"><span>用途说明</span><textarea rows={2} value={input.description} placeholder="说明这个 Skill 适合在什么情况下使用" onChange={event => change('description', event.target.value)} /></label><label className="wide"><span>Skill 指令</span><textarea className="skill-editor-readme" rows={15} value={input.readme} onChange={event => change('readme', event.target.value)} /></label>
    <details className="wide skill-editor-advanced"><summary>高级设置</summary><div className="skill-editor-grid"><label><span>Skill ID</span><input value={input.id} disabled={Boolean(draft)} onChange={event => change('id', event.target.value)} /></label><label><span>版本</span><input value={input.version} disabled={Boolean(draft)} onChange={event => change('version', event.target.value)} /></label><label><span>最低 Agent</span><input value={input.min_agent_version} onChange={event => change('min_agent_version', event.target.value)} /></label><label><span>风险等级</span><select value={input.risk_summary} onChange={event => change('risk_summary', event.target.value)}><option value="read_only">只读</option><option value="local_action">本地操作</option><option value="network_write">网络写入</option><option value="approval_required">需要审批</option></select></label><label className="wide"><span>Capability 依赖</span><textarea rows={4} value={capabilityText} onChange={event => setCapabilityText(event.target.value)} placeholder="system.health|1.0.0|required|agent" /></label><label className="wide"><span>插件依赖</span><textarea rows={4} value={pluginText} onChange={event => setPluginText(event.target.value)} placeholder="com.himind.project-delivery|1.0.0|required" /></label></div></details>
  </div><div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={saving || !input.name.trim() || !input.readme.trim()} onClick={save}>{saving ? '正在保存' : '保存草稿'}</button></div></div></div>;
}

function InstallPlanDialog({ plan, error, busy, onClose, onInstall }: { plan: SkillInstallPlan | null; error: string; busy: boolean; onClose: () => void; onInstall: (optionalIds: string[]) => void }) {
  const [optionalIds, setOptionalIds] = useState<string[]>([]);
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-plan-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>安装计划</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>{error ? <div className="skill-dialog-warning"><CircleAlert size={16} />{error}</div> : null}{plan ? <><div className="skill-plan-summary"><strong>{plan.skill.name} v{plan.skill.version}</strong><span>{plan.ready ? '依赖检查通过' : plan.blocked_reasons.join('；')}</span></div><div className="skill-plan-actions">{plan.plugin_actions.map(action => <label key={action.plugin_id} className={action.action === 'blocked' || action.action === 'unavailable' ? 'blocked' : ''}><input type="checkbox" checked={action.required || optionalIds.includes(action.plugin_id)} disabled={action.required || !['install', 'update'].includes(action.action)} onChange={event => setOptionalIds(current => event.target.checked ? [...current, action.plugin_id] : current.filter(id => id !== action.plugin_id))} /><span><strong>{action.plugin_id}</strong><small>{installActionLabel(action.action)} · {action.reason}</small></span><code>{action.target_version ? `v${action.target_version}` : '--'}</code></label>)}</div></> : null}<div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!plan?.ready || busy} onClick={() => onInstall(optionalIds)}><Download size={15} />确认安装</button></div></div></div>;
}

function SkillListItem({ item, selected, onSelect }: { item: CodexSkillStatusItem; selected: boolean; onSelect: (id: string) => void }) {
  const manifest = item.record.manifest;
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(manifest.id)}><span className={`skill-state-rail ${stateTone(item.client_state)}`} /><span className="skill-browser-item-copy"><strong>{manifest.name}</strong><small>{manifest.description || manifest.id}</small></span><span className={`skill-state-label ${stateTone(item.client_state)}`}>{clientStateLabel(item.client_state)}</span></button>;
}

function SkillDetail({ item, busyAction, onSync, onRepair, onUninstall, onOpenDirectory }: { item: CodexSkillStatusItem; busyAction: string | null; onSync: (id: string) => void; onRepair: (id: string) => void; onUninstall: () => void; onOpenDirectory: (path: string) => void }) {
  const manifest = item.record.manifest;
  const actionBusy = Boolean(busyAction?.endsWith(manifest.id));
  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><span className="skill-detail-mark">{manifest.name.slice(0, 1).toUpperCase()}</span><div><div className="skill-title-line"><h3>{manifest.name}</h3><Pill kind={statePill(item.client_state)}>{clientStateLabel(item.client_state)}</Pill></div><code>{manifest.id}</code></div></div><div className="skill-detail-actions">
      {item.available_actions.includes('install') || item.available_actions.includes('update') ? <button className="btn btn-primary" disabled={actionBusy} onClick={() => onSync(manifest.id)}><RefreshCw className={actionBusy ? 'spin' : ''} size={15} />{item.client_state === 'outdated' ? '更新' : '安装'}</button> : null}
      {item.available_actions.includes('repair') ? <button className="btn" disabled={actionBusy} onClick={() => onRepair(manifest.id)}><Wrench size={15} />{item.client_state === 'modified' ? '修复并备份' : '重新同步'}</button> : null}
      {item.available_actions.includes('uninstall') ? <button className="btn btn-danger-quiet" disabled={actionBusy} onClick={onUninstall}><Trash2 size={15} />卸载</button> : null}
    </div></header>
    <p className="skill-detail-description">{manifest.description || '该 Skill 暂未提供用途说明。'}</p>
    {item.readiness.state !== 'ready' ? <div className="skill-detail-notice"><CircleAlert size={16} /><div><strong>{item.readiness.state === 'blocked' ? '当前不可安装' : '部分能力不可用'}</strong><span>{item.readiness.reasons[0] || '请检查 Capability 依赖。'}</span></div></div> : null}
    {item.client_state === 'modified' ? <div className="skill-detail-notice modified"><Wrench size={16} /><div><strong>Codex 目录中的托管文件已被修改</strong><span>修复时会先保留一份用户备份，再从 Skill Store 重新渲染。</span></div></div> : null}
    <div className="skill-detail-meta"><div><span>可用版本</span><strong>v{item.available_version || manifest.version}</strong></div><div><span>已安装版本</span><strong>{item.installed_version ? `v${item.installed_version}` : '--'}</strong></div><div><span>来源</span><strong>{scopeLabel(manifest.scope)}</strong></div><div><span>风险</span><strong>{riskLabel(manifest.risk_summary)}</strong></div></div>
    <section className="skill-detail-section"><div className="skill-section-title"><div><ShieldCheck size={16} /><strong>Capability 依赖</strong></div><span>{item.readiness.dependencies.length}</span></div><div className="skill-dependency-list">
      {item.readiness.dependencies.map(dependency => <div key={dependency.id}><span className={`status-dot ${dependency.state === 'ready' ? 'success' : 'danger'}`} /><code>{dependency.id}</code><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.capability_version ? `v${dependency.capability_version}` : dependency.reason || '不可用'}</strong></div>)}
      {!item.readiness.dependencies.length ? <span className="skill-section-empty">没有声明 Capability 依赖</span> : null}
    </div></section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><BookOpen size={16} /><strong>托管文件</strong></div><span>{item.managed_files.length}</span></div>{item.modified_files.length ? <div className="skill-modified-files">{item.modified_files.map(path => <code key={path}>{path}</code>)}</div> : null}<div className="skill-file-summary"><div><small>最近同步</small><strong>{formatSyncedAt(item.last_synced_at)}</strong></div><button className="text-action" disabled={!item.rendered} onClick={() => onOpenDirectory(item.rendered_root)}><FolderOpen size={14} />打开渲染目录</button></div></section>
    <footer className="skill-detail-footer"><CheckCircle2 size={14} /><span>Skill 只编排已授权 Capability，不会因安装而获得额外权限。</span><code title={item.record.version_root}>{item.record.version_root}</code></footer>
  </>;
}

function clientStateLabel(state: CodexSkillStatusItem['client_state']) {
  const labels: Record<CodexSkillStatusItem['client_state'], string> = { not_installed: '未安装', installed: '已安装', outdated: '有更新', modified: '已修改', blocked: '不可用', unsupported: '不兼容', failed: '失败' };
  return labels[state] || state;
}

function stateTone(state: CodexSkillStatusItem['client_state']) { if (state === 'installed') return 'success'; if (state === 'outdated' || state === 'modified') return 'warn'; if (state === 'not_installed') return 'neutral'; return 'danger'; }
function statePill(state: CodexSkillStatusItem['client_state']): 'success' | 'warn' | 'danger' { if (state === 'installed') return 'success'; if (state === 'outdated' || state === 'modified' || state === 'not_installed') return 'warn'; return 'danger'; }
function targetModeLabel(mode?: CodexSkillStatusResponse['target_mode']) { if (mode === 'configured') return '已配置目标'; if (mode === 'detected') return '已检测本机目录'; if (mode === 'preview') return '预览模式'; return '等待检测'; }
function scopeLabel(scope: string) { if (scope === 'builtin') return '系统内置'; if (scope === 'organization') return '组织商城'; if (scope === 'user') return '我的 Skill'; return scope || '--'; }
function riskLabel(value?: string) { return ({ read_only: '只读', local_action: '本地操作', network_write: '网络写入', approval_required: '需要审批' } as Record<string, string>)[value || ''] || (value ? '未分类风险' : '未声明'); }
function normalizeReviewNote(value: string) { return /\?{3,}|�/.test(value) ? '审核意见数据无法正常解码，请重新提交中文审核意见。' : value; }
function formatSyncedAt(value?: string | null) { if (!value) return '尚未同步'; const milliseconds = Number.parseInt(value.split('-')[0], 10); return Number.isFinite(milliseconds) ? new Date(milliseconds).toLocaleString('zh-CN', { hour12: false }) : value; }
function draftKey(draft?: AuthoringSkillDraft) { return draft ? `${draft.manifest.id}@${draft.manifest.version}` : ''; }
function draftState(draft: AuthoringSkillDraft, submission?: SkillSubmissionStatus): { label: string; tone: 'success' | 'warn' | 'neutral' | 'danger' } { if (submission?.status === 'approved') return { label: '已上架', tone: 'success' }; if (submission?.status === 'changes_requested') return { label: '需修改', tone: 'warn' }; if (submission?.status === 'rejected') return { label: '已拒绝', tone: 'danger' }; if (draft.submitted_at) return { label: '审核中', tone: 'success' }; if (draft.confirmed_at) return { label: '可提交', tone: 'success' }; if (draft.tested_at) return { label: '待确认', tone: 'warn' }; return { label: '草稿', tone: 'neutral' }; }
function formatAuthoringTime(value?: string | null) { if (!value) return '--'; const time = Number.parseInt(value, 10); return Number.isFinite(time) ? new Date(time).toLocaleString('zh-CN', { hour12: false }) : value; }
function draftInputFrom(draft: AuthoringSkillDraft | null): AuthoringSkillDraftInput { return draft ? { id: draft.manifest.id, name: draft.manifest.name, version: draft.manifest.version, description: draft.manifest.description || '', min_agent_version: draft.manifest.min_agent_version || '0.2.0', supported_clients: draft.manifest.supported_clients || ['codex'], capabilities: draft.manifest.capabilities || [], plugin_dependencies: draft.manifest.plugin_dependencies || [], risk_summary: draft.manifest.risk_summary || 'read_only', readme: draft.readme } : { id: `com.himind.skill.custom-${Date.now().toString(36)}`, name: '', version: '0.1.0', description: '', min_agent_version: '0.2.0', supported_clients: ['codex'], capabilities: [{ id: 'system.health', required: true, min_version: '1.0.0', provider: 'agent' }], plugin_dependencies: [], risk_summary: 'read_only', readme: '# Skill 名称\n\n## 何时使用\n\n说明适合触发这个 Skill 的场景。\n\n## 需要的信息\n\n列出执行前需要向用户确认的信息。\n\n## 执行步骤\n\n1. 检查当前上下文。\n2. 只调用已声明的 Capability。\n3. 汇总结果和需要用户处理的问题。\n\n## 输出要求\n\n说明希望 AI 返回的内容和格式。\n\n## 禁止事项\n\n- 不绕过 Capability Gateway。\n- 不展示账号、Cookie、Token 或其他凭据。\n' }; }
function parseCapabilities(value: string): AuthoringSkillDraftInput['capabilities'] { return value.split(/\r?\n/).map(line => line.trim()).filter(Boolean).map(line => { const [id, minVersion, mode, provider] = line.split('|').map(part => part.trim()); return { id, min_version: minVersion || undefined, required: mode !== 'optional', provider: provider || undefined }; }); }
function parsePluginDependencies(value: string): AuthoringSkillDraftInput['plugin_dependencies'] { return value.split(/\r?\n/).map(line => line.trim()).filter(Boolean).map(line => { const [pluginId, minVersion, mode] = line.split('|').map(part => part.trim()); return { plugin_id: pluginId, min_version: minVersion || undefined, required: mode !== 'optional' }; }); }
function installActionLabel(action: string) { return ({ satisfied: '已满足', install: '将安装', update: '将升级', blocked: '已阻止', unavailable: '不可用' } as Record<string, string>)[action] || action; }
function compareSemanticVersions(left: string, right: string) {
  const parse = (value: string) => value.split(/[.+-]/).slice(0, 3).map(part => Number.parseInt(part, 10) || 0);
  const a = parse(left);
  const b = parse(right);
  for (let index = 0; index < 3; index += 1) {
    if ((a[index] || 0) !== (b[index] || 0)) return (a[index] || 0) - (b[index] || 0);
  }
  return 0;
}
