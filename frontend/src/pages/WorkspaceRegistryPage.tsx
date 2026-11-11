import { useEffect, useMemo, useRef, useState } from 'react';
import { CircleAlert, FolderTree, GitBranch, Plus, RefreshCw, Trash2, Waypoints } from 'lucide-react';
import {
  agentApi,
  type WorkspaceRegistryBinding,
  type WorkspaceRegistryCandidate,
  type WorkspaceRegistryProject,
  type WorkspaceRegistrySnapshot,
  type WorkspaceRegistryTree,
  type WorkspaceRegistryValidation,
} from '../services/agentApi';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { Pill } from '../components/Common';

/**
 * 工作区登记。
 *
 * 这一屏回答一个问题：这个目录属于哪个项目、它的上级是谁。
 * 它不显示工时与进度（那是工作日志的事），也不改变任何会话权限——
 * 「登记了」不等于「能在这里写代码」。
 *
 * 数据落 `data/workspace-registry.json`，走备份包；同一份表也供
 * `workspace.registry.*` 能力读取，AI 在对话里解析工作区用的就是它。
 */

const KIND_OPTIONS: Array<{ value: string; label: string }> = [
  { value: 'product', label: '产品' },
  { value: 'subproject', label: '子项目' },
  { value: 'aggregate', label: '聚合仓库' },
  { value: 'delivery', label: '交付' },
  { value: 'library', label: '库' },
  { value: 'docs', label: '文档' },
  { value: 'sandbox', label: '沙箱' },
  { value: 'other', label: '其他' },
];

const KIND_LABELS: Record<string, string> = Object.fromEntries(KIND_OPTIONS.map(item => [item.value, item.label]));

const ROLE_OPTIONS = ['root', 'worktree', 'mirror', 'release', 'source', 'build'];
const ROLE_LABELS: Record<string, string> = {
  root: '项目根',
  worktree: '工作树',
  mirror: '镜像',
  release: '发布目录',
  source: '源码',
  build: '构建产物',
};

type Tab = 'tree' | 'paths' | 'candidates';

type EditState = {
  /** 空串表示新建。 */
  id: string;
  name: string;
  aliases: string;
  kind: string;
  parentId: string;
  stack: string;
  docs: string;
  rules: string;
  notes: string;
};

function emptyEdit(): EditState {
  return { id: '', name: '', aliases: '', kind: 'product', parentId: '', stack: '', docs: '', rules: '', notes: '' };
}

function splitList(value: string): string[] {
  return value.split(/[,，]/).map(part => part.trim()).filter(Boolean);
}

/** 后端错误码翻成下一步动作，保留原始码便于排障。 */
function friendlyRegistryError(message: string): string {
  const table: Array<[string, string]> = [
    ['PATH_CONFLICT', '这个路径已经绑定到另一个项目了，先在「路径绑定」里解绑。'],
    ['PROJECT_NOT_FOUND', '找不到这个项目，可能刚被删除或改名。'],
    ['PROJECT_HAS_CHILDREN', '该项目还有子项目，先把子项目移走或强制删除。'],
    ['PROJECT_HAS_WORKSPACES', '该项目还绑着路径，先解绑再删除。'],
    ['PARENT_CYCLE', '这样设置会让项目互为上下级，请换一个上级。'],
    ['ALIAS_CONFLICT', '这个别名已被另一个项目占用。'],
    ['PATH_NOT_ABSOLUTE', '需要一个绝对路径（盘符或 UNC 开头）。'],
    ['PATH_INVALID', '路径写法有问题，检查盘符、UNC 主机名或多余的 ..。'],
  ];
  for (const [needle, hint] of table) {
    if (message.includes(needle)) return `${hint}（${message}）`;
  }
  return message;
}

