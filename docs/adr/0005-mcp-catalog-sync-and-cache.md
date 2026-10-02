# ADR 0005: Incremental catalog sync with a local snapshot

Status: accepted (2026-09-29)

## Context

The Agent already has the mechanics this needs, on the extension side:
`data/extension-sources.json` holds source configuration
(`src/app/extension_source.rs:3949`) and `data/extension-source-cache` holds a
local cache (`:3954`) written atomically with a `.bak` sibling. The MCP catalog
should reuse that shape rather than invent a second caching story.

The official registry exposes `updated_since` as a cursor, so a full re-download
is not required to stay current. The catalog itself is small next to what the
Agent already stores: the current `himind-ai-mcp.json` is 43 bytes with an empty
`servers` array, and the extension source cache is in the same order of
magnitude as the config layer counted in ADR 0002 (~1 MB).

## Decision

Sync is incremental, cached, and never on the critical path of running a server.

- **Incremental.** Each source stores the cursor it last reached and asks for
  changes since then. A cursor is only advanced after the page is written to
  disk, so an interrupted sync re-reads rather than skips.
- **Snapshot on disk.** Entries live in the same cache area as the existing
  extension cache, written with the existing atomic write + `.bak` path.
- **The catalog is only consulted when searching and installing.** Starting,
  reconnecting or calling an installed server reads `himind-ai-mcp.json` and
  nothing else. A catalog outage cannot take a working tool offline.
- **Unreachable is a normal state.** The last snapshot stays, the UI shows when
  it was captured and that the list may be out of date, and refresh is an
  explicit action. An empty list and a stale list must not look the same.
- **Refresh policy:** manual refresh always available; a best-effort background
  refresh when the snapshot is older than 24 hours, on app start, with failures
  silent in the UI. Never on every open, never blocking.
- **Format version travels with the snapshot.** When the stored entry format
  version is older than the current one, the snapshot is discarded and rebuilt
  from a full fetch instead of migrated in place.

## Alternatives considered

- **Query the source on every open.** Rejected: it makes a browsing screen
  depend on network reachability, exposes the intranet instance to a request
  storm per window open, and turns a slow upstream into a slow UI.
- **Bake the catalog into the installer.** Rejected: it is the current
  constant-based design with extra download weight, and it puts the catalog back
  on the release train that ADR 0003 removes it from.
- **Full re-download on a schedule.** Rejected: the cursor exists, the data is
  public and paginated, and a periodic full pull buys nothing that the cursor
  does not. Refresh stays manual plus the bounded background check above.

## Consequences

- Browsing works offline, and a stale list is visible as stale rather than as an
  error.
- Cost: cursor state per source, a manual refresh control that must be
  discoverable, and one more cached artifact to reason about during support.
- Risk: a corrupt write. Mitigated by reusing the atomic write and `.bak` path
  already used for extension cache, and by rebuilding from a full fetch on a
  format-version bump.
