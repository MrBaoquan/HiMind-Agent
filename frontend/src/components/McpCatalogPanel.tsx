import { useEffect, useState } from 'react';
import { Check, CircleAlert, FolderOpen, Pencil, Plus, RefreshCw, Search, X } from 'lucide-react';
import { BusyIndicator } from './BusyIndicator';
import { Pill } from './Common';
import { McpConnectionForm } from './McpConnectionForm';
import type { McpManager } from './useMcpManager';
import {
  CATALOG_PAGE_SIZE,
  CATALOG_PAGE_STEP,
  CATALOG_RISK_NOTE,
  catalogNote,
  groupCatalog,
  installBlocker,
  installsDisabled,
  needsAcknowledgement,
  runtimeNote,
  sourceBreakdown,
  trustLabel,
  trustPillKind,
  type CatalogEntry,
  type CatalogInput,
} from '../pages/mcpCatalogView';

/**
 * 市场里的「MCP 工具」页签：挑一条目录里的工具装进来。
 *
 * 装完写进的是同一份 `himind-ai-mcp.json`（ADR 0006），所以这里只负责「获得」；
 * 启停、编辑、删除都在「我的能力」。分组与风险提示按主流做法收敛：
 * 精选置顶，第三方合成一组只铺一屏，风险只提一次（组级），不贴到每张卡上。
 */
