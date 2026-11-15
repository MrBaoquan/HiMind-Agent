import { useCallback, useEffect, useRef, useState } from 'react';
import { agentApi, type AgentActivityItem, type MissionView } from '../services/agentApi';
import { formatError } from '../types';
export type MissionCatalog = { missions: MissionView[]; activities?: AgentActivityItem[]; loading: boolean; error: string;
  refresh: () => Promise<void>; upsert: (mission: MissionView) => void };
export function useMissionCatalog(enabled = true): MissionCatalog {
  const [missions, setMissions] = useState<MissionView[]>([]);
  const [activities, setActivities] = useState<AgentActivityItem[]>([]);
  const [loading, setLoading] = useState(enabled);
  const [error, setError] = useState('');
  const pending = useRef<Promise<void> | null>(null);
  const revision = useRef(0);
  const upsert = useCallback((mission: MissionView) => {
    revision.current += 1;
    setMissions(current => {
      const previous = current.find(item => item.mission_id === mission.mission_id);
      if (previous?.effective_wait && mission.effective_wait && previous.effective_wait.cursor > mission.effective_wait.cursor) return current;
      return [mission, ...current.filter(item => item.mission_id !== mission.mission_id)];
    });
  }, []);
  const refresh = useCallback(() => {
    if (pending.current) return pending.current;
    const atRevision = revision.current;
    const request = Promise.all([agentApi.listMissions(500), agentApi.localActivity(500).catch(() => [] as AgentActivityItem[])]).then(([items, runs]) => { setActivities(runs); if (atRevision === revision.current) setMissions(current => items.map(item => { const old = current.find(value => value.mission_id === item.mission_id); return old?.effective_wait && item.effective_wait && old.effective_wait.cursor > item.effective_wait.cursor ? old : item; })); setError(''); })
      .catch(cause => setError(formatError(cause, '协作任务读取失败')))
      .finally(() => { pending.current = null; setLoading(false); });
    pending.current = request;
    return request;
  }, []);
  useEffect(() => {
    if (!enabled) return;
    void refresh();
    const timer = window.setInterval(() => { if (document.visibilityState !== 'hidden') void refresh(); }, 5000);
    return () => window.clearInterval(timer);
  }, [enabled, refresh]);
  return { missions, activities, loading, error, refresh, upsert };
}
