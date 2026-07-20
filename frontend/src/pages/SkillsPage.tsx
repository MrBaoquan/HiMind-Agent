import { useMemo, useState } from 'react';
import { BookOpen, CheckCircle2, CircleAlert, FolderOpen, RefreshCw, Search, ShieldCheck, Trash2 } from 'lucide-react';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import type { CodexSkillStatusItem, CodexSkillStatusResponse, SkillCatalogItem, SkillCatalogResponse } from '../services/agentApi';

type SkillsPageProps = {
  catalog: SkillCatalogResponse | null;
  status: CodexSkillStatusResponse | null;
  error: string | null;
  onRefresh: () => void;
  onSync: () => void;
  onUninstall: (skillId: string) => void;
  onOpenDirectory: (path: string) => void;
};

type ViewKey = 'target' | 'catalog';

export function SkillsPage({
  catalog,
  status,
  error,
  onRefresh,
  onSync,
  onUninstall,
  onOpenDirectory,
}: SkillsPageProps) {
  const [view, setView] = useState<ViewKey>('target');
  const [query, setQuery] = useState('');

  const catalogItems = catalog?.items || [];
  const targetItems = status?.items || [];
  const normalizedQuery = query.trim().toLowerCase();

  const filteredCatalog = useMemo(
    () => filterCatalogItems(catalogItems, normalizedQuery),
    [catalogItems, normalizedQuery],
  );
  const filteredTarget = useMemo(
    () => filterStatusItems(targetItems, normalizedQuery),
    [targetItems, normalizedQuery],
  );

  const readyCount = catalogItems.filter(item => item.readiness.state === 'ready').length;
  const degradedCount = catalogItems.filter(item => item.readiness.state === 'degraded').length;
  const blockedCount = catalogItems.filter(item => item.readiness.state === 'blocked').length;
  const renderedCount = targetItems.filter(item => item.rendered).length;

  const activePath = view === 'target' ? status?.target_root : catalog?.store_root;
  const activeLabel = view === 'target' ? 'Codex 目标目录' : 'Skill Store';
  const activeCount = view === 'target' ? filteredTarget.length : filteredCatalog.length;

  if (!catalog && !status && !error) {
    return <div className="page-loading"><span className="spinner" />正在读取 Skill 数据</div>;
  }

  return (
    <div className="skill-page">
      <PageHeader
        title="技能"
        description="管理本机 Skill Store，并将可用 Skill 同步到 Codex 目标目录。"
        actions={
          <>
            <button className="btn btn-primary" onClick={onSync}>
              <RefreshCw size={16} />
              同步到 Codex
            </button>
            <button className="btn" onClick={() => activePath && onOpenDirectory(activePath)} disabled={!activePath}>
              <FolderOpen size={16} />
              打开{view === 'target' ? '目标' : '仓库'}
            </button>
            <button className="btn btn-icon" title="刷新 Skill 数据" aria-label="刷新 Skill 数据" onClick={onRefresh}>
              <RefreshCw size={16} />
            </button>
          </>
        }
      />

      {error ? (
        <div className="blocker">
          <CircleAlert size={18} />
          <div>
            <strong>Skill 数据读取失败</strong>
            <span>{error}</span>
          </div>
        </div>
      ) : null}

      <div className="skill-summary-strip">
        <div>
          <span className="skill-summary-icon blue"><BookOpen size={16} /></span>
          <span>
            <small>Skill Store</small>
            <strong>{catalogItems.length}</strong>
          </span>
        </div>
        <div>
          <span className="skill-summary-icon green"><CheckCircle2 size={16} /></span>
          <span>
            <small>已渲染到 Codex</small>
            <strong>{renderedCount}</strong>
          </span>
        </div>
        <div>
          <span className="skill-summary-icon amber"><ShieldCheck size={16} /></span>
          <span>
            <small>可直接同步</small>
            <strong>{readyCount}</strong>
          </span>
        </div>
        <div>
          <span className={`status-dot ${status?.target_configured ? 'success' : 'danger'}`} />
          <span>
            <small>Codex 目标</small>
            <strong>{status ? (status.target_configured ? '已配置' : '预览路径') : '未读取'}</strong>
          </span>
        </div>
      </div>

      <div className="skill-toolbar">
        <div className="skill-tabs">
          <button type="button" className={view === 'target' ? 'active' : ''} onClick={() => setView('target')}>
            Codex 目标 <span>{targetItems.length}</span>
          </button>
          <button type="button" className={view === 'catalog' ? 'active' : ''} onClick={() => setView('catalog')}>
            Skill Store <span>{catalogItems.length}</span>
          </button>
        </div>
        <label className="skill-search">
          <Search size={15} />
          <input
            value={query}
            onChange={event => setQuery(event.target.value)}
            placeholder="按名称、ID 或说明筛选"
          />
        </label>
      </div>

      <section className="card skill-panel">
        <div className="card-header">
          <span>{activeLabel}</span>
          <span className="section-count">{activeCount}</span>
        </div>
        <div className="card-body skill-panel-body">
          {view === 'target' ? (
            <>
              <div className="skill-panel-meta">
                <div>
                  <span>目标根目录</span>
                  <code title={status?.target_root}>{status?.target_root || '--'}</code>
                </div>
                <div>
                  <span>来源</span>
                  <strong>{status?.target_source || '--'}</strong>
                </div>
                <div>
                  <span>已渲染</span>
                  <strong>{renderedCount}</strong>
                </div>
                <div>
                  <span>无效渲染</span>
                  <strong className={targetItems.some(item => item.rendered && !item.rendered_valid) ? 'warning-text' : ''}>
                    {targetItems.filter(item => item.rendered && !item.rendered_valid).length}
                  </strong>
                </div>
              </div>
              <div className="skill-list">
                {filteredTarget.map(item => (
                  <SkillStatusRow
                    key={item.record.manifest.id}
                    item={item}
                    onUninstall={onUninstall}
                  />
                ))}
                {filteredTarget.length === 0 ? (
                  <EmptyState icon={BookOpen} title="没有匹配的 Skill" text="尝试更换筛选关键字或先执行一次同步。" />
                ) : null}
              </div>
            </>
          ) : (
            <>
              <div className="skill-panel-meta">
                <div>
                  <span>Store 根目录</span>
                  <code title={catalog?.store_root}>{catalog?.store_root || '--'}</code>
                </div>
                <div>
                  <span>Agent 版本</span>
                  <strong>{catalog?.agent_version || '--'}</strong>
                </div>
                <div>
                  <span>可直接使用</span>
                  <strong>{readyCount}</strong>
                </div>
                <div>
                  <span>被阻止</span>
                  <strong className={blockedCount ? 'warning-text' : ''}>{blockedCount}</strong>
                </div>
              </div>
              <div className="skill-list">
                {filteredCatalog.map(item => (
                  <SkillCatalogRow
                    key={item.record.manifest.id}
                    item={item}
                  />
                ))}
                {filteredCatalog.length === 0 ? (
                  <EmptyState icon={BookOpen} title="没有匹配的 Skill" text={catalogItems.length === 0 ? 'Skill Store 目前没有可用内容。' : '尝试更换筛选关键字。'} />
                ) : null}
              </div>
            </>
          )}
        </div>
      </section>

      <div className="skill-footer-note">
        <span className="status-dot success" />
        <span>Skill 只负责说明和编排，真正的执行仍然走 Capability Gateway。</span>
      </div>
    </div>
  );
}

