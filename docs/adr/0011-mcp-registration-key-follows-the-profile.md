# The MCP registration key follows the profile

Status: accepted (2026-10-09)

## Context

ADR 0009 gave every process its own data root, port and WebView2 directory, so
the installed Agent and a development Agent can run side by side without
touching each other's state. It deliberately left one surface shared: the AI
client configuration files.

Every client the Agent registers itself into (Codex, GitHub Copilot, WorkBuddy,
Cursor, VS Code, OpenCode, ZCode, and the rest) stores the Agent MCP entry under
a single, fixed key — `himind-agent`. The written entry carries
`HIMIND_AGENT_PROFILE` and a `--state <profile>` path, and the "is this client
already configured?" check compares that profile against the running one.

That made the two Agents mutually exclusive on the client side. Whichever Agent
registered last owned the `himind-agent` key; a development run that registered
silently overwrote the installed Agent's line, and the installed Agent's next
registration overwrote it back. The two could not be connected at the same
time, and a development test that connected a client left production pointing
at the development data root until someone registered again.

ADR 0009's own consequences already warned about the stale-registration path.
This record closes the remaining half: the *slot* itself was still shared even
though the data behind it was not.

## Decision

Make the registration key a function of the profile.

1. **Production keeps the historical key.** `production` — and the legacy
   `default`, and an empty name — still register as `himind-agent`, so an
   upgrade never orphans an entry the installed Agent already wrote
   (`mcp_registry::agent_server_id_for`).
2. **Every other profile is scoped.** A non-production profile registers as
   `himind-agent-<profile>` (for example `himind-agent-development`). The key
   is derived in one place and consumed by every writer, reader and remover, so
   the client adapters, the JSON target adapters, the manual snippet, the
   registration plan and the status check cannot drift apart.
3. **A stale singleton entry is reclaimed, never stolen.** A non-production
   apply or remove also looks at the old `himind-agent` key and drops it *only*
   when its `HIMIND_AGENT_PROFILE` equals the running profile — i.e. when a
   previous build of this same profile wrote it. An entry that still carries
   `production` is left untouched
   (`legacy_singleton_profile`, `reclaim_legacy_*`). Production never reclaims
   anything.
4. **The status check reads the scoped key.** A client is "configured" when the
   entry under this profile's key matches command, arguments, client id and
   profile. An untouched production entry under `himind-agent` therefore reads
   as "not configured" to a development Agent instead of "needs repair", and
   the two coexist.

## Consequences

- The installed Agent and a development Agent can register into the same client
  at the same time: `himind-agent` and `himind-agent-development` side by side.
  Connecting a development Agent to a client no longer moves production.
- The development Agent's self-registration is now also the cleanup: connecting
  it removes the orphan a previous development build left under the singleton
  key, so a client can no longer load a stale development line as if it were
  the installed Agent.
- A client config grows by one entry per profile a developer actually connects.
  That is the intended trade: one visible, removable line per Agent instead of
  one line that two Agents fight over.
- The fix lives in the build that carries it. An installed Agent on an older
  build keeps writing the singleton key and its behaviour is unchanged; the two
  schemes meet only at the reclaim rule, which is profile-guarded on both sides.
- Uninstalling a development Agent still needs its own `-development` entry
  removed; disconnecting the client from that Agent (or `remove`) does it, and
  the reclaim rule also drops the legacy singleton entry if that profile wrote
  one.

## Alternatives rejected

- **Give the development Agent a different client binary or config directory.**
  Rejected: it multiplies setup per client, does not apply to clients with one
  fixed config path, and does not fix the overwrite for the common case.
- **Write both entries from every Agent.** Rejected: an Agent may only speak for
  its own profile; writing the other profile's line is the same crossover ADR
  0009 exists to prevent.
- **Namespace only the value (the launch line) and keep one key.** Rejected: a
  client config is a map keyed by server id; two values under one key is not a
  representable state.
- **Detect the stale entry lazily and repair on open only.** Rejected: the
  window between "stale entry written" and "someone opens the client" is exactly
  when a client silently connects the wrong data root; reclaiming at write time
  closes it without user action.
- **Reclaim the singleton entry whenever it names this profile *or* is
  unparseable.** Rejected: an unparseable entry may belong to another profile or
  a hand-edited config; only an explicit, matching profile is safe to remove.
