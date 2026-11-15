import { mkdirSync, mkdtempSync, realpathSync, statSync } from 'node:fs';
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const workspace = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');

export function validateRuntimeLocation(location, candidateRoot = workspace) {
  if (!location || !isAbsolute(location)) {
    throw new Error('SERVICE_UI_RUNTIME_ROOT must name an existing host-provided, explicitly authorized absolute output directory; no reports/service-ui fallback is permitted.');
  }
  const output = resolve(location);
  const candidate = resolve(candidateRoot);
  for (const [parent, child] of [[candidate, output], [output, candidate]]) {
    const suffix = relative(parent, child);
    if (!suffix || (!isAbsolute(suffix) && suffix !== '..' && !suffix.startsWith(`..${sep}`))) {
      throw new Error('SERVICE_UI_RUNTIME_ROOT must be independent of the workspace, not its ancestor or descendant.');
    }
  }
  return output;
}

export function requireRuntimeRoot() {
  const requested = validateRuntimeLocation(process.env.SERVICE_UI_RUNTIME_ROOT);
  const canonical = realpathSync(requested);
  validateRuntimeLocation(canonical, realpathSync(workspace));
  if (!statSync(canonical).isDirectory()) throw new Error('SERVICE_UI_RUNTIME_ROOT is not a directory.');
  return canonical;
}

export function createRuntime(kind) {
  const hostRoot = requireRuntimeRoot();
  // Host roots already include two content hashes. Leave room for the browser's
  // own profile/artifact names on Windows, without moving outside that root.
  const directory = mkdtempSync(resolve(hostRoot, `${kind.slice(0, 1)}-`));
  const temporary = resolve(directory, 't');
  const cache = resolve(directory, 'v');
  mkdirSync(temporary);
  mkdirSync(cache);
  Object.assign(process.env, {
    TEMP: temporary,
    TMP: temporary,
    TMPDIR: temporary,
    npm_config_cache: resolve(directory, 'npm-cache'),
    npm_config_update_notifier: 'false',
  });
  console.log(`SERVICE_UI_RUNTIME_DIRECTORY=${directory}`);
  return { directory, temporary, cache };
}