function SkillStatusRow({ item, onUninstall }: { item: CodexSkillStatusItem; onUninstall: (skillId: string) => void }) {
  const manifest = item.record.manifest;
  const readinessKind = pillKind(item.readiness.state);
  return (
    <article className="skill-row">
      <div className="skill-row-head">
        <span className="skill-row-mark">{manifest.name.slice(0, 1).toUpperCase()}</span>
        <div className="skill-row-title">
          <strong>{manifest.name}</strong>
          <code>{manifest.id}</code>
        </div>
        <Pill kind={readinessKind}>{readinessLabel(item.readiness.state)}</Pill>
        <Pill kind={item.rendered_valid ? 'success' : item.rendered ? 'warn' : 'danger'}>{item.rendered ? (item.rendered_valid ? '已渲染' : '已修改') : '未同步'}</Pill>
      </div>
      <p className="skill-row-description">{manifest.description || manifest.risk_summary || '暂无说明。'}</p>
      <div className="skill-row-meta">
        <span>版本 <strong>v{manifest.version}</strong></span>
        <span>作用域 <strong>{scopeLabel(manifest.scope)}</strong></span>
        <span>客户端 <strong>{formatClients(manifest.supported_clients)}</strong></span>
        <span>依赖 <strong>{manifest.capabilities?.length || 0}</strong></span>
      </div>
      {item.readiness.reasons.length ? <div className="skill-row-reason">{item.readiness.reasons[0]}</div> : null}
      <div className="skill-row-footer">
        <span>
          <code title={item.rendered_root}>{item.rendered_root}</code>
        </span>
        <div className="actions-row">
          <button className="btn btn-danger-quiet" disabled={!item.rendered} title={item.rendered ? '从 Codex 目标目录卸载' : '未同步到 Codex'} onClick={() => onUninstall(manifest.id)}>
            <Trash2 size={15} />
            卸载
          </button>
        </div>
      </div>
    </article>
  );
}

