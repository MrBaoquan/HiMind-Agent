import { useEffect, useState, type CSSProperties } from 'react';
import type { ExtensionIconInfo } from '../services/agentApi';
import type { ExtensionKind } from '../data/extensionKinds';
import { extensionKindIcons } from './ExtensionKindMark';

type ExtensionIconProps = {
  kind: ExtensionKind;
  name: string;
  icon?: ExtensionIconInfo | null;
  size?: number;
  /** Pass when the icon is the only visible identity for the extension. */
  label?: string;
};

/**
 * Brand icon for a concrete extension. The type glyph remains the fallback so a
 * missing, malformed, or removed image never leaves an empty square.
 */
export function ExtensionIcon({ kind, name, icon, size = 44, label }: ExtensionIconProps) {
  const [failedUrl, setFailedUrl] = useState('');
  const TypeIcon = extensionKindIcons[kind];
  const renderedSize = Math.max(32, size);
  const hasBrandIcon = Boolean(icon?.url && failedUrl !== icon.url);
  useEffect(() => {
    setFailedUrl('');
  }, [icon?.url]);
  const accessibleLabel = label ? `${name}（${label}）` : '';
  const accessible = label
    ? { role: 'img' as const, 'aria-label': accessibleLabel, title: accessibleLabel }
    : { 'aria-hidden': true as const };
  return (
    <span
      className={`extension-icon-frame ${kind} ${hasBrandIcon ? 'has-brand-icon' : 'is-type-fallback'}`}
      style={{ '--extension-icon-size': `${renderedSize}px` } as CSSProperties}
      data-icon-kind={kind}
      data-icon-source={hasBrandIcon ? icon?.source : 'fallback'}
      {...accessible}
    >
      <span className="extension-icon-art">
        {hasBrandIcon ? (
          <img
            src={icon?.url}
            alt=""
            draggable={false}
            decoding="async"
            onError={() => setFailedUrl(icon?.url || '')}
          />
        ) : <TypeIcon size={Math.max(18, Math.round(renderedSize * 0.48))} strokeWidth={1.9} aria-hidden="true" />}
      </span>
      {hasBrandIcon ? (
        <span className={`extension-icon-type-badge ${kind}`} aria-hidden="true">
          <TypeIcon size={Math.max(12, Math.round(renderedSize * 0.25))} strokeWidth={2.1} />
        </span>
      ) : null}
    </span>
  );
}
