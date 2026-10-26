import type { RefObject } from 'react';
import type { CredentialFieldView } from '../services/agentApi';

export function CredentialFieldsEditor({
  fields,
  drafts,
  disabled = false,
  firstInputRef,
  onChange,
}: {
  fields: CredentialFieldView[];
  drafts: Record<string, string>;
  disabled?: boolean;
  firstInputRef?: RefObject<HTMLInputElement | null>;
  onChange: (key: string, value: string) => void;
}) {
  return (
    <div className="credential-fields-editor">
      {fields.map((field, index) => {
        const configured = field.configured;
        const placeholder = configured && field.type !== 'text' && field.type !== 'url' && field.type !== 'file'
          ? '已保存，留空则不修改'
          : field.placeholder || (field.required ? '必填' : '选填');
        return (
          <label key={field.key} className="field-group">
            <span className="field-label">
              {field.label}
              {field.required ? <span className="credential-field-required">必填</span> : null}
            </span>
            <input
              ref={index === 0 ? firstInputRef : undefined}
              autoComplete="off"
              type={field.secret ? 'password' : field.type === 'url' ? 'url' : 'text'}
              value={drafts[field.key] ?? (field.secret ? '' : field.value)}
              disabled={disabled}
              placeholder={placeholder}
              onChange={event => onChange(field.key, event.target.value)}
            />
            {field.help ? <span className="field-hint">{field.help}</span> : null}
          </label>
        );
      })}
    </div>
  );
}
