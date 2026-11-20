import type { OrganizationNode } from './organizationDirectory';

export type WorkbenchAudience = { mode: 'organization' | 'restricted' | ''; user_ids?: string[]; department_ids?: string[] };
export type WorkbenchSubmissionScope = { audience: WorkbenchAudience; review_department_id?: string };
export type WorkbenchSubmissionContext = { audience: WorkbenchAudience; review_departments: OrganizationNode[]; policy_revision?: number; review?: { revision: number; department_id: string } };
