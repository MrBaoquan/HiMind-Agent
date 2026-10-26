import { useState } from 'react';
import { Plus, X } from 'lucide-react';
import { CUSTOM_TOPIC_PATTERN, themeSelectionPlan, type WorkflowStartField } from '../pages/workflowStartForm';

export function ThemeMultiSelect({ field, value, onChange }: {
  field: WorkflowStartField;
  value: unknown;
  onChange: (value: string[]) => void;
}) {
  const [query, setQuery] = useState('');
  const selected = new Set(Array.isArray(value) ? value.map(String) : []);
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const visibleOptions = normalizedQuery
    ? field.optionEntries.filter(option => [option.label, option.value, option.source, option.description].join(' ').toLocaleLowerCase().includes(normalizedQuery))
    : field.optionEntries;
  return (
    <div className="workflow-theme-picker-wrap">
      {field.optionEntries.length > 6 ? (
        <input
          className="workflow-theme-search"
          value={query}
          placeholder="搜索分类名称或 topic"
          onChange={event => setQuery(event.target.value)}
        />
      ) : null}
      {field.optionSourceWarning ? <small className="workflow-theme-source-warning">{field.optionSourceWarning}</small> : null}
      <div className="workflow-theme-picker" role="group" aria-label={field.label}>
        {visibleOptions.map(option => {
          const active = selected.has(option.value);
          return (
            <button
              type="button"
              key={option.value}
              className={`workflow-theme-option${active ? ' active' : ''}`}
              aria-pressed={active}
              data-theme-id={option.value}
              title={option.description || option.topics.join(' · ')}
              onClick={() => {
                const next = new Set(selected);
                if (active) next.delete(option.value);
                else next.add(option.value);
                onChange(Array.from(next));
              }}
            >
              <span className="workflow-theme-option-head">
                <strong>{option.label}</strong>
                {option.source ? <em>{option.source === 'user' ? '我的' : '系统'}</em> : null}
              </span>
              <small>{option.topics.length ? `${option.topics.length} 个标签 · ${option.topics.length * 2} 次查询` : option.value}</small>
            </button>
          );
        })}
        {!visibleOptions.length ? <small className="workflow-theme-empty">没有匹配的分类</small> : null}
      </div>
    </div>
  );
}

export function TopicTagsInput({ field, value, onChange }: {
  field: WorkflowStartField;
  value: unknown;
  onChange: (value: string[]) => void;
}) {
  const items = Array.isArray(value) ? value.map(String) : [];
  const [draft, setDraft] = useState('');
  const [error, setError] = useState('');

  function addDraft() {
    const candidates = draft
      .split(/[,，\s]+/)
      .map(item => item.trim().toLowerCase())
      .filter(Boolean);
    if (!candidates.length) return;
    const invalid = candidates.find(item => !CUSTOM_TOPIC_PATTERN.test(item));
    if (invalid) {
      setError(`「${invalid}」不是合法 GitHub topic；只允许小写字母、数字和连字符`);
      return;
    }
    onChange(Array.from(new Set([...items, ...candidates])));
    setDraft('');
    setError('');
  }

  return (
    <div className="workflow-topic-tags">
      {items.length ? (
        <div className="workflow-topic-tag-list">
          {items.map(item => (
            <span key={item} className="workflow-topic-tag">
              {item}
              <button type="button" title={`移除 ${item}`} aria-label={`移除 ${item}`} onClick={() => onChange(items.filter(value => value !== item))}>
                <X size={12} />
              </button>
            </span>
          ))}
        </div>
      ) : null}
      <div className="workflow-topic-add">
        <input
          value={draft}
          placeholder={field.placeholder || '输入 GitHub topic，例如 remotion'}
          spellCheck={false}
          onChange={event => { setDraft(event.target.value); setError(''); }}
          onKeyDown={event => {
            if (event.key === 'Enter' || event.key === ',') {
              event.preventDefault();
              addDraft();
            }
          }}
        />
        <button type="button" className="btn" disabled={!draft.trim()} onClick={addDraft}>
          <Plus size={13} />添加
        </button>
      </div>
      {error ? <small className="workflow-start-field-error" role="alert">{error}</small> : null}
    </div>
  );
}

export function ThemeBudgetPreview({ fields, values }: {
  fields: WorkflowStartField[];
  values: Record<string, unknown>;
}) {
  const plan = themeSelectionPlan(fields, values);
  if (!plan) return null;
  const blocked = plan.queryCount > plan.maxQueries;
  const empty = !plan.selectedThemeIds.length && !plan.customTopics.length;
  return (
    <div className={`workflow-theme-budget${blocked ? ' is-blocked' : empty ? ' is-empty' : ''}`} aria-live="polite">
      <div className="workflow-theme-budget-head">
        <strong>{empty ? '尚未选择分类' : `预计 ${plan.queryCount}/${plan.maxQueries} 次查询`}</strong>
        <span>{empty ? '选 1 个即单分类，选多个即多分类' : `未鉴权约 ${plan.estimatedSeconds} 秒`}</span>
      </div>
      {plan.selectedThemeLabels.length ? <p>分类：{plan.selectedThemeLabels.join('、')}</p> : null}
      {plan.customTopics.length ? <p>额外 topic：{plan.customTopics.join('、')}</p> : null}
      {plan.resolvedTopics.length ? <p className="workflow-theme-budget-topics">Topics：{plan.resolvedTopics.join(' · ')}</p> : null}
      {blocked ? <p className="workflow-theme-budget-error">超过查询上限，请减少分类或额外 topic。</p> : null}
    </div>
  );
}