export function WorkspaceRegistryPage() {
  const confirm = useConfirm();
  const [tab, setTab] = useState<Tab>('tree');
  const [snapshot, setSnapshot] = useState<WorkspaceRegistrySnapshot | null>(null);
  const [candidates, setCandidates] = useState<WorkspaceRegistryCandidate[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [edit, setEdit] = useState<EditState | null>(null);
  const [filter, setFilter] = useState('');
  const alive = useRef(true);

  async function load() {
    setError('');
    try {
      const [next, pending] = await Promise.all([
        agentApi.workspaceRegistry(),
        agentApi.listWorkspaceCandidates(),
      ]);
      if (!alive.current) return;
      setSnapshot(next);
      setCandidates(pending.candidates);
    } catch (loadError) {
      if (alive.current) setError(friendlyRegistryError(String(loadError)));
    } finally {
      if (alive.current) setLoading(false);
    }
  }

  useEffect(() => {
    alive.current = true;
    void load();
    return () => { alive.current = false; };
  }, []);

  const projects = snapshot?.projects ?? [];
  const workspaces = snapshot?.workspaces ?? [];

  const projectName = (id: string) => projects.find(item => item.id === id)?.name ?? id;

  const visibleTree = useMemo(() => {
    const keyword = filter.trim().toLowerCase();
    if (!keyword) return snapshot?.tree ?? [];
    // 筛选时保留祖先链：否则搜到子项目，界面上看不到它在谁下面。
    const byId = new Map(projects.map(item => [item.id, item]));
    const keep = new Set<string>();
    for (const project of projects) {
      const hit = project.name.toLowerCase().includes(keyword)
        || (project.aliases ?? []).some(alias => alias.toLowerCase().includes(keyword));
      if (!hit) continue;
      keep.add(project.id);
      let cursor = byId.get(project.id);
      let guard = 0;
      while (cursor?.parent_id && guard < 32) {
        keep.add(cursor.parent_id);
        cursor = byId.get(cursor.parent_id);
        guard += 1;
      }
    }
    const prune = (nodes: WorkspaceRegistryTree[]): WorkspaceRegistryTree[] => nodes
      .filter(node => keep.has(node.project.id))
      .map(node => ({ ...node, children: prune(node.children) }));
    return prune(snapshot?.tree ?? []);
  }, [snapshot, projects, filter]);

  function startCreate() {
    setEdit(emptyEdit());
    setNotice('');
    setError('');
  }

  function startEdit(project: WorkspaceRegistryProject) {
    setEdit({
      id: project.id,
      name: project.name,
      aliases: (project.aliases ?? []).join(', '),
      kind: project.kind || 'product',
      parentId: project.parent_id ?? '',
      stack: (project.context?.stack ?? []).join(', '),
      docs: (project.context?.docs ?? []).map(item => item.path).join(', '),
      rules: (project.context?.rules ?? []).map(item => item.path).join(', '),
      notes: project.context?.notes ?? '',
    });
    setNotice('');
    setError('');
  }

  async function saveEdit() {
    if (!edit) return;
    if (!edit.name.trim()) {
      setError('显示名称不能为空');
      return;
    }
    setBusy('save');
    setError('');
    try {
      await agentApi.upsertWorkspaceProject({
        id: edit.id || undefined,
        name: edit.name.trim(),
        aliases: splitList(edit.aliases),
        kind: edit.kind,
        // 空串表示「提升为根」，后端用三态字段区分「没传」与「显式清空」。
        parent_id: edit.parentId || null,
        stack: splitList(edit.stack),
        docs: splitList(edit.docs).map(path => ({ label: path, path })),
        rules: splitList(edit.rules).map(path => ({ label: path, path })),
        notes: edit.notes,
      });
      setNotice(edit.id ? `已保存：${edit.name}` : `已新建项目：${edit.name}`);
      setEdit(null);
      await load();
    } catch (saveError) {
      setError(friendlyRegistryError(String(saveError)));
    } finally {
      setBusy('');
    }
  }

  async function removeProject(project: WorkspaceRegistryProject) {
    const ok = await confirm({
      title: `删除「${project.name}」？`,
      description: '如果它还有子项目或绑定的路径，删除会被拒绝；先移走它们再删。',
      confirmText: '删除',
      danger: true,
    });
    if (!ok) return;
    setBusy(`delete:${project.id}`);
    setError('');
    try {
      await agentApi.deleteWorkspaceProject(project.id);
      setNotice(`已删除：${project.name}`);
      if (edit?.id === project.id) setEdit(null);
      await load();
    } catch (deleteError) {
      setError(friendlyRegistryError(String(deleteError)));
    } finally {
      setBusy('');
    }
  }

  async function bindPath(project: WorkspaceRegistryProject) {
    setBusy(`bind:${project.id}`);
    setError('');
    try {
      const picked = await agentApi.pickWorkspaceDirectory();
      const path = (picked?.path ?? '').trim();
      if (!path) return;
      await agentApi.bindWorkspacePath({
        project_ref: project.id,
        path,
        role: 'root',
        source: 'manual',
        confidence: 'exact',
      });
      setNotice(`已绑定到 ${project.name}：${path}`);
      await load();
    } catch (bindError) {
      setError(friendlyRegistryError(String(bindError)));
    } finally {
      setBusy('');
    }
  }

  async function unbind(item: WorkspaceRegistryBinding) {
    const ok = await confirm({
      title: '解绑这条路径？',
      description: `解绑后，AI 无法再从 ${item.path} 认出项目；登记本身不会被删除。`,
      confirmText: '解绑',
      danger: true,
    });
    if (!ok) return;
    setBusy(`unbind:${item.path}`);
    setError('');
    try {
      await agentApi.bindWorkspacePath({ path: item.path, unbind: true });
      setNotice(`已解绑：${item.path}`);
      await load();
    } catch (unbindError) {
      setError(friendlyRegistryError(String(unbindError)));
    } finally {
      setBusy('');
    }
  }

  async function acceptCandidate(candidate: WorkspaceRegistryCandidate, mode: 'create' | 'bind', projectRef?: string) {
    setBusy(`candidate:${candidate.path}`);
    setError('');
    try {
      await agentApi.acceptWorkspaceCandidate(candidate.path, mode, projectRef, candidate.suggested_parent);
      setNotice(mode === 'create' ? `已新建项目并绑定：${candidate.path}` : `已绑定：${candidate.path}`);
      await load();
    } catch (acceptError) {
      setError(friendlyRegistryError(String(acceptError)));
    } finally {
      setBusy('');
    }
  }

  async function dismissCandidate(candidate: WorkspaceRegistryCandidate) {
    setBusy(`candidate:${candidate.path}`);
    setError('');
    try {
      await agentApi.dismissWorkspaceCandidate(candidate.path);
      setNotice('已忽略这条候选');
      await load();
    } catch (dismissError) {
      setError(friendlyRegistryError(String(dismissError)));
    } finally {
      setBusy('');
    }
  }

  async function validate() {
    setBusy('validate');
    setError('');
    try {
      const result: WorkspaceRegistryValidation = await agentApi.validateWorkspaceRegistry();
      if (result.ok) {
        setNotice(`登记表没问题：${result.project_count} 个项目，${result.workspace_count} 条路径`);
      } else {
        const issues = [...result.problems, ...result.missing_paths.map(path => `路径不存在：${path}`)];
        setError(issues.join('\n'));
      }
    } catch (validateError) {
      setError(friendlyRegistryError(String(validateError)));
    } finally {
      setBusy('');
    }
  }

  const parentOptions = edit
    ? projects.filter(item => item.id !== edit.id).sort((left, right) => left.name.localeCompare(right.name, 'zh-CN'))
    : [];

  return (
    <section className="workspace-registry">
      <header className="workspace-registry-head">
        <div>
          <h2>工作区登记</h2>
          <p>记录路径属于哪个项目。AI 判断当前目录时读的就是这张表。</p>
        </div>
        <div className="actions-row">
          <button type="button" className="btn" onClick={() => void load()} disabled={loading || Boolean(busy)}>
            <RefreshCw size={15} />刷新
          </button>
          <button type="button" className="btn" onClick={() => void validate()} disabled={Boolean(busy)}>
            <CircleAlert size={15} />检查登记表
          </button>
          <button type="button" className="btn btn-primary" onClick={startCreate}>
            <Plus size={15} />新建项目
          </button>
        </div>
      </header>

      {error ? <div className="workspace-registry-alert error"><CircleAlert size={16} /><pre>{error}</pre></div> : null}
      {notice ? <div className="workspace-registry-alert">{notice}</div> : null}

      <nav className="workspace-registry-tabs" role="tablist" aria-label="工作区登记">
        {([
          ['tree', `项目树${projects.length ? ` · ${projects.length}` : ''}`],
          ['paths', `路径绑定${workspaces.length ? ` · ${workspaces.length}` : ''}`],
          ['candidates', `候选${candidates.length ? ` · ${candidates.length}` : ''}`],
        ] as Array<[Tab, string]>).map(([key, label]) => (
          <button
            key={key}
            type="button"
            role="tab"
            aria-selected={tab === key}
            className={tab === key ? 'active' : ''}
            onClick={() => setTab(key)}
          >
            {label}
          </button>
        ))}
      </nav>

      {loading ? <BusyIndicator label="读取登记表" /> : null}

      {tab === 'tree' ? (
        <div className="workspace-registry-panel">
          <div className="workspace-registry-toolbar">
            <input
              type="search"
              className="input"
              placeholder="按名称或别名筛选"
              value={filter}
              onChange={event => setFilter(event.target.value)}
            />
          </div>
          {visibleTree.length === 0 ? (
            <p className="workspace-registry-empty">
              {projects.length === 0 ? '还没有登记任何项目。点右上角「新建项目」开始。' : '没有匹配的项目。'}
            </p>
          ) : (
            <ul className="workspace-registry-tree">
              {visibleTree.map(node => renderTreeNode(node, 0))}
            </ul>
          )}
        </div>
      ) : null}

      {tab === 'paths' ? (
        <div className="workspace-registry-panel">
          {workspaces.length === 0 ? (
            <p className="workspace-registry-empty">还没有路径绑定。在项目树里点「绑定路径」。</p>
          ) : (
            <table className="table workspace-registry-table">
              <thead>
                <tr><th>路径</th><th>项目</th><th>角色</th><th>来源</th><th>置信度</th><th /></tr>
              </thead>
              <tbody>
                {workspaces.map(item => (
                  <tr key={item.id}>
                    <td className="workspace-registry-path">{item.path}</td>
                    <td>{projectName(item.project_id)}</td>
                    <td>{ROLE_LABELS[item.role] ?? item.role}</td>
                    <td>{item.source}</td>
                    <td><Pill kind={item.confidence === 'exact' ? 'success' : 'warn'}>{item.confidence === 'exact' ? '精确' : '推断'}</Pill></td>
                    <td>
                      <button
                        type="button"
                        className="btn"
                        disabled={Boolean(busy)}
                        onClick={() => void unbind(item)}
                      >
                        解绑
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      ) : null}

      {tab === 'candidates' ? (
        <div className="workspace-registry-panel">
          {candidates.length === 0 ? (
            <p className="workspace-registry-empty">
              AI 遇到未登记的目录会在这里留下候选；它只是建议。
            </p>
          ) : (
            <ul className="workspace-registry-candidates">
              {candidates.map(candidate => (
                <li key={candidate.path}>
                  <div className="workspace-registry-candidate-head">
                    <strong>{candidate.suggested_project || '未命名'}</strong>
                    <Pill kind={candidate.confidence === 'exact' ? 'success' : 'warn'}>
                      {candidate.confidence === 'exact' ? '精确匹配' : '推断'}
                    </Pill>
                    <span className="muted">出现 {candidate.seen_count} 次</span>
                  </div>
                  <p className="workspace-registry-path">{candidate.path}</p>
                  <div className="actions-row">
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={Boolean(busy)}
                      onClick={() => void acceptCandidate(candidate, 'create')}
                    >
                      新建项目并绑定
                    </button>
                    <select
                      className="input"
                      defaultValue=""
                      onChange={event => {
                        const value = event.target.value;
                        if (value) void acceptCandidate(candidate, 'bind', value);
                      }}
                    >
                      <option value="">绑定到已有项目…</option>
                      {projects.map(project => (
                        <option key={project.id} value={project.id}>{project.name}</option>
                      ))}
                    </select>
                    <button
                      type="button"
                      className="btn"
                      disabled={Boolean(busy)}
                      onClick={() => void dismissCandidate(candidate)}
                    >
                      忽略
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}

      {edit ? (
        <div className="workspace-registry-editor" role="dialog" aria-modal="true" aria-labelledby="workspace-editor-title">
          <div className="workspace-registry-editor-head">
            <strong id="workspace-editor-title">{edit.id ? '编辑项目' : '新建项目'}</strong>
            <button type="button" className="btn btn-icon" aria-label="关闭" onClick={() => setEdit(null)}>×</button>
          </div>
          <form
            className="workspace-registry-form"
            onSubmit={event => { event.preventDefault(); void saveEdit(); }}
          >
            <label>
              <span>显示名称</span>
              <input
                required
                maxLength={40}
                value={edit.name}
                onChange={event => setEdit({ ...edit, name: event.target.value })}
              />
            </label>
            <label>
              <span>别名（逗号分隔）</span>
              <input
                placeholder="work-log, 日志"
                value={edit.aliases}
                onChange={event => setEdit({ ...edit, aliases: event.target.value })}
              />
            </label>
            <label>
              <span>形态</span>
              <select value={edit.kind} onChange={event => setEdit({ ...edit, kind: event.target.value })}>
                {KIND_OPTIONS.map(option => (
                  <option key={option.value} value={option.value}>{option.label}</option>
                ))}
              </select>
            </label>
            <label>
              <span>上级项目</span>
              <select value={edit.parentId} onChange={event => setEdit({ ...edit, parentId: event.target.value })}>
                <option value="">（无，作为根）</option>
                {parentOptions.map(project => (
                  <option key={project.id} value={project.id}>{project.name}</option>
                ))}
              </select>
            </label>
            <label>
              <span>技术栈（逗号分隔）</span>
              <input
                placeholder="Go, JavaScript"
                value={edit.stack}
                onChange={event => setEdit({ ...edit, stack: event.target.value })}
              />
            </label>
            <label>
              <span>文档入口（逗号分隔路径）</span>
              <input
                placeholder="README.md, docs/design.md"
                value={edit.docs}
                onChange={event => setEdit({ ...edit, docs: event.target.value })}
              />
            </label>
            <label>
              <span>规则文件（逗号分隔路径）</span>
              <input
                placeholder="AGENTS.md"
                value={edit.rules}
                onChange={event => setEdit({ ...edit, rules: event.target.value })}
              />
            </label>
            <label>
              <span>备注</span>
              <textarea
                rows={3}
                value={edit.notes}
                onChange={event => setEdit({ ...edit, notes: event.target.value })}
              />
            </label>
            <div className="actions-row workspace-registry-form-actions">
              {edit.id ? (
                <button
                  type="button"
                  className="btn btn-danger"
                  disabled={Boolean(busy)}
                  onClick={() => {
                    const project = projects.find(item => item.id === edit.id);
                    if (project) void removeProject(project);
                  }}
                >
                  <Trash2 size={15} />删除
                </button>
              ) : null}
              <span className="workspace-registry-spacer" />
              <button type="button" className="btn" onClick={() => setEdit(null)}>取消</button>
              <button type="submit" className="btn btn-primary" disabled={busy === 'save'}>保存</button>
            </div>
          </form>
        </div>
      ) : null}
    </section>
  );

  function renderTreeNode(node: WorkspaceRegistryTree, depth: number) {
    const project = node.project;
    const paths = node.paths ?? [];
    return (
      <li key={project.id}>
        <div className="workspace-registry-node" style={{ paddingLeft: 8 + depth * 18 }}>
          <FolderTree size={15} />
          <button type="button" className="workspace-registry-node-name" onClick={() => startEdit(project)}>
            {project.name}
          </button>
          <span className="muted">
            {KIND_LABELS[project.kind ?? 'other'] ?? project.kind}
            {paths.length ? ` · ${paths.length} 条路径` : ' · 未绑定路径'}
          </span>
          <span className="workspace-registry-spacer" />
          <button
            type="button"
            className="btn"
            disabled={Boolean(busy)}
            onClick={() => void bindPath(project)}
          >
            <Waypoints size={14} />绑定路径
          </button>
        </div>
        {paths.length ? (
          <ul className="workspace-registry-node-paths">
            {paths.map(item => (
              <li key={item.id}>
                <GitBranch size={13} />
                <span className="workspace-registry-path">{item.path}</span>
                <span className="muted">{ROLE_LABELS[item.role] ?? item.role}</span>
              </li>
            ))}
          </ul>
        ) : null}
        {node.children?.length ? (
          <ul>{node.children.map(child => renderTreeNode(child, depth + 1))}</ul>
        ) : null}
      </li>
    );
  }
}
