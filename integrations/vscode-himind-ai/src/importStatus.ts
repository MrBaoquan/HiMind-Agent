import { promises as fs } from "node:fs";
import * as path from "node:path";
import { EnrollmentPayload } from "./protocol";

const IMPORT_STATUS_FILE = "vscode-import-status.json";
const PROFILE_MATCH_WINDOW_MS = 5 * 60 * 1_000;

type ImportStatusCredential = EnrollmentPayload & { connected_at?: string };

type StatusCandidate = {
  path: string;
  importedAt: string;
  modifiedAt: number;
};

export async function writeImportStatus(
  imported: boolean,
  credential?: ImportStatusCredential,
  localAppData = process.env.LOCALAPPDATA,
): Promise<void> {
  const statusPaths = await resolveImportStatusPaths(credential, localAppData);
  if (!statusPaths.length) return;
  if (!imported) {
    await Promise.all(statusPaths.map((statusPath) =>
      fs.unlink(statusPath).catch((error: NodeJS.ErrnoException) => {
        if (error.code !== "ENOENT") throw error;
      })
    ));
    return;
  }

  const now = new Date().toISOString();
  await Promise.all(statusPaths.map(async (statusPath) => {
    await fs.mkdir(path.dirname(statusPath), { recursive: true });
    const existingImportedAt = await readImportedAt(statusPath);
    await fs.writeFile(statusPath, JSON.stringify({
      imported_at: existingImportedAt || credential?.connected_at || now,
      synced_at: now,
      models: credential?.models ?? [],
    }), "utf8");
  }));
}

export async function resolveImportStatusPath(
  credential?: ImportStatusCredential,
  localAppData = process.env.LOCALAPPDATA,
): Promise<string | undefined> {
  return (await resolveImportStatusPaths(credential, localAppData))[0];
}

export async function resolveImportStatusPaths(
  credential?: ImportStatusCredential,
  localAppData = process.env.LOCALAPPDATA,
): Promise<string[]> {
  const configuredPath = credential?.import_status_path?.trim();
  if (configuredPath) {
    const statusPath = path.resolve(configuredPath);
    if (!path.isAbsolute(configuredPath) || path.basename(statusPath) !== IMPORT_STATUS_FILE) {
      throw new Error("HiMind Agent returned an invalid VS Code import status path");
    }
    if (!localAppData?.trim() || !samePath(statusPath, legacyStatusPath(localAppData))) {
      return [statusPath];
    }
    const profile = selectCandidate(
      credential,
      (await discoverExistingStatusFiles(localAppData)).filter((candidate) => !samePath(candidate.path, statusPath)),
    );
    return profile ? [statusPath, profile.path] : [statusPath];
  }
  if (!localAppData?.trim()) return [];

  const candidates = await discoverExistingStatusFiles(localAppData);
  const selected = selectCandidate(credential, candidates);
  return selected ? [selected.path] : [];
}

function selectCandidate(
  credential: ImportStatusCredential | undefined,
  candidates: StatusCandidate[],
): StatusCandidate | undefined {
  if (!candidates.length) return undefined;
  const connectedAt = Date.parse(credential?.connected_at ?? "");
  if (Number.isFinite(connectedAt)) {
    const nearest = [...candidates].sort((left, right) =>
      Math.abs(Date.parse(left.importedAt) - connectedAt) - Math.abs(Date.parse(right.importedAt) - connectedAt)
    )[0];
    if (nearest?.importedAt && Math.abs(Date.parse(nearest.importedAt) - connectedAt) <= PROFILE_MATCH_WINDOW_MS) {
      return nearest;
    }
  }
  return candidates.sort((left, right) => right.modifiedAt - left.modifiedAt)[0];
}

async function discoverExistingStatusFiles(localAppData: string): Promise<StatusCandidate[]> {
  const agentRoot = path.join(localAppData, "HiMindAgent");
  const candidates = [legacyStatusPath(localAppData)];
  try {
    const profiles = await fs.readdir(path.join(agentRoot, "profiles"), { withFileTypes: true });
    for (const profile of profiles) {
      if (profile.isDirectory()) {
        candidates.push(path.join(agentRoot, "profiles", profile.name, "data", IMPORT_STATUS_FILE));
      }
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }

  const existing = await Promise.all(candidates.map(async (candidate): Promise<StatusCandidate | undefined> => {
    try {
      const stat = await fs.stat(candidate);
      if (!stat.isFile()) return undefined;
      return { path: candidate, importedAt: await readImportedAt(candidate), modifiedAt: stat.mtimeMs };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
      throw error;
    }
  }));
  return existing.filter((candidate): candidate is StatusCandidate => candidate !== undefined);
}

function legacyStatusPath(localAppData: string): string {
  return path.join(localAppData, "HiMindAgent", "data", IMPORT_STATUS_FILE);
}

function samePath(left: string, right: string): boolean {
  return path.resolve(left).toLocaleLowerCase() === path.resolve(right).toLocaleLowerCase();
}

async function readImportedAt(statusPath: string): Promise<string> {
  try {
    const parsed = JSON.parse(await fs.readFile(statusPath, "utf8")) as { imported_at?: unknown };
    return typeof parsed.imported_at === "string" ? parsed.imported_at : "";
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return "";
    return "";
  }
}
