import { useEffect, useState } from 'react';
import { Bell, Plus, Send, Trash2, Webhook } from 'lucide-react';
import { BusyIndicator } from './BusyIndicator';
import { agentApi, type NotificationChannelView } from '../services/agentApi';
import { formatError } from '../types';

/**
 * 通知通道设置面板。
 *
 * 定位：把「审批卡住 / 运行失败」这类执行侧事实推到人手上，而不是等人来开界面。
 * 明确不做的：这里不接收消息、不承载会话——入站渠道归工作台，Agent 只负责出站。
 */

const KIND_OPTIONS: Array<{ value: string; label: string; hint: string }> = [
  { value: 'dingtalk', label: '钉钉', hint: '群自定义机器人 Webhook' },
  { value: 'feishu', label: '飞书', hint: '群机器人 Webhook' },
  { value: 'wecom', label: '企业微信', hint: '群机器人 Webhook' },
  { value: 'slack', label: 'Slack', hint: 'Incoming Webhook' },
  { value: 'discord', label: 'Discord', hint: 'Channel Webhook' },
  { value: 'generic', label: '自定义 Webhook', hint: '接收结构化 JSON' },
];

const EVENT_OPTIONS: Array<{ value: string; label: string }> = [
  { value: 'approval.requested', label: '需要审批' },
  { value: 'run.failed', label: '运行失败' },
];

const RISK_OPTIONS: Array<{ value: string; label: string }> = [
  { value: '', label: '全部风险' },
  { value: 'R2', label: 'R2 及以上' },
  { value: 'R3', label: 'R3 及以上' },
  { value: 'R4', label: '仅 R4' },
];

type Draft = {
  id: string;
  kind: string;
  label: string;
  enabled: boolean;
  events: string[];
  min_risk: string;
  webhook: string;
};

const EMPTY_DRAFT: Draft = {
  id: '',
  kind: 'dingtalk',
  label: '',
  enabled: true,
  events: ['approval.requested', 'run.failed'],
  min_risk: '',
  webhook: '',
};