export function McpCatalogPanel({ mcp, query, onManage }: {
  mcp: McpManager;
  /** 市场页只有一个搜索框，关键词从这里进来。 */
  query: string;
  /** 「我的能力」里的 MCP 页签。 */
  onManage: () => void;
}) {
  const [limit, setLimit] = useState(CATALOG_PAGE_SIZE);
  // 换关键词就回到一屏，避免「上一次点开的更多」把搜索结果铺得看不到头。
  useEffect(() => { setLimit(CATALOG_PAGE_SIZE); }, [query]);

  const { groups, hidden } = groupCatalog(mcp.catalog.entries, query, limit);
  const sourcesBroken = mcp.catalog.sources.some(source => source.error);
  const installHint = mcp.installing
    ? installBlocker(mcp.installing, mcp.installValues, mcp.installChecked, mcp.acknowledgedSource(mcp.installing.source_id), mcp.requirements)
    : '';
  const installKey = mcp.installing ? `install:${mcp.installing.source_id}/${mcp.installing.id}` : '';

  return (
    <div className="mcp-catalog-panel" ref={mcp.panelRef}>
      <header className="mcp-catalog-head">
        <div>
          <h3>MCP 工具</h3>
          <span className={`builtin-ai-catalog-note${mcp.catalog.stale ? ' stale' : ''}${sourcesBroken ? ' error' : ''}`} title={sourceBreakdown(mcp.catalog)}>{catalogNote(mcp.catalog)}</span>
        </div>
        <div className="mcp-heading-actions">
          {mcp.servers.length ? <button type="button" className="btn" onClick={onManage}>已添加 {mcp.servers.length} 条 · 去管理</button> : null}
          <button type="button" className="btn" disabled={mcp.refreshing} onClick={() => void mcp.refreshCatalog()}>
            {mcp.refreshing ? <BusyIndicator size={14} /> : <RefreshCw size={14} />}{mcp.refreshing ? '正在刷新' : '刷新目录'}
          </button>
          <button type="button" className="btn" disabled={Boolean(mcp.busy)} onClick={() => { mcp.closeInstall(); mcp.beginAdd(); }}><Plus size={14} />自定义连接</button>
        </div>
      </header>

      <McpConnectionForm mcp={mcp} />

      {mcp.installing ? (
        <section className="builtin-ai-catalog-install" aria-label={`安装 ${mcp.installing.title}`}>
          <div className="builtin-ai-catalog-install-head">
            <div>
              <h4>安装「{mcp.installing.title}」</h4>
              <span title={mcp.installing.command_preview}>{mcp.installing.source_label} · {trustLabel(mcp.installing.trust)}{mcp.installing.version ? ` · v${mcp.installing.version}` : ''}</span>
            </div>
            <button type="button" className="btn btn-icon" title="取消安装" aria-label="取消安装" onClick={mcp.closeInstall}><X size={15} /></button>
          </div>
          <div className="builtin-ai-mcp-form-grid">
            <label className="field-group">
              <span className="field-label">名称<span className="builtin-ai-catalog-optional">留空就用原名</span></span>
              <input value={mcp.installName} placeholder={mcp.installing.title} onChange={event => mcp.setInstallName(event.target.value)} />
            </label>
            {mcp.installing.inputs.map(input => <InstallInput key={input.key} input={input} mcp={mcp} />)}
          </div>
          {installsDisabled(mcp.installing.trust) ? (
            needsAcknowledgement(mcp.installing.trust, mcp.acknowledgedSource(mcp.installing.source_id)) ? (
              <label className="builtin-ai-mcp-check">
                <input type="checkbox" checked={mcp.installChecked} onChange={event => mcp.setInstallChecked(event.target.checked)} />
                <span><strong>我确认「{mcp.installing.source_label}」是可信来源</strong><small>本地 MCP 会执行任意代码且来源未验证；装好后默认停用，需在「我的能力」手动启用。</small></span>
              </label>
            ) : (
              <p className="builtin-ai-catalog-install-trust">来自未验证来源「{mcp.installing.source_label}」，装好后默认停用，需要手动启用。</p>
            )
          ) : null}
          {installHint ? <p className="builtin-ai-catalog-install-hint">{installHint}</p> : null}
          <div className="builtin-ai-mcp-form-actions">
            <button type="button" className="btn" onClick={mcp.closeInstall}>取消</button>
            <button type="button" className="btn btn-primary" disabled={Boolean(mcp.busy)} onClick={() => void mcp.installEntry()}>
              {mcp.busy === installKey ? <BusyIndicator size={15} /> : <Plus size={15} />}{mcp.busy === installKey ? '正在安装' : '安装'}
            </button>
          </div>
        </section>
      ) : null}

      {/* 刷新目录、安装这些动作都在这一页发生，失败和成功必须在这一页说，
          而不是只写进别的入口的状态里。安装面板自己的前置提示已经在面板里写了，
          这里不重复同一句话。 */}
      {mcp.error && !mcp.draft && !installHint ? (
        <div className="builtin-ai-extension-feedback error" role="alert">
          <CircleAlert size={15} /><span>{mcp.error}</span>
        </div>
      ) : null}
      {mcp.notice ? <div className="builtin-ai-extension-feedback success" role="status"><Check size={15} /><span>{mcp.notice}</span></div> : null}

      {groups.map(group => (
        <section className="mcp-catalog-group" key={group.id}>
          <div className="mcp-catalog-group-head">
            <h4>{group.label}</h4>
            <span>{group.id === 'curated' ? '随 HiMind Agent 内置，装完即可用' : `${group.entries.length + (hidden && group.id === 'other' ? hidden : 0)} 条`}</span>
          </div>
          {group.id === 'other' ? <p className="mcp-catalog-risk"><CircleAlert size={13} /><span>{CATALOG_RISK_NOTE}</span></p> : null}
          <div className="builtin-ai-catalog-grid">
            {group.entries.map(entry => (
              <CatalogCard key={`${entry.source_id}/${entry.id}`} entry={entry} mcp={mcp} />
            ))}
          </div>
        </section>
      ))}

      {hidden && mcp.installing === null ? (
        <div className="mcp-catalog-more">
          <button type="button" className="btn" onClick={() => setLimit(current => current + CATALOG_PAGE_STEP)}>再显示 {Math.min(hidden, CATALOG_PAGE_STEP)} 条</button>
          <span>还有 {hidden} 条没铺出来；也可以直接搜索名称缩小范围。</span>
        </div>
      ) : null}

      {!groups.length ? (
        <div className="builtin-ai-mcp-empty">
          <strong>{query.trim() ? '没有匹配的工具' : '目录里还没有可安装的工具'}</strong>
          <span>{query.trim() ? '换个关键词，或者直接用「自定义连接」填启动命令。' : '点「刷新目录」从公开目录拉一份，或者直接用「自定义连接」。'}</span>
        </div>
      ) : null}
      <SearchHint />
    </div>
  );
}

