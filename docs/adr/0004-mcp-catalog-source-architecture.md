# ADR 0004: One catalog source abstraction, official registry as the axis, self-hosted inside the intranet

Status: accepted (2026-09-29)

## Context

Reachability was measured from a developer machine on 2026-09-29, not read from
documentation:

| Source | Endpoint | Result |
| --- | --- | --- |
| Official MCP registry | `registry.modelcontextprotocol.io/v0/servers` | 200, real data; `search`, `limit`, `updated_since` all work; staging instance and `/openapi.yaml` published |
| Smithery | `registry.smithery.ai/servers?q=...` | 200, real data |
| Zed | `api.zed.dev/extensions?filter=context_server` | 200, real data |
| Docker MCP catalog | `docker/mcp-registry` (MIT) | one YAML per server in git; no HTTP API, no release artifacts |
| PulseMCP | `api.pulsemcp.com/v0beta/servers` | 410 Gone; `/v0/` 404 |
| Glama | `glama.ai/api/mcp/v1/servers` | 401 — needs a commercial API key |
| ModelScope MCP 广场 | `/mcp` | page 200, five API paths tried, all 404 |
| Cline marketplace, awesome-mcp-servers | GitHub raw | documents, not APIs |

Two facts shape the decision. First, the official registry already aggregates
other sources — its live data carries `ai.smithery/*` and `ac.inference.sh/*`
names — so connecting to it covers far more than connecting to Smithery alone.
Second, the clients we ship to are, by construction, intranet machines: the
users of this Agent run it against an internal workbench, and several of the
public endpoints above (or the CDN in front of them) are not reliably reachable
from there.

## Decision

The client knows exactly one concept: a **catalog source**. A source yields
entries (ADR 0003) and a sync cursor. It does not know which upstream the
entries originally came from.

```
官方 registry (upstream) ─┐
Docker MCP catalog ───────┼─> 自建 registry 实例 (Dashboard 侧) ─> 目录源 API ─> Agent
团队内部源 ───────────────┘         staging 实例 = 同一套配置
                                   官方 registry (无内网部署时的直连回退)
```

- **Default: a self-hosted instance of the official registry**, run on the
  Dashboard side, with a staging instance built from the same configuration.
  The server does the upstream syncing; the Agent does not.
- **Direct access from the Agent to the official registry is a fallback**, for
  installations with no intranet deployment. It is off by default in managed
  deployments, because "sometimes reachable" produces intermittent catalog
  failures that look like our bug.
- **Supplementary sources sit behind the same abstraction**: the Docker MCP
  catalog (its YAML is converted to `server.json` by the same server-side sync
  job that feeds the instance) and the team's internal source. They are not
  separate code paths in the client.
- **No scraper is written for an aggregator without a public API.** PulseMCP
  returns 410, Glama wants a paid key, ModelScope has no published contract.
  Their content is reachable through the official registry where it is
  syndicated, and that is enough.

## Alternatives considered

- **Let the Agent call every public source directly.** Rejected: an intranet
  machine cannot reach most of them, each source has its own SLA and error
  semantics, and every new source becomes a client release. The client would
  inherit N availability profiles for data it renders identically.
- **Use the Docker MCP catalog as the axis.** Rejected as the axis, kept as a
  source: it is a git repository of curated YAML with no API and no releases,
  so every client would clone a repository to read a list.
- **Scrape the 国内 MCP 广场 web APIs.** Rejected: no published contract, so
  we would own a parser that breaks silently, for content that is largely
  syndicated into the official registry anyway.
- **Ship the catalog inside the installer.** Rejected: it is the current
  problem in a new place — see ADR 0005.

## Consequences

- One protocol in the client, one place to add a source, intranet-controllable
  content and the ability to publish internal servers without upstreaming them.
- The self-hosted instance becomes a component we operate: a sync job, a
  staging environment and an upgrade path for the registry software. Content
  lags upstream by one sync interval, which is acceptable for a catalog.
- Risk: the instance is a single point of failure for browsing. Mitigated by
  the local snapshot (ADR 0005) and the direct fallback above. Catalog
  availability never blocks *using* an already-installed server — that is
  ADR 0001's job and it does not depend on the catalog at all.
