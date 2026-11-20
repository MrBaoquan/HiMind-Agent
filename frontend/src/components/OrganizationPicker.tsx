import { useEffect, useState } from 'react';
import { ChevronRight, Folder, Search, User, X } from 'lucide-react';
import { organizationRef, type OrganizationDirectory, type OrganizationNode, type OrganizationPage } from '../services/organizationDirectory';
import './OrganizationPicker.css';

const messageFor = (error: unknown) => error instanceof Error ? error.message : String(error);

export function OrganizationPicker({ directory, selected, onToggle, onClear }: {
  directory: OrganizationDirectory;
  selected: Map<string, OrganizationNode>;
  onToggle(node: OrganizationNode): void;
  onClear(): void;
}) {
  const [ancestors, setAncestors] = useState<OrganizationNode[]>([]);
  const [query, setQuery] = useState('');
  const [page, setPage] = useState<OrganizationPage>({ nodes: [] });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  const [cursor, setCursor] = useState<string>();
  const parent = ancestors[ancestors.length - 1]?.id;
  const searching = query.trim() !== '';
  useEffect(() => {
    let live = true;
    const timer = window.setTimeout(() => {
      const request = query.trim() ? directory.search(query.trim(), cursor) : directory.children(parent, cursor);
      void request.then(result => {
        if (live) { setError(''); setPage(previous => ({ ...result, nodes: cursor ? [...previous.nodes, ...result.nodes] : result.nodes })); }
      }).catch(cause => { if (live) setError(messageFor(cause)); }).finally(() => { if (live) setLoading(false); });
    }, query.trim() ? 180 : 0);
    return () => { live = false; window.clearTimeout(timer); };
  }, [directory, parent, query, cursor, attempt]);
  const browse = (path: OrganizationNode[]) => { setPage({ nodes: [] }); setError(''); setLoading(true); setCursor(undefined); setAncestors(path); setQuery(''); };
  const row = (node: OrganizationNode, browseable: boolean) => {
    const reference = organizationRef(node);
    const Icon = node.kind === 'department' ? Folder : User;
    return <li className="organization-picker-row" key={`${reference}:${node.parent_id || ''}`}>
      <label title={node.path || node.name}>
        <input type="checkbox" checked={selected.has(reference)} onChange={() => onToggle(node)} />
        <Icon size={15} aria-hidden="true" />
        <span><strong>{node.name}</strong>{(node.kind === 'person' || searching) && node.path ? <small>{node.path}</small> : null}{node.kind === 'department' && !searching ? <small>包含下级部门</small> : null}</span>
      </label>
      {browseable && <button type="button" className="btn btn-ghost organization-picker-open" aria-label={`浏览 ${node.name}`} onClick={() => browse([...ancestors, node])}><ChevronRight size={16} aria-hidden="true" /></button>}
    </li>;
  };
  const departments = page.nodes.filter(node => node.kind === 'department');
  const people = page.nodes.filter(node => node.kind === 'person');
  return <div className="organization-picker" aria-busy={loading}>
    <label className="organization-picker-search"><Search size={16} aria-hidden="true" /><input type="search" aria-label="搜索人员或部门" placeholder="搜索人员或部门" value={query} onChange={event => { setPage({ nodes: [] }); setError(''); setLoading(true); setCursor(undefined); setQuery(event.target.value); }} /></label>
    {!searching && <nav className="organization-picker-path" aria-label="部门路径">
      {ancestors.length ? <button type="button" onClick={() => browse([])}>组织目录</button> : <span aria-current="location">组织目录</span>}
      {ancestors.map((node, index) => <span key={node.id}><ChevronRight size={12} aria-hidden="true" />{index < ancestors.length - 1 ? <button type="button" onClick={() => browse(ancestors.slice(0, index + 1))}>{node.name}</button> : <span aria-current="location" title={node.name}>{node.name}</span>}</span>)}
    </nav>}
    <div className="organization-picker-columns">
      {!searching && <section aria-label="部门"><h3>部门</h3><ul>{departments.map(node => row(node, true))}</ul>{!loading && !error && !departments.length && <small>当前层级无下级部门</small>}</section>}
      <section aria-label={searching ? '搜索结果' : '成员'}><h3>{searching ? '搜索结果' : '成员'}</h3><ul>{(searching ? page.nodes : people).map(node => row(node, false))}</ul>{!loading && !error && !(searching ? page.nodes : people).length && <small>{searching ? '未找到符合条件的对象' : '当前层级无成员'}</small>}</section>
    </div>
    {error && <div role="alert" className="organization-picker-message"><span>{error}</span><button type="button" className="btn" onClick={() => { setLoading(true); setAttempt(value => value + 1); }}>重试</button></div>}
    {loading && <small role="status">正在读取组织目录…</small>}
    {page.next_cursor && !loading && !error && <button type="button" className="btn" onClick={() => { setLoading(true); setCursor(page.next_cursor); }}>加载更多</button>}
    <div className="organization-picker-selection">
      <span aria-live="polite">已选 {selected.size} 项</span>{selected.size > 0 && <button className="btn btn-ghost" type="button" onClick={onClear}>清空</button>}
      {selected.size > 0 && <ul>{[...selected].map(([key, node]) => <li key={key}><button type="button" title={node.path || node.name} onClick={() => onToggle(node)} aria-label={`移除 ${node.name}`}>{node.name}<X size={13} aria-hidden="true" /></button></li>)}</ul>}
    </div>
  </div>;
}
