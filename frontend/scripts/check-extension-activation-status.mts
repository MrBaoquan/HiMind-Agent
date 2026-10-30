import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const page = readFileSync(join(root, 'src/pages/ExtensionDevelopmentPage.tsx'), 'utf8');
const main = readFileSync(join(root, 'src/main.tsx'), 'utf8');
const css = readFileSync(join(root, 'styles.css'), 'utf8');

for (const label of ['候选：', '运行时：', '可见性：', '发布：']) {
  assert.ok(page.includes(label), `activation facts must expose ${label}`);
}
assert.ok(page.includes('title={`${project.name} · ${listMeta(project)}`}'), 'truncated project rows must expose the full name on hover');
assert.match(page, /replace\(\/\^\\\/\\\/\\\?\\\/UNC\\\//, 'Windows verbatim UNC paths must normalize');
assert.match(page, /replace\(\/\^\\\/\\\/\\\?\\\//, 'Windows verbatim drive paths must normalize');
assert.match(page, /clients\.length \? `\$\{clients\.length\} 个客户端`/, 'visibility facts must summarize clients instead of listing every client');
assert.match(page, /title=\{visibilityTitle \|\| undefined\}/, 'visibility facts must keep the complete surface list in a tooltip');
assert.ok(!page.includes('恢复上一开发版'), 'development detail must not expose the rollback action');
assert.ok(!page.includes('onRefreshActivationSurfaces'), 'rollback-only refresh wiring must be removed with the action');
assert.match(main, /singleFlight\('development',[\s\S]*?\{ force \}\)/, 'development refresh must honor force');
assert.match(main, /refreshDevelopment\(true\)/, 'build completion must use a forced development refresh');
assert.match(css, /\.development-build-summary > \.development-activation-facts > span \{[^}]*width: auto;[^}]*height: auto;[^}]*min-width: 0;[^}]*max-width: 100%;[^}]*flex: 0 1 auto;[^}]*overflow: hidden;[^}]*text-overflow: ellipsis;/, 'fact chips must shrink and ellipsize instead of overflowing the detail pane');
assert.match(css, /\.development-detail-header \{[^}]*display: flex;/, 'development detail header must remain a flex layout');
assert.match(css, /@container agent-main \(max-width: 880px\) \{[\s\S]*?\.development-rail-head \{ display: none; \}/, 'compact workspace rail must not consume vertical reading space');
assert.match(css, /@container agent-main \(max-width: 880px\) \{[\s\S]*?\.instruction-panel-head \{ min-height: 36px; \}/, 'compact project rules must retain a usable detail viewport');
assert.match(css, /@container detail-pane \(min-width: 500px\) and \(max-width: 640px\) \{[\s\S]*?\.development-detail-header \{[^}]*flex-direction: row;/, 'minimum desktop detail header must keep title and actions on one row');
assert.match(css, /@container detail-pane \(min-width: 500px\) and \(max-width: 640px\) \{[\s\S]*?\.development-detail-actions \{[^}]*width: auto;[^}]*margin: 0 0 0 auto;/, 'minimum desktop actions must not reserve a second row');

console.log('extension activation UI checks passed');
