import type { MissionBudgetIncrease } from '../services/agentApi';

// Retain the exact submission across a lost response, refresh and dialog reopen.
// A changed input gets a new key; an identical retry reuses the original key.
const memory = new Map<string, MissionBudgetIncrease>();
const key = (id: string, plan: string) => `mission-budget:${id}:${plan}`;
export function pendingBudgetRequest(id: string, plan: string) {
  const name = key(id, plan);
  try {
    const value = JSON.parse(sessionStorage.getItem(name) || 'null') as MissionBudgetIncrease | null;
    if (value && typeof value.request_id === 'string' && value.expected_plan_sha256 === plan) return value;
  } catch { /* The in-memory copy also works when storage is unavailable. */ }
  return memory.get(name) || null;
}
export function retainBudgetRequest(id: string, input: Omit<MissionBudgetIncrease, 'request_id'>) {
  const previous = pendingBudgetRequest(id, input.expected_plan_sha256);
  if (previous) {
    const {request_id: _requestId, ...body} = previous;
    if (JSON.stringify(body) === JSON.stringify(input)) return previous;
  }
  const request = {request_id: `agent-ui:budget:${crypto.randomUUID()}`, ...input};
  const name = key(id, input.expected_plan_sha256);
  memory.set(name, request);
  try { sessionStorage.setItem(name, JSON.stringify(request)); } catch { /* memory retained */ }
  return request;
}
export function clearBudgetRequest(id: string, plan: string) {
  memory.delete(key(id, plan));
  try { sessionStorage.removeItem(key(id, plan)); } catch { /* memory cleared */ }
}