function CatalogCard({ entry, mcp }: { entry: CatalogEntry; mcp: McpManager }) {
  const installed = mcp.installedServer(entry);
  const note = runtimeNote(entry, mcp.requirements);
  const active = mcp.installing?.source_id === entry.source_id && mcp.installing?.id === entry.id;
  return (
    <article className={`builtin-ai-catalog-card${active ? ' active' : ''}`}>
      <div className="builtin-ai-catalog-card-head">
        <strong title={entry.id}>{entry.title}</strong>
        <Pill kind={trustPillKind(entry.trust)}>{trustLabel(entry.trust)}</Pill>
      </div>
      {entry.description ? <p>{entry.description}</p> : null}
      <code title={entry.command_preview || entry.id}>{entry.command_preview || '没有可用的启动方式'}</code>
      <div className="builtin-ai-catalog-card-meta">
        <span title={entry.source_label}>{entry.source_label}</span>
        {entry.version ? <span title={`条目版本 ${entry.version}`}>v{entry.version}</span> : null}
        {note ? <span className={note.missing ? 'missing' : ''}>{note.label}</span> : null}
      </div>
      <div className="builtin-ai-catalog-card-actions">
        {!entry.installable ? (
          <span className="builtin-ai-catalog-blocked" title={entry.reason}><CircleAlert size={13} /><span>{entry.reason}</span></span>
        ) : entry.installed_as ? (
          <>
            <Pill kind="success">已添加</Pill>
            <button type="button" className="btn" disabled={Boolean(mcp.busy) || !installed} title={installed ? '编辑这条连接' : '配置还没读回来，先在「我的能力」里重新读取'} onClick={() => mcp.editInstalled(entry)}><Pencil size={14} />编辑</button>
          </>
        ) : (
          <button
            type="button"
            className="btn btn-primary"
            disabled={Boolean(mcp.busy) || Boolean(note?.missing)}
            title={note?.missing ? `本机还没有${note.label.replace('需要 ', ' ')}` : ''}
            onClick={() => mcp.beginInstall(entry)}
          >
            <Plus size={14} />安装
          </button>
        )}
      </div>
    </article>
  );
}

function InstallInput({ input, mcp }: { input: CatalogInput; mcp: McpManager }) {
  const label = <span className="field-label">{input.label}{input.required ? <span className="builtin-ai-catalog-required">必填</span> : null}</span>;
  if (input.picker === 'directory') {
    return (
      <label className="field-group builtin-ai-mcp-wide">
        {label}
        <span className="builtin-ai-catalog-picker">
          <input value={mcp.installValues[input.key] ?? ''} placeholder={input.placeholder || '选择一个目录'} spellCheck={false} onChange={event => mcp.setInstallValues(current => ({ ...current, [input.key]: event.target.value }))} />
          <button type="button" className="btn" disabled={Boolean(mcp.busy)} onClick={() => void mcp.pickInstallValue(input)}><FolderOpen size={14} />选择目录</button>
        </span>
        {input.description ? <span className="field-hint">{input.description}</span> : null}
      </label>
    );
  }
  return (
    <label className="field-group builtin-ai-mcp-wide">
      {label}
      <input
        type={input.secret ? 'password' : 'text'}
        value={mcp.installValues[input.key] ?? ''}
        placeholder={input.placeholder || (input.secret ? '只保存在本机，用 DPAPI 加密' : '')}
        spellCheck={false}
        autoComplete="off"
        onChange={event => mcp.setInstallValues(current => ({ ...current, [input.key]: event.target.value }))}
      />
      {input.description ? <span className="field-hint">{input.description}</span> : null}
    </label>
  );
}

/// 搜索框在市场页头（aside 顶部），这里只留一句说明，避免出现第二个输入框。
function SearchHint() {
  return <p className="mcp-catalog-search-hint"><Search size={12} />用左上角的搜索框按名称或来源筛选。</p>;
}
