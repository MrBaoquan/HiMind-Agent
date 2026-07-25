export type FunctionalCategory = {
  id: string;
  label: string;
  aliases?: string[];
};

// Shared functional domains for Plugins and Skills. Keep ownership, risk and
// client support as separate metadata instead of adding them as categories.
export const FUNCTIONAL_CATEGORIES: FunctionalCategory[] = [
  { id: 'software-engineering', label: '软件开发与工程', aliases: ['开发工具', '软件开发'] },
  { id: 'visual-design', label: '平面与视觉', aliases: ['平面设计', '视觉设计'] },
  { id: 'video-post', label: '视频剪辑与后期', aliases: ['视频后期', '后期'] },
  { id: '3d-animation', label: '三维与动画', aliases: ['三维制作', '三维'] },
  { id: 'content-production', label: '内容策划与制作', aliases: ['内容制作', '编导'] },
  { id: 'audio-sound', label: '音频与声音', aliases: ['音频制作', '声音处理'] },
  { id: 'data-automation', label: '数据与自动化', aliases: ['数据处理', '自动化'] },
  { id: 'docs-knowledge', label: '文档与知识', aliases: ['文档', '知识管理'] },
  { id: 'testing-quality', label: '测试与质量', aliases: ['测试', '质量保障'] },
  { id: 'collaboration-delivery', label: '协作与交付', aliases: ['研发协作', '项目协作', '交付'] },
  { id: 'system-device', label: '系统与设备', aliases: ['系统运维', '设备管理'] },
];

const categoryByValue = new Map(
  FUNCTIONAL_CATEGORIES.flatMap(category => [
    [category.id, category] as const,
    [category.label, category] as const,
    ...(category.aliases || []).map(alias => [alias, category] as const),
  ]),
);

export function resolveFunctionalCategory(value?: string) {
  return value ? categoryByValue.get(value.trim()) : undefined;
}

export function functionalCategoryLabel(value?: string) {
  return resolveFunctionalCategory(value)?.label || value || '未分类';
}

export function functionalCategoryID(value?: string) {
  return resolveFunctionalCategory(value)?.id || value || '';
}

export function functionalCategoryMatches(values: string[] | undefined, categoryID: string) {
  return Boolean(values?.some(value => functionalCategoryID(value) === categoryID));
}

export function functionalCategoryLabels(values?: string[]) {
  return Array.from(new Set((values || []).map(functionalCategoryLabel)));
}

export function categorySearchText(values?: string[]) {
  return (values || []).flatMap(value => {
    const category = resolveFunctionalCategory(value);
    return category ? [category.id, category.label, ...(category.aliases || [])] : [value];
  }).join(' ');
}
