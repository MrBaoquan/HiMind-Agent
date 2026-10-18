import { useEffect, useState } from 'react';
import { KeyRound, Trash2 } from 'lucide-react';
import { agentApi, type PluginCredentialView } from '../services/agentApi';
import { BusyIndicator } from './BusyIndicator';
import { Pill } from './Common';

/**
 * 插件凭据：插件在清单里声明需求，值由 Agent 统一保管。
 *
 * 界面只显示「配没配 / 配了哪些字段 / 什么时候更新的」，**不回显值**：值一保存
 * 就以 DPAPI 按当前 Windows 用户加密落盘，只有 Agent 自己能解密。凭据按插件隔离，
 * 轮换就是重新保存一次，删除即失效。
 */
export function PluginCredentialsPanel({ pluginId }: { pluginId: string }) {
  const [items, setItems] = useState<PluginCredentialView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState('');
  const [drafts, setDrafts] = useState<Record<string, Record<string, string>>>({});

  const load = () => {
    setLoading(true);
    setError('');
    return agentApi.pluginCredentials(pluginId)
      .then(result => setItems(result.credentials))
      .catch(reason => setError(reason instanceof Error ? reason.message : String(reason)))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError('');
    agentApi.pluginCredentials(pluginId)
      .then(result => { if (active) setItems(result.credentials); })
      .catch(reason => { if (active) setError(reason instanceof Error ? reason.message : String(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [pluginId]);

  if (loading) {
    return <section className="plugin-detail-section"><div className="plugin-section-heading"><div><h4>凭据</h4></div></div><div className="plugin-section-empty"><BusyIndicator size={14} />正在读取凭据状态</div></section>;
  }
  // 没有声明就不占位置：凭据是插件自己提的需求，不是所有插件都需要。
  if (!items.length && !error) return null;

  const fieldsOf = (item: PluginCredentialView) => (item.fields.length ? item.fields : ['value']);

  const save = async (item: PluginCredentialView) => {
    const values = drafts[item.id] || {};
    const filled: Record<string, string> = {};
    for (const field of fieldsOf(item)) {
      const value = (values[field] || '').trim();
      if (value) filled[field] = value;
    }
    if (!Object.keys(filled).length) {
      setError('请先填写要保存的字段');
      return;
    }
    setBusy(item.id);
    setError('');
    try {
      await agentApi.savePluginCredential(pluginId, item.id, filled);
      setDrafts(current => ({ ...current, [item.id]: {} }));
      await load();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy('');
    }
  };

  const remove = async (item: PluginCredentialView) => {
    setBusy(item.id);
    setError('');
    try {
      await agentApi.deletePluginCredential(pluginId, item.id);
      await load();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy('');
    }
  };

  return <section className="plugin-detail-section">
    <div className="plugin-section-heading"><div><h4>凭据</h4><small>由 Agent 保管：按插件隔离、加密落盘，插件读不到明文</small></div></div>
    {error ? <div className="plugin-local-error">{error}</div> : null}
    <div className="plugin-credential-list">
      {items.map(item => <article className="plugin-credential-row" key={item.id}>
        <div className="plugin-credential-head">
          <KeyRound size={16} />
          <div><strong>{item.label}</strong><small>{item.kind || 'api_key'}{item.required ? ' · 必需' : ' · 可选'}</small></div>
          <Pill kind={item.configured ? 'success' : item.required ? 'warn' : 'neutral'}>{item.configured ? '已配置' : '未配置'}</Pill>
        </div>
        {item.description ? <p className="muted">{item.description}</p> : null}
        <div className="plugin-credential-fields">
          {fieldsOf(item).map(field => <label key={field}>
            <span>{field}</span>
            <input
              type="password"
              autoComplete="off"
              placeholder={item.configured && item.configured_fields.includes(field) ? '已保存（留空则不修改）' : '待填写'}
              value={(drafts[item.id] || {})[field] || ''}
              onChange={event => setDrafts(current => ({ ...current, [item.id]: { ...(current[item.id] || {}), [field]: event.target.value } }))}
            />
          </label>)}
        </div>
        <div className="plugin-credential-actions">
          <button className="btn btn-primary" disabled={Boolean(busy)} onClick={() => void save(item)}>{busy === item.id ? <BusyIndicator size={14} /> : null}保存</button>
          {item.configured ? <button className="btn btn-danger-quiet" disabled={Boolean(busy)} onClick={() => void remove(item)}><Trash2 size={14} />删除</button> : null}
          {item.docs_url ? <a className="plugin-credential-docs" href={item.docs_url} target="_blank" rel="noreferrer">如何获取</a> : null}
        </div>
      </article>)}
    </div>
  </section>;
}
