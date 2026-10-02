import { useCallback, useEffect, useRef, useState } from 'react';
import { agentApi, type BuiltinAIMcpServer, type McpProbeResult } from '../services/agentApi';
import { errorDetail } from '../types';
import { formatCommand } from '../lib/commandLine';
import { parseCommand, resolveServerName, validateMcpServer } from '../pages/mcpServerView';
import {
  emptyCatalogView,
  initialValues,
  installBlocker,
  installNotice,
  type CatalogEntry,
  type CatalogInput,
  type CatalogView,
} from '../pages/mcpCatalogView';

/**
 * MCP 工具的状态与动作。
 *
 * 同一份 MCP 世界（`himind-ai-mcp.json` + 目录快照）在三个地方要出现：
 * 市场的「MCP 工具」页签（获得）、「我的能力」的 MCP 页签（拥有）、
 * HiMind AI 会话旁的快捷管理。三处的数据与写操作完全一样，只有排版不同，
 * 所以状态机放在这里，三处只负责画。
 */

export type McpProbeEntry = { pending: true } | { pending: false; result: McpProbeResult };

export function emptyMcpServer(): BuiltinAIMcpServer {
  return {
    server_name: '',
    display_name: '',
    transport: 'stdio',
    command: '',
    args: [],
    env: {},
    cwd: '',
    url: '',
    headers: {},
    tool_call_timeout_ms: 30_000,
    fail_on_startup_error: false,
    reconnect: true,
    enabled: true,
  };
}

/** 后端错误多数是英文底层信息，读不出下一步时退回一句中文。 */
export function mcpErrorText(error: unknown, fallback: string): string {
  const detail = errorDetail(error);
  if (!detail || detail.toLowerCase().includes('invoke') || detail.toLowerCase().includes('undefined')) return fallback;
  return detail;
}

/**
 * 探针的失败原因直接来自 MCP 子进程，多数是英文的底层报错（例如通道已关闭）。
 * 列表里只留一句能让人判断下一步的话，原始错误放进 title，排查时仍然看得到。
 */
export function probeFailureText(result: McpProbeResult): string {
  const detail = (result.error || '').trim();
  if (/[\u4e00-\u9fa5]/.test(detail)) return detail;
  switch (result.error_kind) {
    case 'command_not_found': return '找不到启动命令，请先安装或填完整路径';
    case 'process_start_failed': return '启动命令执行失败';
    case 'process_exit': return '进程启动后立即退出，检查包名或版本是否存在';
    case 'startup_timeout': return '启动超时，检查启动命令与网络';
    case 'invalid_handshake': return '握手失败，这个命令可能不是 MCP 服务';
    case 'tools_list_failed': return '已连上，但读取工具列表失败';
    case 'connection_failed': return '无法连接，检查地址或网络';
    default: return detail || '测试没有完成';
  }
}

export type McpManagerOptions = {
  /** 面板可见时才读写；不可见时不发请求、不重置状态。 */
  active: boolean;
  /** 工具目录只在市场和会话对话框里需要，「我的能力」那一处不读它。 */
  withCatalog?: boolean;
  /** 连接被增删改之后通知外层：让 HiMind AI 重新加载工具上下文。 */
  onChanged?: () => void;
};

