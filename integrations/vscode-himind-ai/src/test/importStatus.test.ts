import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import { resolveImportStatusPath, writeImportStatus } from "../importStatus";
import { EnrollmentPayload } from "../protocol";

function credential(overrides: Partial<EnrollmentPayload & { connected_at: string }> = {}) {
  return {
    base_url: "https://ai.example.com/v1",
    api_key: "test-key",
    model: "glm-5.2",
    models: ["glm-5.2", "deepseek-v4-pro"],
    expires_at: 1_800_000_000,
    connected_at: "2026-08-17T01:00:02.000Z",
    ...overrides,
  };
}

test("writes model synchronization state to the configured Agent path", async (t) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "himind-vscode-status-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const statusPath = path.join(root, "vscode-import-status.json");

  await writeImportStatus(true, credential({ import_status_path: statusPath }), root);

  const status = JSON.parse(await fs.readFile(statusPath, "utf8")) as Record<string, unknown>;
  assert.equal(status.imported_at, "2026-08-17T01:00:02.000Z");
  assert.deepEqual(status.models, ["glm-5.2", "deepseek-v4-pro"]);
  assert.equal(typeof status.synced_at, "string");
});

test("migrates an existing legacy marker from the matching Agent profile only", async (t) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "himind-vscode-status-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const development = path.join(root, "HiMindAgent", "profiles", "development", "data", "vscode-import-status.json");
  const production = path.join(root, "HiMindAgent", "profiles", "production", "data", "vscode-import-status.json");
  await fs.mkdir(path.dirname(development), { recursive: true });
  await fs.mkdir(path.dirname(production), { recursive: true });
  await fs.writeFile(development, JSON.stringify({ imported_at: "2026-08-17T01:00:00.000Z" }));
  await fs.writeFile(production, JSON.stringify({ imported_at: "2026-08-16T01:00:00.000Z" }));

  assert.equal(await resolveImportStatusPath(credential(), root), development);
  await writeImportStatus(true, credential(), root);

  const migrated = JSON.parse(await fs.readFile(development, "utf8")) as Record<string, unknown>;
  assert.deepEqual(migrated.models, ["glm-5.2", "deepseek-v4-pro"]);
  assert.deepEqual(JSON.parse(await fs.readFile(production, "utf8")), { imported_at: "2026-08-16T01:00:00.000Z" });
});

test("mirrors a configured pre-profile status path into one existing Agent profile", async (t) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "himind-vscode-status-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const legacy = path.join(root, "HiMindAgent", "data", "vscode-import-status.json");
  const development = path.join(root, "HiMindAgent", "profiles", "development", "data", "vscode-import-status.json");
  await fs.mkdir(path.dirname(legacy), { recursive: true });
  await fs.mkdir(path.dirname(development), { recursive: true });
  await fs.writeFile(legacy, JSON.stringify({ imported_at: "2026-08-17T01:00:00.000Z" }));
  await fs.writeFile(development, JSON.stringify({ imported_at: "2026-08-16T01:00:00.000Z" }));

  await writeImportStatus(true, credential({ import_status_path: legacy }), root);

  for (const statusPath of [legacy, development]) {
    const status = JSON.parse(await fs.readFile(statusPath, "utf8")) as Record<string, unknown>;
    assert.deepEqual(status.models, ["glm-5.2", "deepseek-v4-pro"]);
    assert.equal(typeof status.synced_at, "string");
  }
});

test("removes an existing legacy marker when disconnecting", async (t) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "himind-vscode-status-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const statusPath = path.join(root, "HiMindAgent", "profiles", "development", "data", "vscode-import-status.json");
  await fs.mkdir(path.dirname(statusPath), { recursive: true });
  await fs.writeFile(statusPath, JSON.stringify({ imported_at: "2026-08-17T01:00:00.000Z" }));

  await writeImportStatus(false, credential(), root);

  await assert.rejects(() => fs.stat(statusPath), { code: "ENOENT" });
});

test("rejects an invalid configured status path", async () => {
  await assert.rejects(
    () => resolveImportStatusPath(credential({ import_status_path: "relative/status.json" })),
    /invalid VS Code import status path/,
  );
});
