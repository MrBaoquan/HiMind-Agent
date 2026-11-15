import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import test from 'node:test';
import { validateRuntimeLocation, workspace } from './service-ui-runtime.mjs';

test('missing and relative output roots fail without a fallback', () => {
  for (const location of [undefined, '', 'reports/service-ui', '../cache']) {
    assert.throws(() => validateRuntimeLocation(location), /explicitly authorized/);
  }
});

test('workspace descendants, traversal and ancestors are rejected', () => {
  for (const location of [workspace, resolve(workspace, 'reports/service-ui'), resolve(workspace, 'frontend/../reports'), resolve(workspace, '..')]) {
    assert.throws(() => validateRuntimeLocation(location), /independent/);
  }
});

test('lexically independent roots do not themselves grant filesystem authorization', () => {
  const sibling = `${workspace}-host-owned`;
  assert.equal(validateRuntimeLocation(sibling), sibling);
});

// The browser harness always needs host outputs. Ordinary build behavior is
// exercised by service-ui-build-modes.mjs in a generated source copy, never here.
for (const entry of ['service-ui-browser.mjs']) {
  test(`${entry} blocks before dependency loading when host output is absent`, () => {
    const environment = { ...process.env };
    delete environment.SERVICE_UI_RUNTIME_ROOT;
    const result = spawnSync(process.execPath, [resolve(workspace, 'frontend/scripts/remediation', entry)], {
      cwd: workspace, env: environment, encoding: 'utf8', timeout: 10000,
    });
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /SERVICE_UI_RUNTIME_ROOT/);
    assert.doesNotMatch(result.stdout, /SERVICE_UI_RUNTIME_DIRECTORY=/);
  });
}

test('explicitly invalid build roots still fail before dependency loading', () => {
  for (const location of ['', 'relative-runtime']) {
    const result = spawnSync(process.execPath, [resolve(workspace, 'frontend/scripts/remediation/service-ui-build.mjs')], {
      cwd: workspace, env: { ...process.env, SERVICE_UI_RUNTIME_ROOT: location }, encoding: 'utf8', timeout: 10000,
    });
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /SERVICE_UI_RUNTIME_ROOT/);
    assert.doesNotMatch(result.stdout, /SERVICE_UI_RUNTIME_DIRECTORY=/);
  }
});

test('measurement-only cannot return a successful required browser check', () => {
  const result = spawnSync(process.execPath, ['frontend/scripts/remediation/service-ui-browser.mjs'], {
    cwd: workspace, env: { ...process.env, SERVICE_UI_MEASURE_ONLY: '1' }, encoding: 'utf8', timeout: 10000,
  });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /Measurement-only mode cannot satisfy/);
});