function SkillCatalogRow({ item }: { item: SkillCatalogItem }) {
  const manifest = item.record.manifest;
  const readinessKind = pillKind(item.readiness.state);
  return (
    <article className="skill-row">
      <div className="skill-row-head">
        <span className="skill-row-mark">{manifest.name.slice(0, 1).toUpperCase()}</span>
        <div className="skill-row-title">
          <strong>{manifest.name}</strong>
          <code>{manifest.id}</code>
        </div>
        <Pill kind={readinessKind}>{readinessLabel(item.readiness.state)}</Pill>
        <Pill kind={item.record.current ? 'success' : 'warn'}>{item.record.current ? '当前版本' : '历史版本'}</Pill>
      </div>
      <p className="skill-row-description">{manifest.description || manifest.risk_summary || '暂无说明。'}</p>
      <div className="skill-row-meta">
        <span>版本 <strong>v{manifest.version}</strong></span>
        <span>作用域 <strong>{scopeLabel(manifest.scope)}</strong></span>
        <span>客户端 <strong>{formatClients(manifest.supported_clients)}</strong></span>
        <span>依赖 <strong>{manifest.capabilities?.length || 0}</strong></span>
      </div>
      {item.readiness.reasons.length ? <div className="skill-row-reason">{item.readiness.reasons[0]}</div> : null}
      <div className="skill-row-footer">
        <span>
          <code title={item.record.version_root}>{item.record.version_root}</code>
        </span>
        <div className="skill-row-foot-tags">
          <Tags items={manifest.supported_clients || []} />
        </div>
      </div>
    </article>
  );
}

function filterCatalogItems(items: SkillCatalogItem[], query: string) {
  if (!query) return items;
  return items.filter(item => skillSearchText(item.record.manifest).includes(query));
}

function filterStatusItems(items: CodexSkillStatusItem[], query: string) {
  if (!query) return items;
  return items.filter(item => skillSearchText(item.record.manifest).includes(query));
}

function skillSearchText(manifest: SkillCatalogItem['record']['manifest']) {
  return [
    manifest.id,
    manifest.name,
    manifest.description,
    manifest.risk_summary,
    manifest.scope,
    ...(manifest.supported_clients || []),
    ...(manifest.capabilities || []).map(item => item.id),
  ]
    .join(' ')
    .toLowerCase();
}

function scopeLabel(scope: string) {
  if (scope === 'builtin') return '系统内置';
  if (scope === 'organization') return '组织';
  if (scope === 'user') return '用户';
  return scope || '--';
}

function readinessLabel(state: string) {
  if (state === 'ready') return '可用';
  if (state === 'degraded') return '降级';
  if (state === 'blocked') return '阻止';
  if (state === 'denied') return '拒绝';
  if (state === 'approval_required') return '待审批';
  return state || '--';
}

function pillKind(state: string): 'success' | 'warn' | 'danger' {
  if (state === 'ready') return 'success';
  if (state === 'degraded' || state === 'approval_required') return 'warn';
  return 'danger';
}

function formatClients(items?: string[]) {
  if (!items?.length) return '--';
  return items.join(', ');
}
