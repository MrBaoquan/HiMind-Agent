import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { createRuntime, workspace } from './service-ui-runtime.mjs';

// Only an absent variable selects the ordinary release build. An explicitly
// configured (even empty or invalid) root must still fail closed in createRuntime.
const runtime = process.env.SERVICE_UI_RUNTIME_ROOT === undefined ? null : createRuntime('build');
const require = createRequire(import.meta.url);
// Resolve the public package metadata, then its declared executable. TypeScript
// 7 exports package.json but does not export the bin/tsc package subpath.
const typescriptPackage = require.resolve('typescript/package.json');
const compiler = resolve(dirname(typescriptPackage), require(typescriptPackage).bin.tsc);
const compilation = spawnSync(process.execPath, [compiler, '--noEmit'], {
  cwd: resolve(workspace, 'frontend'), env: process.env, stdio: 'inherit',
});
if (compilation.error) throw compilation.error;
if (compilation.status !== 0) {
  process.exitCode = compilation.status || 1;
} else {
  const { build } = await import('vite');
  const { default: configuration } = await import('../../vite.config.ts');
  await build({
    ...configuration,
    root: resolve(workspace, 'frontend'),
    configFile: false,
    ...(runtime ? { cacheDir: runtime.cache } : {}),
    build: {
      ...configuration.build,
      outDir: runtime ? resolve(runtime.directory, 'dist') : resolve(workspace, 'frontend/dist'),
      emptyOutDir: true,
    },
  });
}