export function NotificationChannelsPanel() {
  const [channels, setChannels] = useState<NotificationChannelView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [draft, setDraft] = useState<Draft | null>(null);
  const [busy, setBusy] = useState('');

  async function load() {
    setLoading(true);
    try {
      const result = await agentApi.notificationChannels();
      setChannels(result.channels);
      setError('');
    } catch (loadError) {
      setError(formatError(loadError, '读取通知通道失败'));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function save() {
    if (!draft) return;
    // 新通道必须填 webhook 才可用；编辑已有通道时留空表示「不改地址」。
    const existing = channels.find(item => item.id === draft.id);
    if (!existing && !draft.webhook.trim()) {
      setError('新通道必须填写 Webhook 地址');
      return;
    }
    setBusy('save');
    try {
      await agentApi.saveNotificationChannel({
        id: draft.id.trim(),
        kind: draft.kind,
        label: draft.label,
        enabled: draft.enabled,
        events: draft.events,
        min_risk: draft.min_risk,
        webhook: draft.webhook.trim() || undefined,
      });
      setDraft(null);
      setError('');
      await load();
    } catch (saveError) {
      setError(formatError(saveError, '保存通知通道失败'));
    } finally {
      setBusy('');
    }
  }

  async function remove(id: string) {
    setBusy(`remove:${id}`);
    try {
      await agentApi.deleteNotificationChannel(id);
      await load();
    } catch (removeError) {
      setError(formatError(removeError, '删除通知通道失败'));
    } finally {
      setBusy('');
    }
  }

  return (
    <section className="card settings-section notification-channels-card">
      <div className="notification-channels-heading">
        <div>
          <strong>外部通知</strong>
          {/* 只写后果，不介绍功能：这里最需要讲清的是「发出去的是什么、收不到的后果」。 */}
          <span>审批和运行失败时推送到群里。只推摘要，不含代码、产物路径或凭据。</span>
        </div>
        <button type="button" className="btn" onClick={() => setDraft({ ...EMPTY_DRAFT, id: `channel-${Date.now().toString(36)}` })}>
          <Plus size={14} />添加通道
        </button>
      </div>

      {error ? <div className="workflow-inline-error" role="alert">{error}</div> : null}

      {loading ? (
        <div className="notification-channels-loading"><BusyIndicator size={14} />正在读取</div>
      ) : channels.length === 0 ? (
        <div className="notification-channels-empty">
          <Bell size={18} aria-hidden="true" />
          <span>还没有配置通道。添加后，审批请求和运行失败会主动推给你。</span>
        </div>
      ) : (
        <ul className="notification-channels-list">
          {channels.map(channel => (
            <li className={`notification-channel-row${channel.enabled ? '' : ' is-disabled'}`} key={channel.id}>
              <Webhook size={16} aria-hidden="true" />
              <div className="notification-channel-main">
                <strong>{channel.label || channel.id}</strong>
                <small>
                  {KIND_OPTIONS.find(option => option.value === channel.kind)?.label || channel.kind}
                  {' · '}
                  {channel.events.length ? channel.events.map(event => EVENT_OPTIONS.find(option => option.value === event)?.label || event).join(' / ') : '全部事件'}
                  {channel.min_risk ? ` · ${channel.min_risk} 及以上` : ''}
                </small>
                {/* 凭据状态必须显式：配了但没存凭据的通道看起来是好的、实际推不出去。 */}
                {!channel.configured ? <small className="notification-channel-warning">缺少 Webhook 地址，不会投递</small> : null}
              </div>
              <span className={`notification-channel-state ${channel.enabled ? 'on' : 'off'}`}>{channel.enabled ? '已启用' : '已停用'}</span>
              <button
                type="button"
                className="btn btn-icon"
                title="编辑"
                aria-label="编辑"
                onClick={() => setDraft({
                  id: channel.id,
                  kind: channel.kind,
                  label: channel.label,
                  enabled: channel.enabled,
                  events: channel.events,
                  min_risk: channel.min_risk,
                  webhook: '',
                })}
              >
                <Send size={14} />
              </button>
              <button
                type="button"
                className="btn btn-icon"
                title="删除"
                aria-label="删除"
                disabled={busy === `remove:${channel.id}`}
                onClick={() => void remove(channel.id)}
              >
                <Trash2 size={14} />
              </button>
            </li>
          ))}
        </ul>
      )}

      {draft ? (
        <div className="notification-channel-editor" role="dialog" aria-label="编辑通知通道">
          <label className="notification-channel-field">
            <span>通道类型</span>
            <select value={draft.kind} onChange={event => setDraft({ ...draft, kind: event.target.value })}>
              {KIND_OPTIONS.map(option => (
                <option value={option.value} key={option.value}>{option.label} — {option.hint}</option>
              ))}
            </select>
          </label>
          <label className="notification-channel-field">
            <span>名称</span>
            <input
              type="text"
              value={draft.label}
              placeholder="例如：研发群"
              onChange={event => setDraft({ ...draft, label: event.target.value })}
            />
          </label>
          <label className="notification-channel-field">
            <span>Webhook 地址</span>
            <input
              type="text"
              value={draft.webhook}
              placeholder={channels.some(item => item.id === draft.id) ? '留空表示不修改已保存的地址' : 'https://…'}
              onChange={event => setDraft({ ...draft, webhook: event.target.value })}
            />
            {/* 说清存储方式：用户有权知道这个地址会不会明文落盘。 */}
            <small>地址加密保存在本机凭据里，不会明文写入配置或日志。</small>
          </label>
          <fieldset className="notification-channel-field">
            <span>推送哪些事件</span>
            <div className="notification-channel-events">
              {EVENT_OPTIONS.map(option => (
                <label key={option.value}>
                  <input
                    type="checkbox"
                    checked={draft.events.includes(option.value)}
                    onChange={event => setDraft({
                      ...draft,
                      events: event.target.checked
                        ? [...draft.events, option.value]
                        : draft.events.filter(value => value !== option.value),
                    })}
                  />
                  <span>{option.label}</span>
                </label>
              ))}
            </div>
            <small>都不勾表示推送全部事件。</small>
          </fieldset>
          <label className="notification-channel-field">
            <span>风险门槛</span>
            <select value={draft.min_risk} onChange={event => setDraft({ ...draft, min_risk: event.target.value })}>
              {RISK_OPTIONS.map(option => (
                <option value={option.value} key={option.value || 'all'}>{option.label}</option>
              ))}
            </select>
          </label>
          <label className="notification-channel-field inline">
            <input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled: event.target.checked })} />
            <span>启用这个通道</span>
          </label>
          <div className="notification-channel-actions">
            <button type="button" className="btn" onClick={() => { setDraft(null); setError(''); }}>取消</button>
            <button type="button" className="btn btn-primary" disabled={busy === 'save'} onClick={() => void save()}>
              {busy === 'save' ? <BusyIndicator size={14} /> : null}{busy === 'save' ? '保存中' : '保存'}
            </button>
          </div>
        </div>
      ) : null}
    </section>
  );
}
