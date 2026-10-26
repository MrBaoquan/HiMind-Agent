import type { WorkflowCenterSnapshot, WorkflowOptionItem, WorkflowOptionSourceResult, WorkflowViewField } from '../services/agentApi';

const CACHE_PREFIX = 'himind.workflow-option-source.v1';
const CACHE_TTL_MS = 30 * 24 * 60 * 60 * 1000;

type CachedOptions = {
  saved_at: number;
  options: WorkflowOptionItem[];
};

function cacheKey(workflowId: string, fieldId: string, capabilityId: string) {
  return `${CACHE_PREFIX}:${workflowId}:${fieldId}:${capabilityId}`;
}

function readCache(key: string): WorkflowOptionItem[] {
  if (typeof localStorage === 'undefined') return [];
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return [];
    const cached = JSON.parse(raw) as CachedOptions;
    if (!cached || !Array.isArray(cached.options) || !Number.isFinite(cached.saved_at)) return [];
    if (Date.now() - cached.saved_at > CACHE_TTL_MS) {
      localStorage.removeItem(key);
      return [];
    }
    return cached.options.filter(option => option && typeof option.value === 'string');
  } catch {
    return [];
  }
}

function writeCache(key: string, options: WorkflowOptionItem[]) {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(key, JSON.stringify({ saved_at: Date.now(), options }));
  } catch {
    // 缓存只是降级能力，存储配额不足不能阻断目录加载。
  }
}

function isFieldDefinition(field: WorkflowViewField): field is Exclude<WorkflowViewField, string> {
  return typeof field === 'object' && field !== null;
}

/**
 * 用户分类目录不能写死在 workflow 包里。Agent 只在字段显式声明
 * option_source 时才调用对应 capability，并让前端缓存最近一次成功结果。
 */
type WorkflowOptionResolver = (workflowId: string, fieldId: string) => Promise<WorkflowOptionSourceResult>;

export async function hydrateWorkflowOptionSources(snapshot: WorkflowCenterSnapshot, resolve: WorkflowOptionResolver): Promise<WorkflowCenterSnapshot> {
  const workflows = await Promise.all(snapshot.workflows.map(async workflow => {
    if (!workflow.view) return workflow;
    const sections = await Promise.all(workflow.view.sections.map(async section => ({
      ...section,
      fields: await Promise.all((section.fields || []).map(async field => {
        if (!isFieldDefinition(field) || !field.option_source?.capability_id) return field;
        const capabilityId = field.option_source.capability_id;
        const key = cacheKey(workflow.package.id, field.id, capabilityId);
        try {
          const result = await resolve(workflow.package.id, field.id);
          if (!result.options.length) throw new Error('分类目录为空');
          writeCache(key, result.options);
          return {
            ...field,
            options: result.options,
            option_source_warning: '',
            option_source_cached: false,
          };
        } catch (error) {
          const cached = readCache(key);
          const message = String((error as { message?: unknown })?.message || error || '未知错误');
          return {
            ...field,
            options: cached.length ? cached : field.options,
            option_source_cached: cached.length > 0,
            option_source_warning: cached.length
              ? `分类目录暂时不可用，已使用最近缓存：${message}`
              : `分类目录暂时不可用，当前显示内置分类：${message}`,
          };
        }
      })),
    })));
    return {
      ...workflow,
      view: { ...workflow.view, sections },
    };
  }));
  return { ...snapshot, workflows };
}
