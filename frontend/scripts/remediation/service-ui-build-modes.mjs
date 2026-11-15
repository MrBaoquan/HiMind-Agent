import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  cpSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync,
  realpathSync, rmSync, statSync, utimesSync, writeFileSync,
} from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
import { createRuntime, workspace } from './service-ui-runtime.mjs';

// This entry always requires the task's host root. The ordinary build below
// runs only in its generated copy, with the variable removed from that child.
const runtime = createRuntime('modes');
const evidence = runtime.directory;
const copy = resolve(evidence, 's');
const external = resolve(evidence, 'o');
const localDist = resolve(copy, 'frontend/dist');
const checks = [];
const scenarios = [];
const inputs = ['src', 'scripts', 'styles.css', 'package.json', 'package-lock.json', 'index.html', 'vite.config.ts', 'tsconfig.json'];
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const save = (name, value) => writeFileSync(resolve(evidence, name), JSON.stringify(value, null, 2));
let active;

function within(parent, child) {
  const suffix = relative(parent, resolve(child));
  assert.ok(suffix && !isAbsolute(suffix) && suffix !== '..' && !suffix.startsWith(`..${sep}`), `Outside generated root: ${child}`);
}

function files(directory) {
  return readdirSync(directory).sort().flatMap(name => {
    const path = resolve(directory, name);
    assert.ok(!lstatSync(path).isSymbolicLink(), `Symlink not permitted in build inputs/outputs: ${path}`);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}

function sourceManifest(base) {
  return Object.fromEntries(inputs.flatMap(name => {
    const path = resolve(base, 'frontend', name);
    return statSync(path).isDirectory() ? files(path) : [path];
  }).sort().map(path => [relative(base, path).replaceAll('\\', '/'), hash(readFileSync(path))]));
}

function stopChild() {
  if (!active || active.exitCode !== null) return;
  if (process.platform === 'win32') {
    const result = spawnSync('taskkill', ['/pid', String(active.pid), '/t', '/f'], { windowsHide: true, encoding: 'utf8' });
    save('timeout-process-cleanup.json', { pid: active.pid, exit_code: result.status, error: result.error?.message, stdout: result.stdout, stderr: result.stderr });
  } else {
    try { process.kill(-active.pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
  }
}

async function command(label, text, env) {
  const started = Date.now();
  let stdout = '', stderr = '', timedOut = false, timer;
  try {
    const windows = process.platform === 'win32';
    active = spawn(windows ? (process.env.ComSpec || 'cmd.exe') : '/bin/sh', windows ? ['/d', '/s', '/c', text] : ['-c', text], {
      cwd: copy, env, windowsHide: true, detached: !windows, stdio: ['ignore', 'pipe', 'pipe'],
    });
    active.stdout.on('data', chunk => { stdout += chunk; });
    active.stderr.on('data', chunk => { stderr += chunk; });
    timer = setTimeout(() => { timedOut = true; stopChild(); }, 240000);
    const exit = await new Promise((accept, reject) => {
      active.once('error', reject);
      active.once('close', (code, signal) => accept({ code, signal }));
    });
    const record = {
      command: text, cwd: copy, exit_code: Number.isInteger(exit.code) ? exit.code : 1,
      signal: exit.signal, timed_out: timedOut, started_at: new Date(started).toISOString(),
      finished_at: new Date().toISOString(), evidence: resolve(evidence, `${label}.log`),
      SERVICE_UI_RUNTIME_ROOT: env.SERVICE_UI_RUNTIME_ROOT ?? null,
      classification: 'current-task-generated-source-copy', test_summary: null,
    };
    writeFileSync(record.evidence, stdout + stderr);
    record.output_sha256 = hash(readFileSync(record.evidence));
    checks.push(record);
    save('commands.json', checks);
    assert.ok(!timedOut && exit.signal === null, `${label}: command timed out or was interrupted`);
    return { ...record, stdout, stderr, started };
  } catch (error) {
    writeFileSync(resolve(evidence, `${label}-error.txt`), String(error.stack) + '\n' + stdout + stderr);
    throw error;
  } finally {
    clearTimeout(timer);
    stopChild();
    active = undefined;
  }
}

function cleanDist() {
  within(evidence, localDist);
  mkdirSync(localDist, { recursive: true });
  const stale = resolve(localDist, 'index.html');
  writeFileSync(stale, 'STALE_BUILD_MUST_NOT_SATISFY_ASSERTIONS');
  utimesSync(stale, new Date('2000-01-01'), new Date('2000-01-01'));
  rmSync(localDist, { recursive: true, force: true });
  assert.equal(existsSync(localDist), false, 'Old local output must be removed before the build');
}

function verifyOutput(directory, started) {
  within(evidence, directory);
  const index = resolve(directory, 'index.html');
  assert.ok(statSync(index).size > 0, 'index.html must be nonempty');
  const html = readFileSync(index, 'utf8');
  assert.doesNotMatch(html, /STALE_BUILD/);
  const assets = [...html.matchAll(/(?:src|href)=["']([^"']+\.(?:js|css)(?:\?[^"']*)?)["']/g)].map(match => match[1]);
  assert.ok(assets.length > 0, 'index.html must actually reference built JS/CSS assets');
  for (const asset of assets) {
    const path = resolve(directory, asset.split('?')[0].replace(/^\//, ''));
    within(directory, path);
    assert.ok(statSync(path).size > 0, `Referenced asset missing or empty: ${asset}`);
  }
  const manifest = files(directory).map(path => {
    const stat = statSync(path);
    assert.ok(stat.size > 0, `Empty output: ${path}`);
    assert.ok(stat.mtimeMs >= started, `Output predates build: ${path}`);
    return { path: relative(directory, path).replaceAll('\\', '/'), size: stat.size, mtime_ms: stat.mtimeMs, sha256: hash(readFileSync(path)) };
  });
  return { directory, references: assets, build_started_ms: started, files: manifest };
}

const onSignal = () => { stopChild(); process.exitCode = 1; };
process.on('SIGINT', onSignal);
process.on('SIGTERM', onSignal);
try {
  mkdirSync(resolve(copy, 'frontend'), { recursive: true });
  mkdirSync(external);
  const before = sourceManifest(workspace);
  save('source-origin.json', { workspace, copy, inputs, files: before });
  for (const input of inputs) cpSync(resolve(workspace, 'frontend', input), resolve(copy, 'frontend', input), { recursive: true, dereference: false, errorOnExist: true, force: false });
  assert.deepEqual(sourceManifest(copy), before, 'Generated copy must match current source');
  save('source-copy.json', sourceManifest(copy));
  const env = { ...process.env, npm_config_cache: resolve(evidence, 'npm-cache') };
  const install = await command('ci', 'npm --prefix frontend ci', env);
  assert.equal(install.exit_code, 0, install.stdout + install.stderr);

  cleanDist();
  const ordinaryEnv = { ...env };
  delete ordinaryEnv.SERVICE_UI_RUNTIME_ROOT;
  const ordinary = await command('ordinary', 'npm --prefix frontend run build', ordinaryEnv);
  assert.equal(ordinary.exit_code, 0, ordinary.stdout + ordinary.stderr);
  assert.doesNotMatch(ordinary.stdout, /SERVICE_UI_RUNTIME_DIRECTORY=/);
  const ordinaryOutput = verifyOutput(localDist, ordinary.started);
  const retainedDist = resolve(evidence, 'ordinary-dist');
  cpSync(localDist, retainedDist, { recursive: true, preserveTimestamps: true });
  assert.deepEqual(files(retainedDist).map(path => hash(readFileSync(path))), ordinaryOutput.files.map(file => file.sha256), 'Retained ordinary output must match its verified hashes');
  scenarios.push({ name: 'ordinary-absent-root', passed: true, output: ordinaryOutput, retained_directory: retainedDist, command_evidence: ordinary.evidence });
  save('scenarios.json', scenarios);

  cleanDist();
  const isolated = await command('explicit-valid', 'npm --prefix frontend run build', { ...env, SERVICE_UI_RUNTIME_ROOT: external });
  assert.equal(isolated.exit_code, 0, isolated.stdout + isolated.stderr);
  const directory = isolated.stdout.match(/^SERVICE_UI_RUNTIME_DIRECTORY=(.+)\r?$/m)?.[1].trim();
  assert.ok(directory, 'Explicit mode must report its actual external directory');
  within(realpathSync(external), realpathSync(directory));
  assert.equal(existsSync(localDist), false, 'Explicit output must not write local dist');
  const output = verifyOutput(resolve(directory, 'dist'), isolated.started);
  scenarios.push({ name: 'explicit-valid-root', passed: true, local_dist_exists: false, output, command_evidence: isolated.evidence });
  save('scenarios.json', scenarios);

  const notDirectory = resolve(evidence, 'file-root');
  writeFileSync(notDirectory, 'not a directory');
  const invalidRoots = [
    ['relative', 'relative-runtime'], ['missing', resolve(evidence, 'nonexistent')],
    ['file', notDirectory], ['workspace', copy], ['ancestor', evidence],
  ];
  for (const [name, location] of invalidRoots) {
    cleanDist();
    const failed = await command(`invalid-${name}`, 'npm --prefix frontend run build', { ...env, SERVICE_UI_RUNTIME_ROOT: location });
    assert.notEqual(failed.exit_code, 0, `Invalid explicit ${name} must fail`);
    assert.match(failed.stderr + failed.stdout, /SERVICE_UI_RUNTIME_ROOT|ENOENT/, 'Failure must come from runtime-root validation');
    assert.doesNotMatch(failed.stdout, /SERVICE_UI_RUNTIME_DIRECTORY=/);
    assert.equal(existsSync(localDist), false, 'Invalid explicit mode must not fall back to local dist');
    scenarios.push({ name: `explicit-invalid-${name}`, passed: true, build_exit_code: failed.exit_code, local_dist_exists: false, command_evidence: failed.evidence });
    save('scenarios.json', scenarios);
  }
  assert.deepEqual(sourceManifest(copy), before, 'Build must not rewrite copied source');
  assert.deepEqual(sourceManifest(workspace), before, 'Regression must not modify current candidate inputs');
  assert.deepEqual(scenarios.map(item => item.name), ['ordinary-absent-root', 'explicit-valid-root', ...invalidRoots.map(([name]) => `explicit-invalid-${name}`)]);
  save('build-modes.json', { passed: true, scenarios, checks, evidence, classification: 'generated-copy-build-regression-not-formal-VERIFY' });
  console.log(`Three build modes verified: ${resolve(evidence, 'build-modes.json')}`);
} catch (error) {
  save('build-modes.json', { passed: false, scenarios, checks, error: String(error.stack), evidence });
  throw error;
} finally {
  stopChild();
  process.removeListener('SIGINT', onSignal);
  process.removeListener('SIGTERM', onSignal);
  save('cleanup.json', { child_active: Boolean(active && active.exitCode === null), evidence_retained: true });
}
