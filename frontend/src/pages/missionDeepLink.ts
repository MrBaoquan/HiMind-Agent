/**
 * Mission deep-link parsing shared by the main-window and Tauri event paths.
 *
 * Mission IDs arrive from an OS protocol URL. Keep this validation pure so the
 * security boundary is directly testable and both entry paths cannot drift.
 */
export type MissionOpenBootstrap = { mission_id?: unknown };

export const MISSION_OPEN_EVENT = 'himind:mission-navigate';

export function validMissionId(value: unknown): string {
  if (typeof value !== 'string') return '';
  const missionId = value.trim();
  return missionId.length > 0 && missionId.length <= 160 && /^[A-Za-z0-9_-]+$/.test(missionId) ? missionId : '';
}

export function missionIdFromBootstrap(value: unknown): string {
  if (!value || typeof value !== 'object') return '';
  return validMissionId((value as MissionOpenBootstrap).mission_id);
}

export function initialMissionOpen(source: unknown): string {
  return missionIdFromBootstrap(source);
}
