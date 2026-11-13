import type { WorkspaceRegistryProject } from '../services/agentApi';

export const hasChinese = (value: string) => /[\u{3400}-\u{4dbf}\u{4e00}-\u{9fff}\u{f900}-\u{faff}\u{20000}-\u{323af}]/u.test(value);

export function chineseProjectAliases(project: Pick<WorkspaceRegistryProject, 'aliases'>): string[] {
  return [...new Set((project.aliases ?? []).map(value => value.trim()).filter(value => value && hasChinese(value)))];
}

/** Display identity is independent of a directory basename and stable IDs. */
export function projectDisplayName(project: Pick<WorkspaceRegistryProject, 'name' | 'aliases'>): string {
  const name = project.name.trim();
  if (hasChinese(name)) return name;
  const aliases = chineseProjectAliases(project);
  return aliases.length === 1 ? aliases[0] : name;
}

export function projectTechnicalName(project: Pick<WorkspaceRegistryProject, 'name' | 'aliases'>): string {
  const display = projectDisplayName(project);
  if (project.name.trim() !== display) return project.name.trim();
  const aliases = (project.aliases ?? []).filter(value => value.trim() && !hasChinese(value));
  return aliases.find(value => !/[\\/]/.test(value)) ?? aliases[0] ?? '';
}

export function projectSearchText(project: Pick<WorkspaceRegistryProject, 'name' | 'aliases'>): string {
  return [project.name, ...(project.aliases ?? [])].join(' ').toLocaleLowerCase();
}

export function projectAncestors(projects: WorkspaceRegistryProject[], projectId: string): WorkspaceRegistryProject[] {
  const byId = new Map(projects.map(project => [project.id, project]));
  const result: WorkspaceRegistryProject[] = [];
  const seen = new Set([projectId]);
  let parentId = byId.get(projectId)?.parent_id;
  while (parentId && !seen.has(parentId)) {
    seen.add(parentId);
    const parent = byId.get(parentId);
    if (!parent) break;
    result.unshift(parent);
    parentId = parent.parent_id;
  }
  return result;
}

export function projectQualifiedName(projects: WorkspaceRegistryProject[], project: WorkspaceRegistryProject): string {
  return [...projectAncestors(projects, project.id), project].map(projectDisplayName).join(' / ');
}

export function descendantProjectIds(projects: WorkspaceRegistryProject[], projectId: string): Set<string> {
  const result = new Set([projectId]);
  const children = new Map<string, string[]>();
  for (const project of projects) {
    if (!project.parent_id) continue;
    children.set(project.parent_id, [...(children.get(project.parent_id) ?? []), project.id]);
  }
  const queue = [projectId];
  for (let index = 0; index < queue.length; index += 1) {
    for (const id of children.get(queue[index]) ?? []) {
      if (result.has(id)) continue;
      result.add(id);
      queue.push(id);
    }
  }
  return result;
}
