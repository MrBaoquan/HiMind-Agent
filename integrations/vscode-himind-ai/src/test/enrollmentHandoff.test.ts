import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import { discoverEnrollmentHandoffs } from "../enrollmentHandoff";

test("discovers legacy and profile-scoped Agent handoffs newest first", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "himind-handoff-"));
  try {
    const legacy = path.join(root, "HiMindAgent", "data", "vscode-enrollment-v2.json");
    const development = path.join(root, "HiMindAgent", "profiles", "development", "data", "vscode-enrollment-v2.json");
    const production = path.join(root, "HiMindAgent", "profiles", "production", "data", "vscode-enrollment-v2.json");
    await fs.mkdir(path.dirname(legacy), { recursive: true });
    await fs.mkdir(path.dirname(development), { recursive: true });
    await fs.mkdir(path.dirname(production), { recursive: true });
    await fs.writeFile(legacy, "legacy");
    await fs.writeFile(development, "development");
    await fs.writeFile(production, "production");
    const now = Date.now() / 1000;
    await fs.utimes(legacy, now - 30, now - 30);
    await fs.utimes(development, now - 20, now - 20);
    await fs.utimes(production, now - 10, now - 10);

    assert.deepEqual(await discoverEnrollmentHandoffs(root), [production, development, legacy]);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
