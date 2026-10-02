# ADR 0003: MCP catalog entries use the server.json format

Status: accepted (2026-09-29)

## Context

**HiMind AI 工具** today offers four servers from a constant compiled into the
frontend:

`frontend/src/pages/mcpServerView.ts:38` defines `mcpPresets` with six fields
(`id`, `name`, `summary`, `command`, `requires`, `requiresLabel`, plus an
optional `directory`): `memory`, `sequential-thinking`, `filesystem`,
`everything` — all of them `npx -y @modelcontextprotocol/server-*@2026.8.31`.
`frontend/scripts/check-mcp-servers.mts:40-49` locks that shape down: every
preset must parse to `executable === 'npx'`, every `requires` must be `'npx'`,
and `filesystem` must be the only preset carrying a directory.

That shape cannot express the rest of the ecosystem. It has no place for a
remote URL, a PyPI or Docker package, a pinned argument template, a required
environment variable, an icon or a version other than the one in the source
file. Adding a fifth server means editing TypeScript and shipping a desktop
release, and a build-time test asserts the constraint so it cannot drift on its
own.

The public ecosystem already standardizes this. The official MCP registry
publishes each server as a `server.json` document whose `$schema` is versioned
(`https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json`
from a live call on 2026-09-29; `2025-09-29` has been observed on the same
endpoint). A `server.json` covers npm, PyPI, Cargo, NuGet, OCI/Docker and
remote (`streamable-http`, `sse`) transports, with fixed arguments and argument
templates. MCPB (manifest v0.4, `@anthropic-ai/mcpb`) is the companion format
for shipping a server as a local bundle: it adds `user_config`, `compatibility`,
`icons`, `license` and `privacy_policies`, and requires
`name`/`version`/`description`/`author`/`server`.

## Decision

A catalog entry **is** a `server.json` document. We do not invent a field
layout, and we do not restate fields the standard already defines.

- `server.json` is the catalog format. Consuming a public source means
  consuming its documents, not translating them into a private schema.
- **MCPB is the packaging format, not the catalog format.** A catalog entry may
  point at an MCPB artifact, and the Agent may install it, but the entry itself
  stays `server.json`.
- Our own metadata (catalog source, trust level, first-seen time, download
  hints) lives under `_meta` with a namespaced key, the same way the registry
  uses `_meta["io.modelcontextprotocol.registry/official"]`. It never becomes a
  top-level field.
- Mapping into the internal `McpServerConfig` is mechanical and one-way:
  `packages[]` → `command`/`args`/`env`, `remotes[]` → `transport`/`url`/
  `headers`. The install target is unchanged (see ADR 0006).

### Parsing rules that follow from a versioned schema

The schema is versioned and has already moved once. The parser therefore:

- branches on the `$schema` version string instead of the document as a whole;
- ignores fields it does not know;
- degrades per entry rather than per source: an entry that uses an unknown
  package registry type, or is missing a required field, is **listed but not
  installable**, with the reason shown. One bad entry must not blank the list.

## Alternatives considered

- **Keep a private catalog schema.** Rejected: it forfeits the point of
  consuming public sources, and every source would need a hand-written
  translator that breaks whenever the source changes.
- **Use MCPB as the catalog format.** Rejected: it would exclude the majority
  of published servers, which are npm packages or plain remote URLs, and it
  conflates "how this is packaged" with "what this is".
- **Keep `mcpPresets` as the catalog.** Rejected: it makes the catalog a
  release artifact. The four presets may remain as a curated first-run list
  rendered from the same format, but they stop being the only possible
  contents.
- **Support one transport shape (stdio/`npx`) to stay simple.** Rejected as a
  design rule: it is the current limitation, not a simplification. It stays
  supportable as a *rendering* choice for entries that need a local runtime the
  machine does not have.

## Consequences

- A public source can be consumed directly, and the official `mcpb` CLI can be
  used for local bundles instead of hand-rolled packaging.
- The four current presets become data, not code. `check-mcp-servers.mts`
  moves from "every preset is npx" to "every entry in the curated list parses
  and maps to a valid config".
- Cost: the parser carries version tolerance forever, and an entry can be
  visible without being installable. Both are accepted; the alternative is a
  catalog frozen at release time.
- Risk: schema drift is real, not hypothetical — two `$schema` versions were
  served by the same endpoint within the same period. Tolerance, degradation
  and an explicit "this entry needs a newer Agent" message are the mitigations.
