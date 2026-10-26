import { strict as assert } from 'node:assert';
import { hydrateWorkflowOptionSources } from '../src/pages/workflowOptionSources.ts';

const storage = new Map<string, string>();
(globalThis as any).localStorage = {
  getItem: (key: string) => storage.get(key) ?? null,
  setItem: (key: string, value: string) => storage.set(key, value),
  removeItem: (key: string) => storage.delete(key),
};

function snapshot() {
  return {
    workflows: [{
      package: { id: 'com.example.workflow' },
      view: {
        sections: [{
          id: 'scope',
          title: '范围',
          fields: [{
            id: 'category_ids',
            label: '关注分类',
            type: 'theme-list',
            options: [{ value: 'system', label: '系统分类', topics: ['system-topic'] }],
            option_source: { kind: 'plugin_capability', capability_id: 'example.catalog' },
          }],
        }],
      },
    }],
  } as any;
}

const successful = await hydrateWorkflowOptionSources(snapshot(), async () => ({
  capability_id: 'example.catalog',
  options: [{ value: 'mine', label: '我的分类', topics: ['a', 'b'], source: 'user' }],
}));
const successField = successful.workflows[0].view!.sections[0].fields![0] as any;
assert.equal(successField.options[0].value, 'mine');
assert.equal(successField.option_source_warning, '');
assert.equal(storage.size, 1);

const cached = await hydrateWorkflowOptionSources(snapshot(), async () => {
  throw new Error('plugin offline');
});
const cachedField = cached.workflows[0].view!.sections[0].fields![0] as any;
assert.equal(cachedField.options[0].value, 'mine');
assert.equal(cachedField.option_source_cached, true);
assert.match(cachedField.option_source_warning, /最近缓存/);

storage.clear();
const fallback = await hydrateWorkflowOptionSources(snapshot(), async () => {
  throw new Error('plugin offline');
});
const fallbackField = fallback.workflows[0].view!.sections[0].fields![0] as any;
assert.equal(fallbackField.options[0].value, 'system');
assert.equal(fallbackField.option_source_cached, false);
assert.match(fallbackField.option_source_warning, /内置分类/);
console.log('workflow option source cache checks passed');
