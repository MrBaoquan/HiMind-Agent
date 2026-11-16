import test from 'node:test';
import assert from 'node:assert/strict';
import { createAiClient, brokerTransport, viewTransport } from './client.mjs';

function fixture(pluginId = 'example', projectId = 'one', shared = new Map()) {
  const calls = [];
  const storage = { getItem: key => shared.get(key) ?? null, setItem: (key, value) => shared.set(key, value), removeItem: key => shared.delete(key) };
  const client = createAiClient({ pluginId, projectId, storage, invoke: async (capability, input) => { calls.push({ capability, input }); return { selected: { eligible: true, service_id: 'a', model: 'image-two' } }; } });
  return { client, calls, shared };
}

test('invocation > project > plugin > Agent defaults, scoped by capability/plugin/project', async () => {
  const { client, calls, shared } = fixture();
  const image = 'media.image.generate';
  assert.equal(client.effective(image).selection_source, 'agent_default');
  client.save(image, { selection: { mode: 'service_default', service_id: 'a' } }, 'plugin');
  assert.equal(client.effective(image).selection_source, 'plugin');
  client.save(image, { selection: { mode: 'pinned', service_id: 'b', model: 'chosen' } }, 'project');
  assert.equal(client.effective(image).selection_source, 'project');
  assert.equal(fixture('example', 'two', shared).client.effective(image).selection_source, 'plugin');
  assert.equal(fixture('different', 'one', shared).client.effective(image).selection_source, 'agent_default');
  assert.equal(client.effective('media.audio.speech').selection_source, 'agent_default');
  await client.execute(image, { prompt: 'cat' }, { selection: { mode: 'agent_default' } });
  assert.equal(calls.at(-1).input.selection.mode, 'agent_default');
  assert.equal(calls.at(-1).input.selection_source, 'invocation');
  await client.execute(image, { prompt: 'cat' });
  assert.deepEqual(calls.at(-1).input.selection, { mode: 'pinned', service_id: 'b', model: 'chosen' });
  assert.equal(calls.at(-1).input.selection_source, 'project');
  assert.ok(calls.every(call => !call.capability.includes('.set')));
  assert.throws(() => client.execute(image, { model: 'hidden', prompt: 'cat' }), /选择参数/);
  assert.throws(() => fixture('example', null, shared).client.clear(image), /不可清除/);
});

test('batch pin fixes model and disables fallback; unsupported voices stay explicit', async () => {
  const { client } = fixture();
  const pin = await client.pin('media.image.generate');
  assert.deepEqual(pin, { selection: { mode: 'pinned', service_id: 'a', model: 'image-two' }, fallback: 'none' });
  const voice = createAiClient({ pluginId: 'voice', storage: null, invoke: async capability => capability === 'media.route.plan'
    ? { selected: { eligible: true, service_id: 'a', model: 'tts' } }
    : { providers: [{ id: 'a', media_capabilities: [{ capability: 'audio.speech', voice_catalog: 'unsupported' }] }] } });
  assert.equal((await voice.voices()).state, 'unsupported');
});

test('view uses host identity and broker uses loopback declared-capability endpoint', async () => {
  let seen;
  const view = viewTransport(async (command, args) => { seen = { command, args }; return {}; });
  await view('media.route.plan', { capability: 'image.generate' });
  assert.deepEqual(seen, { command: 'invoke_plugin_view_capability', args: { capabilityId: 'media.route.plan', input: { capability: 'image.generate' } } });
  assert.throws(() => brokerTransport({ url: 'https://example.com', token: 'test' }), /本机/);
  const broker = brokerTransport({ url: 'http://127.0.0.1:1234/invoke', token: 'test-only', fetch: async (url, options) => {
    seen = { url: String(url), ...options }; return { ok: true, json: async () => ({ result: { state: 'resolved' } }) };
  } });
  assert.deepEqual(await broker('media.route.plan', { capability: 'audio.speech' }), { state: 'resolved' });
  assert.equal(seen.url, 'http://127.0.0.1:1234/invoke');
  assert.equal(seen.headers.Authorization, 'Bearer test-only');
  assert.equal(JSON.parse(seen.body).capability, 'media.route.plan');
});
