import type {MissionSnapshot} from '../services/agentApi';

/** Event-page continuation and the current state head can differ on long runs. */
export async function readMissionSnapshot(read: (after: number) => Promise<MissionSnapshot>, after: number) {
  let snapshot = await read(after);
  let replace = after === 0;
  const head = (page: MissionSnapshot) => page.mission.effective_wait?.cursor;
  if (snapshot.requires_snapshot && !(typeof head(snapshot) === 'number' && snapshot.cursor < head(snapshot)!)) {
    snapshot = await read(0); replace = true;
  }
  const events = new Map(snapshot.events.map(event => [event.event_id,event]));
  for (let page = 0; typeof head(snapshot) === 'number' && snapshot.cursor < head(snapshot)!; page++) {
    if (page >= 32) throw new Error('事件历史过长，尚未取得完整状态快照；请重新读取');
    const next = await read(snapshot.cursor);
    if (next.cursor <= snapshot.cursor) throw new Error('事件分页未推进，状态资格尚未确认；请重新读取');
    for (const event of next.events) events.set(event.event_id,event);
    snapshot = next;
  }
  return {snapshot:{...snapshot,events:[...events.values()].sort((a,b)=>a.sequence-b.sequence)},replace};
}
