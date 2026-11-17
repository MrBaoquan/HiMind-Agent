// Keep the legacy command on the same production-component browser checks.
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const check = fileURLToPath(new URL('./check-mission-assignment-ui.mts', import.meta.url));
const result = spawnSync(process.execPath, ['--experimental-strip-types', check, '--browser'], { stdio: 'inherit' });
if (result.error) throw result.error;
process.exit(result.status ?? 1);
