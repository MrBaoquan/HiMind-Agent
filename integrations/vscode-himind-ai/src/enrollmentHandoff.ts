import { promises as fs } from "node:fs";
import * as path from "node:path";

const HANDOFF_FILE = "vscode-enrollment-v2.json";

/**
 * Agent profiles keep their handoff files next to profile-scoped state. The
 * legacy data directory remains supported for production installations and
 * older Agents. Newest-first ordering prevents an old profile from replacing
 * a request the user just made in another profile.
 */
export async function discoverEnrollmentHandoffs(localAppData: string): Promise<string[]> {
  const root = path.join(localAppData, "HiMindAgent");
  const candidates = new Set<string>([
    path.join(root, "data", HANDOFF_FILE),
  ]);
  try {
    const profiles = await fs.readdir(path.join(root, "profiles"), { withFileTypes: true });
    for (const profile of profiles) {
      if (profile.isDirectory()) {
        candidates.add(path.join(root, "profiles", profile.name, "data", HANDOFF_FILE));
      }
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }

  const existing = await Promise.all([...candidates].map(async (candidate) => {
    try {
      const stat = await fs.stat(candidate);
      return stat.isFile() ? { candidate, modifiedAt: stat.mtimeMs } : undefined;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
      throw error;
    }
  }));
  return existing
    .filter((item): item is { candidate: string; modifiedAt: number } => item !== undefined)
    .sort((left, right) => right.modifiedAt - left.modifiedAt)
    .map((item) => item.candidate);
}
