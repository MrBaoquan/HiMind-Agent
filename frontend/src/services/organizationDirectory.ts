export type OrganizationNode = {
  id: string;
  kind: 'department' | 'person';
  name: string;
  parent_id?: string;
  path?: string;
  has_children?: boolean;
};
export type OrganizationPage = { nodes: OrganizationNode[]; next_cursor?: string };
export type OrganizationDirectory = {
  namespace: string;
  children(parent?: string, cursor?: string): Promise<OrganizationPage>;
  search(query: string, cursor?: string): Promise<OrganizationPage>;
  resolve(refs: string[], cursor?: string): Promise<OrganizationPage>;
};
export const organizationRef = (node: Pick<OrganizationNode, 'id' | 'kind'>) => `${node.kind}:${node.id}`;

export function createOrganizationDirectoryAdapter(request: (params: [string, string][]) => Promise<OrganizationPage & { protocol: string; namespace: string }>, namespace: string): OrganizationDirectory {
  const call = async (operation: string, params: [string, string][], cursor?: string) => {
    const page = await request([['operation', operation], ...params, ...(cursor ? [['cursor', cursor] as [string, string]] : [])]);
    if (page.protocol !== 'organization-directory.v1' || page.namespace !== namespace) {
      throw new Error('组织目录协议不兼容，请更新组织连接。');
    }
    return page;
  };
  return {
    namespace,
    children: (parent = '', cursor) => call('children', [['parent_id', parent]], cursor),
    search: (query, cursor) => call('search', [['query', query]], cursor),
    resolve: (refs, cursor) => call('resolve', refs.map(ref => ['ref', ref]), cursor),
  };
}
