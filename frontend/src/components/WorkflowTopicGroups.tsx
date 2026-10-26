import { useState } from 'react';
import { Plus, Trash2, X } from 'lucide-react';
import {
  CUSTOM_TOPIC_PATTERN,
  newTopicGroup,
  normalizeTopicGroupsSafe,
  topicGroupsSelectionPlan,
  type TopicGroupFormValue,
  type WorkflowStartField,
} from '../pages/workflowStartForm';

export function TopicGroupsEditor({ field, value, onChange }: {
  field: WorkflowStartField;
  value: unknown;
  onChange: (value: TopicGroupFormValue[]) => void;
}) {
  const groups = normalizeTopicGroupsSafe(value);
  const updateGroup = (index: number, next: TopicGroupFormValue) => {
    onChange(groups.map((group, current) => current === index ? next : group));
  };
  return (
    <div className="topic-groups-editor">
      {field.optionSourceWarning ? <small className="workflow-theme-source-warning">{field.optionSourceWarning}</small> : null}
      {groups.map((group, index) => (
        <TopicGroupCard
          key={group.id}
          field={field}
          group={group}
          index={index}
          total={groups.length}
          onChange={next => updateGroup(index, next)}
          onRemove={() => onChange(groups.filter((_, current) => current !== index))}
        />
      ))}
      <button
        type="button"
        className="btn topic-group-add"
        disabled={groups.length >= 4}
        onClick={() => onChange([...groups, newTopicGroup(groups.length)])}
      >
        <Plus size={14} />添加采集组
      </button>
    </div>
  );
}

function TopicGroupCard({ field, group, index, total, onChange, onRemove }: {
  field: WorkflowStartField;
  group: TopicGroupFormValue;
  index: number;
  total: number;
  onChange: (group: TopicGroupFormValue) => void;
  onRemove: () => void;
}) {
  const [query, setQuery] = useState('');
  const [custom, setCustom] = useState('');
  const [error, setError] = useState('');
  const selected = new Set(group.topics);
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const options = field.optionEntries
    .filter(option => !selected.has(option.value))
    .filter(option => !normalizedQuery || [option.label, option.value, option.description, option.source].join(' ').toLocaleLowerCase().includes(normalizedQuery))
    .slice(0, 18);
  const addTopic = (topic: string) => {
    if (!CUSTOM_TOPIC_PATTERN.test(topic) || selected.has(topic)) return;
    onChange({ ...group, topics: [...group.topics, topic] });
  };
  const addCustom = () => {
    const topic = custom.trim().toLowerCase();
    if (!CUSTOM_TOPIC_PATTERN.test(topic)) {
      setError('只允许小写字母、数字和连字符，长度 1–50');
      return;
    }
    if (selected.has(topic)) {
      setError('这个标签已经在当前采集组中');
      return;
    }
    addTopic(topic);
    setCustom('');
    setError('');
  };
  return (
    <section className="topic-group-card">
      <div className="topic-group-head">
        <span className="topic-group-index">采集组 {index + 1}</span>
        <input
          className="topic-group-name"
          value={group.name}
          maxLength={40}
          placeholder="例如：AI 视频创作"
          aria-label={`采集组 ${index + 1} 名称`}
          onChange={event => onChange({ ...group, name: event.target.value })}
        />
        <span className="topic-group-count">{group.topics.length} 个标签 · {group.topics.length * 2} 次查询</span>
        <button type="button" className="btn btn-icon" title="删除采集组" aria-label={`删除采集组 ${index + 1}`} disabled={total <= 1} onClick={onRemove}>
          <Trash2 size={14} />
        </button>
      </div>

      <div className="topic-group-selected">
        {group.topics.length ? group.topics.map(topic => {
          const option = field.optionEntries.find(item => item.value === topic);
          return (
            <span className="topic-group-chip" key={topic}>
              <strong>{option?.label || topic}</strong>
              <small>{topic}</small>
              <button type="button" aria-label={`移除 ${topic}`} onClick={() => onChange({ ...group, topics: group.topics.filter(item => item !== topic) })}>
                <X size={12} />
              </button>
            </span>
          );
        }) : <small className="topic-group-empty">还没有标签，从下方标签库选择或添加自定义 topic。</small>}
      </div>

      <div className="topic-group-picker">
        <input value={query} placeholder="搜索标签：中文名、slug、说明" onChange={event => setQuery(event.target.value)} />
        <div className="topic-group-options">
          {options.map(option => (
            <button type="button" className="topic-option" key={option.value} onClick={() => addTopic(option.value)}>
              <span><strong>{option.label}</strong><small>{option.value}</small></span>
              {option.source ? <em>{option.source === 'custom' ? '自定义' : '系统'}</em> : null}
            </button>
          ))}
          {!options.length ? <small className="topic-group-empty">没有匹配的标签</small> : null}
        </div>
      </div>

      <div className="topic-group-custom">
        <input value={custom} placeholder="自定义 topic，例如 motion-graphics" spellCheck={false} onChange={event => { setCustom(event.target.value); setError(''); }} onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); addCustom(); } }} />
        <button type="button" className="btn" disabled={!custom.trim()} onClick={addCustom}>添加</button>
      </div>
      {error ? <small className="workflow-start-field-error" role="alert">{error}</small> : null}
    </section>
  );
}

export function TopicGroupsBudgetPreview({ fields, values }: {
  fields: WorkflowStartField[];
  values: Record<string, unknown>;
}) {
  const plan = topicGroupsSelectionPlan(fields, values);
  if (!plan) return null;
  const blocked = plan.queryCount > plan.maxQueries;
  return (
    <div className={`workflow-theme-budget${blocked ? ' is-blocked' : ''}`} aria-live="polite">
      <div className="workflow-theme-budget-head">
        <strong>预计 {plan.queryCount}/{plan.maxQueries} 次查询</strong>
        <span>{plan.groups.length}/{plan.maxGroups} 个采集组 · 未鉴权约 {plan.estimatedSeconds} 秒</span>
      </div>
      <p>去重后 {plan.resolvedTopics.length} 个 topic；同一标签跨组只查询一次。</p>
      {blocked ? <p className="workflow-theme-budget-error">超过查询上限，请减少组内标签。</p> : null}
    </div>
  );
}