export function useMcpManager({ active, withCatalog = true, onChanged }: McpManagerOptions) {
  const [servers, setServers] = useState<BuiltinAIMcpServer[]>([]);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [confirmDelete, setConfirmDelete] = useState('');
  // 连接是不是真能用，只有真连一次才知道。保存后自动测一次，用户也可以手动重测。
  const [probes, setProbes] = useState<Record<string, McpProbeEntry>>({});
  // 预设依赖的命令行工具在不在。读不到时不提示，避免把「没查」显示成「缺失」。
  const [requirements, setRequirements] = useState<Record<string, { available: boolean; path: string }>>({});
  // 手填连接的表单：编辑框里始终是一行启动命令，保存时才拆成 command + args。
  const [draft, setDraft] = useState<BuiltinAIMcpServer | null>(null);
  const [editingName, setEditingName] = useState('');
  const [commandLine, setCommandLine] = useState('');
  // 工具目录：只读本地快照，刷新是一次显式动作（ADR 0005）。
  const [catalog, setCatalog] = useState<CatalogView>(emptyCatalogView);
  const [refreshing, setRefreshing] = useState(false);
  // 正在装的那一条，以及用户填的输入值。安装面板一次只开一个。
  const [installing, setInstalling] = useState<CatalogEntry | null>(null);
  const [installValues, setInstallValues] = useState<Record<string, string>>({});
  const [installName, setInstallName] = useState('');
  const [installChecked, setInstallChecked] = useState(false);
  // 表单和安装面板都在滚动容器顶部，点开时把视线带回去，否则用户以为没反应。
  const panelRef = useRef<HTMLDivElement | null>(null);
  // 外层回调每次渲染都是新函数，用 ref 兜住，避免把「打开面板」的重置逻辑反复触发。
  const changedRef = useRef(onChanged);
  changedRef.current = onChanged;

  const probing = Object.values(probes).some(entry => entry.pending);

  function scrollToTop() {
    const node = panelRef.current;
    if (!node) return;
    // 面板自己可能是滚动容器（会话对话框），也可能只是详情区里的一段（市场、我的能力）。
    // 前者直接滚回去，后者交给最近的滚动祖先把这个面板带回视野，否则点「安装」后
    // 安装面板出现在屏幕外，用户只会以为没反应。
    if (node.scrollHeight > node.clientHeight) node.scrollTo({ top: 0, behavior: 'smooth' });
    else node.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }

  function closeDraft() {
    setDraft(null);
    setEditingName('');
    setCommandLine('');
    setError('');
  }

  function closeInstall() {
    setInstalling(null);
    setInstallValues({});
    setInstallName('');
    setInstallChecked(false);
  }

  const loadServers = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      setServers(await agentApi.builtinAiMcpServers());
    } catch (loadError) {
      setError(mcpErrorText(loadError, '无法读取工具连接。'));
    } finally {
      setLoading(false);
    }
  }, []);

  // 目录是本地快照，读它不联网、不阻塞；联不联网刷新由用户决定。
  const loadCatalog = useCallback(async () => {
    try {
      setCatalog(await agentApi.mcpCatalog());
    } catch (catalogError) {
      setError(mcpErrorText(catalogError, '无法读取工具目录。'));
    }
  }, []);

  const refreshCatalog = useCallback(async () => {
    setRefreshing(true);
    setError('');
    setNotice('');
    try {
      setCatalog(await agentApi.refreshMcpCatalog());
      setNotice('工具目录已更新。');
    } catch (refreshError) {
      setError(mcpErrorText(refreshError, '工具目录刷新失败，检查网络后重试。'));
    } finally {
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    if (!active) return;
    setConfirmDelete('');
    setNotice('');
    closeDraft();
    closeInstall();
    void loadServers();
    if (withCatalog) void loadCatalog();
    else setCatalog(emptyCatalogView());
    void agentApi.mcpRuntimeRequirements().then(setRequirements).catch(() => setRequirements({}));
  }, [active, loadCatalog, loadServers, withCatalog]);

  async function probeServers(names: string[]) {
    const targets = names.filter(Boolean);
    if (!targets.length) return;
    setProbes(current => {
      const next = { ...current };
      for (const name of targets) next[name] = { pending: true };
      return next;
    });
    await Promise.all(targets.map(async name => {
      let result: McpProbeResult;
      try {
        result = await agentApi.testMcpServer(name);
      } catch (probeError) {
        result = {
          ok: false,
          server_name: name,
          server_version: '',
          protocol_version: '',
          transport: '',
          capability_count: 0,
          tool_count: 0,
          duration_ms: 0,
          error_kind: 'probe_failed',
          error: mcpErrorText(probeError, '测试没有完成。'),
        };
      }
      setProbes(current => ({ ...current, [name]: { pending: false, result } }));
    }));
  }

  function beginAdd() {
    closeInstall();
    setDraft(emptyMcpServer());
    setEditingName('');
    setCommandLine('');
    setError('');
    setNotice('');
    scrollToTop();
  }

  function beginEdit(server: BuiltinAIMcpServer) {
    closeInstall();
    setDraft({ ...server, args: [...server.args], env: { ...server.env }, headers: { ...server.headers } });
    setEditingName(server.server_name);
    setCommandLine(formatCommand(server.command, server.args));
    setError('');
    setNotice('');
    scrollToTop();
  }

  /** 目录里那条工具对应哪份已存配置；按服务 ID 找，不区分大小写。 */
  function installedServer(entry: CatalogEntry): BuiltinAIMcpServer | undefined {
    if (!entry.installed_as) return undefined;
    return servers.find(server => server.server_name.toLowerCase() === entry.installed_as!.toLowerCase());
  }

  function editInstalled(entry: CatalogEntry) {
    const server = installedServer(entry);
    if (!server) {
      setError('这条工具的配置没读回来，请先重新读取工具连接。');
      return;
    }
    beginEdit(server);
  }

  function acknowledgedSource(sourceId: string): boolean {
    return catalog.sources.find(source => source.id === sourceId)?.acknowledged ?? false;
  }

  function beginInstall(entry: CatalogEntry) {
    closeDraft();
    setInstallName('');
    setInstallValues(initialValues(entry));
    setInstallChecked(false);
    setInstalling(entry);
    setError('');
    setNotice('');
    scrollToTop();
  }

  async function persist(server: BuiltinAIMcpServer, message: string, busyKey = 'save') {
    setBusy(busyKey);
    setError('');
    setNotice('');
    try {
      await agentApi.validateBuiltinAiMcpServer(server);
      const saved = await agentApi.saveBuiltinAiMcpServer(server);
      await loadServers();
      setNotice(message);
      changedRef.current?.();
      void probeServers([saved.server_name]);
      return saved;
    } catch (saveError) {
      setError(mcpErrorText(saveError, '工具连接保存失败。'));
      return null;
    } finally {
      setBusy('');
    }
  }

  /** 目录条目里声明「要一个目录」的输入，用系统目录选择器填值。 */
  async function pickInstallValue(input: CatalogInput) {
    const picked = await agentApi.pickWorkspaceDirectory();
    const path = picked?.path?.trim();
    if (!path) return;
    setInstallValues(current => ({ ...current, [input.key]: path }));
  }

  /**
   * 从目录装一条。写进去的仍然是 `himind-ai-mcp.json`（ADR 0006），
   * 所以装完就是普通连接：列表、启停、探针全都复用。
   */
  async function installEntry() {
    const entry = installing;
    if (!entry || busy) return;
    const problem = installBlocker(entry, installValues, installChecked, acknowledgedSource(entry.source_id), requirements);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(`install:${entry.source_id}/${entry.id}`);
    setError('');
    setNotice('');
    try {
      const saved = await agentApi.installMcpCatalogEntry({
        source_id: entry.source_id,
        entry_id: entry.id,
        values: installValues,
        display_name: installName.trim(),
        server_name: '',
        acknowledge: installChecked,
      });
      await Promise.all([loadServers(), loadCatalog()]);
      setNotice(installNotice(entry));
      changedRef.current?.();
      void probeServers([saved.server_name]);
      closeInstall();
    } catch (installError) {
      setError(mcpErrorText(installError, '安装失败，检查网络后重试。'));
    } finally {
      setBusy('');
    }
  }

  async function saveDraft() {
    if (!draft || busy) return;
    const mode = editingName ? 'edit' : 'create';
    const serverName = resolveServerName({ mode, typedName: draft.server_name, displayName: draft.display_name, command: commandLine });
    let next: BuiltinAIMcpServer = { ...draft, server_name: serverName };
    if (draft.transport === 'stdio') {
      const parsed = parseCommand(commandLine);
      if (!parsed.ok) {
        setError(parsed.error);
        return;
      }
      next = { ...next, command: parsed.executable, args: parsed.args };
    }
    const problem = validateMcpServer({
      serverName,
      draft: next,
      existingNames: servers.filter(item => item.server_name.toLowerCase() !== editingName.toLowerCase()).map(item => item.server_name),
    });
    if (problem) {
      setError(problem);
      return;
    }
    const saved = await persist(next, '已保存，HiMind AI 已重新连接。');
    if (saved) closeDraft();
  }

  async function setEnabled(server: BuiltinAIMcpServer, enabled: boolean) {
    if (busy) return;
    setBusy(`toggle:${server.server_name}`);
    setError('');
    setNotice('');
    try {
      await agentApi.saveBuiltinAiMcpServer({ ...server, enabled });
      await loadServers();
      setNotice(enabled ? '工具连接已启用。' : '工具连接已停用。');
      changedRef.current?.();
    } catch (toggleError) {
      setError(mcpErrorText(toggleError, '无法更新工具连接。'));
    } finally {
      setBusy('');
    }
  }

  async function removeServer(serverName: string) {
    if (busy) return;
    setBusy(`delete:${serverName}`);
    setError('');
    setNotice('');
    try {
      await agentApi.deleteBuiltinAiMcpServer(serverName);
      if (editingName === serverName) closeDraft();
      setConfirmDelete('');
      setProbes(current => { const next = { ...current }; delete next[serverName]; return next; });
      await loadServers();
      setNotice('工具连接已删除。');
      changedRef.current?.();
    } catch (removeError) {
      setError(mcpErrorText(removeError, '无法删除工具连接。'));
    } finally {
      setBusy('');
    }
  }

  return {
    // 数据
    servers, loading, probes, requirements, catalog,
    // 状态
    busy, error, notice, refreshing, probing, confirmDelete, draft, editingName, commandLine,
    installing, installValues, installName, installChecked, panelRef,
    // 目录动作
    loadCatalog, refreshCatalog, acknowledgedSource,
    // 连接动作
    loadServers, probeServers, beginAdd, beginEdit, editInstalled, installedServer, saveDraft, closeDraft,
    setEnabled, removeServer, setConfirmDelete,
    // 安装动作
    beginInstall, closeInstall, installEntry, pickInstallValue,
    // 表单字段
    setDraft, setCommandLine, setInstallName, setInstallValues, setInstallChecked,
    // 反馈
    setError, setNotice,
  };
}

export type McpManager = ReturnType<typeof useMcpManager>;
