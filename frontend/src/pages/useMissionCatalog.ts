import { useCallback, useEffect, useRef, useState } from 'react';
import { agentApi, type MissionView } from '../services/agentApi';
import { formatError } from '../types';
export type MissionCatalog = { missions: MissionView[]; loading: boolean; error: string;
  refresh: () => Promise<void>; upsert: (mission: MissionView) => void };
export function useMissionCatalog(enabled = true): MissionCatalog {
  const [missions, setMissions] = useState<MissionView[]>([]);
  const [loading, setLoading] = useState(enabled);
  const [error, setError] = useState('');
  const pending = useRef<Promise<void> | null>(null);
  const revision = useRef(0);
  const upsert = useCallback((mission: MissionView) => {
    revision.current += 1;
    setMissions(current => [mission, ...current.filter(item => item.mission_id !== mission.mission_id)]);
  }, []);
  const refresh = useCallback(() => {
    if (pending.current) return pending.current;
    const atRevision = revision.current;
    const request = agentApi.listMissions(500).then(items => { if (atRevision === revision.current) setMissions(items); setError(''); })
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
  return { missions, loading, error, refresh, upsert };
}
