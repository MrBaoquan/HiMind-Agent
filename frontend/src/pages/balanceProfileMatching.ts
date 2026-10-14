export type BalanceProfileSuggestion =
  | 'deepseek'
  | 'kimi'
  | 'kimi-coding'
  | 'zhipu-plan'
  | 'minimax-plan-cn'
  | 'minimax-plan-en'
  | 'stepfun'
  | 'siliconflow'
  | 'siliconflow-en'
  | 'openrouter'
  | 'novita'
  | 'opencode-go';

/**
 * Conservative host/path based recommendation, not a query trigger.
 * Leave ambiguous/custom relays for the user to configure manually.
 */
export function inferBalanceProfile(baseUrl: string): BalanceProfileSuggestion | null {
  let url: URL;
  try {
    url = new URL(baseUrl.trim());
  } catch {
    return null;
  }
  if (url.protocol !== 'https:' || (url.port && url.port !== '443')) return null;
  const host = url.hostname.toLowerCase().replace(/\.$/, '');
  const path = url.pathname.replace(/\/+$/, '') || '/';
  const exactHosts: Record<string, BalanceProfileSuggestion> = {
    'api.deepseek.com': 'deepseek',
    'api.moonshot.cn': 'kimi',
    'api.stepfun.ai': 'stepfun',
    'api.stepfun.com': 'stepfun',
    'api.siliconflow.cn': 'siliconflow',
    'api.siliconflow.com': 'siliconflow-en',
    'openrouter.ai': 'openrouter',
    'api.openrouter.ai': 'openrouter',
    'api.novita.ai': 'novita',
  };
  if (host === 'api.kimi.com' && (path === '/coding' || path === '/coding/v1')) return 'kimi-coding';
  if ((host === 'open.bigmodel.cn' || host === 'api.z.ai') && path.includes('/paas/v4')) return 'zhipu-plan';
  if (host === 'api.minimaxi.com' || host === 'api.minimax.cn') return 'minimax-plan-cn';
  if (host === 'api.minimax.io') return 'minimax-plan-en';
  if (host === 'opencode.ai' && (path === '/zen/go' || path === '/zen/go/v1')) {
    return 'opencode-go';
  }
  return exactHosts[host] ?? null;
}
