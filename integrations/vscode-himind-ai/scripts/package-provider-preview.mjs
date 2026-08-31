import { mkdir } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const stageRoot = resolve(extensionRoot, "dist", "provider-preview");
const outputPath = resolve(extensionRoot, "dist", "himind-ai-provider-preview.vsix");
const vsceCli = resolve(extensionRoot, "node_modules", "@vscode", "vsce", "vsce");

await mkdir(dirname(outputPath), { recursive: true });
const result = spawnSync(
  process.execPath,
  [vsceCli, "package", "--no-dependencies", "-o", outputPath],
  { cwd: stageRoot, stdio: "inherit" }
);
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`Provider preview packaging failed with exit code ${result.status}`);

console.log(outputPath);
