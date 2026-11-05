# ADR 0022: MCP discovery liveness is decided by the process, never by probing the pipe

Status: accepted (2026-11-04)

## Context

`himind-agent-mcp.exe` (the stdio bridge) has to find the `--mcp-service`
singleton that owns the capability gateway. The handoff is a discovery record:

```
%LOCALAPPDATA%\HiMindAgent\runtime\mcp-service\<profile>-<identity>-<state>\service.json
    { pid, endpoint, token, profile, identity_hash, state_hash, started_at_unix, agent_version }
```

Only `started_at_unix` is written when the service starts; `Drop` deletes it.
`Drop` does not run when the process is killed or crashes, and the MCP service is
usually a child of the bridge, so closing the client leaves a record pointing at
a dead pid behind.

The bridge used to read that record and connect to it without asking whether the
service still existed. The symptom is a client that can never connect again:

```
HiMind MCP bridge failed: 系统找不到指定的文件 (os error 2)
```

and, in Claude Desktop's log,

```
Couldn't start for Cowork and Code sessions. Error: Connection closed
```

Reproduced A/B on the same binary and state: with no record, the handshake
succeeds in ~1.2 s; with a record whose pid is dead, the bridge fails in 1.2 s
with `os error 2`. The record is the only difference.

The obvious fix — "check that the pid is alive, and also check that the endpoint
answers" — is what we tried first. Two measurements rejected the second half.

**1. A probe is a client.** `CreateNamedPipeW` is called with
`PIPE_UNLIMITED_INSTANCES`, and the service loop is one `accept` at a time. Any
`CreateFileW` on the endpoint, including one with zero access rights, is handed
to the service as a connection. The service then blocks reading a `hello` frame
that never comes, fails the session with `MCP bridge closed before hello`, and
moves on. Measured against a healthy idle service: 300 zero-access
open/close pairs took the service down at probe 46 with
`agent mcp service failed: 管道正在被关闭。 (os error 232)`. The `Drop` ran on the
way out, so the record was deleted too — the probe turned a self-healing state
into a permanent one.

**2. A probe is not a liveness test.** While one client holds a connection,
every other `CreateFileW` on the same endpoint returns `ERROR_PIPE_BUSY` (231),
because the free instance is consumed by the accept loop the moment it is
created. Measured with a real bridge holding a connection: 292 of 300 probes
were `231`, 8 were `0`, and one connection attempt consumed the only listening
instance. Treating `231` as "unreachable" makes any second client declare a live
record stale.

A single probe is also harmless, which is why this is easy to get wrong: 15 out
of 15 trials of one zero-access open/close against a fresh service left it alive.
The damage only shows up under the retry cadence the bridge actually uses
(80 iterations at 25 ms).

## Decision

Decide liveness from the process, and never touch the endpoint to find out.

`discovery_is_live` returns true only when **both** hold:

- `pid` names a running process (`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
  + `GetExitCodeProcess == STILL_ACTIVE`), and
- that process's creation time (from `GetProcessTimes`, converted from FILETIME
  to UNIX seconds, ±1 s for second-granularity records) matches
  `started_at_unix`.

The second condition is what makes pid reuse safe: a recycled pid is a live
process, but not the process that wrote the record.

`ensure_service_running` discards the record and spawns the service when the
check fails, and `wait_for_discovery` (bridge side) only returns records that
pass it. There is no `pipe_is_reachable` helper in the codebase, deliberately: a
comment where it used to be says why, so it does not get reintroduced as an
"obvious improvement".

Whether the pipe actually accepts a connection is decided by
`mcp_pipe::connect`, which retries `ERROR_PIPE_BUSY` and `NotFound` for up to
10 s and is the only place that consumes an instance. The window was widened
from 40 × 25 ms (1 s) after measuring a false failure: several clients launching
bridges at once, right after a service restart, could exhaust one second while
the service was still coming up, and the bridge reported `os error 2` for a
service that was ready moments later.
Liveness ("is the service process there") and reachability ("can I get an
instance now") are different questions and are answered in different layers.

The service also runs a keeper thread that rewrites `service.json` every 500 ms
whenever it is missing. The record is deletable by things other than us — an
older bridge, a cleanup script, a hand edit — and once it is gone the service is
invisible while still holding the singleton lock, so every new bridge spawns a
service that exits 1. Before this, deleting the record of a live service was
permanent: two retries both failed with
`HiMind Agent service exited during startup: exit code: 1`. After it, the record
is back within one interval and the next bridge handshakes normally. The keeper
takes a stop flag that `Drop` sets before removing the file, so a shutting-down
service cannot re-create its own record.

## Consequences

- A stale record costs one failed spawn attempt instead of a permanently broken
  client, and the recovery is automatic on the next bridge start.
- Killing the service leaves the record behind, as before. That is now harmless:
  the next bridge sees a dead pid, deletes it, and starts a service.
- Probe-style traffic against the endpoint is gone from the bridge, so a client
  can no longer knock the service over by retrying, and a second client can no
  longer consume the listening instance before the real one connects.
- Multi-client works: three concurrent bridges against one service all handshake
  and the record survives.
- A record written by a process whose `started_at_unix` is 0, or in the future
  beyond the 1 s tolerance, reads as stale. Only the service writes this field,
  so that means a corrupted record — failing closed is right.
- The check is process-scoped, so it cannot distinguish "the service is alive but
  wedged". A hung service keeps its record and the bridge fails on `connect`.
  That is the same outcome as before and is not made worse; a real fix would be a
  health frame inside the protocol, not a probe from outside.
- One extra background thread per service process (one per machine per scope).

## Alternatives rejected

- **Only check the pid.** Cheaper, but a recycled pid points the bridge at
  someone else's process and the record is never cleaned up. Creation time is one
  extra syscall and closes that.
- **Check the pid, then probe the endpoint for good measure.** Rejected by
  measurement above: it kills the service under the bridge's own retry cadence
  and misreads `ERROR_PIPE_BUSY` as "down".
- **Keep the 1 s connect window and treat a timeout as "service is down".** A
  service that is still starting is indistinguishable from a dead one at that
  scale, and the failure surfaced as the same `os error 2` the whole ADR is
  about. Waiting up to 10 s costs nothing when the service is already up (the
  first attempt succeeds) and only delays a genuine failure by seconds.
- **Probe with `WaitNamedPipe` instead of `CreateFileW`.** It does not consume an
  instance, so it is safer, but it waits for the pipe to become free rather than
  answering "is this service alive", and it still cannot tell a busy service from
  a dead one without a timeout. The connection itself already does this job.
- **Delete the record only when the pid is provably dead, otherwise leave it.**
  That is what this decision does; the alternative — leaving a record whose
  process is gone so the next start can "try it and see" — is the original bug.
- **Have the bridge reap records older than N minutes.** Time is not evidence of
  death: a service can legitimately run for days.
- **Write the record from a watchdog in the Agent UI process.** The UI is not
  guaranteed to be running; the service is the only process that knows the truth
  about itself.
