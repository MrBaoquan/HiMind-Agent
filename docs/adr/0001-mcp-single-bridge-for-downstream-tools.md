# ADR 0001: One Agent bridge for every downstream MCP tool

Status: accepted (2026-09-29)

## Context

HiMind AI runs on the DSH runtime. DSH reads MCP servers from
`agent.patch.yml`, and every server it finds becomes part of the model's tool
surface. Users can add their own MCP servers in **HiMind AI 工具**, so there are
two ways to wire them up:

1. Expand each personal server into its own DSH entry (`id: himind-agent-personal-mcp-<name>`).
2. Keep one DSH entry (`himind-agent-mcp`) and let the Agent process fan out to
   the personal servers itself.

The first option was implemented once and removed. The second is the design we
keep. This ADR records why, and the boundaries that come with it.

## Decision

The Agent exposes exactly one MCP server to DSH and to every other MCP client:
`himind-agent`. Personal and third-party servers are **downstream connections**
owned by the Agent. The bridge never expands them into the client's config.

```
DSH / other MCP clients
        |  (one stdio entry: himind-agent)
        v
HiMind Agent  -- policy, credentials, audit, capability registry
        |
        +--> downstream stdio / streamable-http servers (user-managed)
        +--> Dashboard business tools (connected mode only)
```

Consequences of the single entry point:

- **Credentials stay in the Agent.** Downstream `env` and `headers` values are
  stored with DPAPI and revealed only inside the Agent process. A DSH-layer
  expansion would have written those values into the runtime profile, which is
  plain YAML on disk. DSH also strips `*_KEY`/`*_SECRET`/`*_TOKEN` and every
  `DSH_*` variable from child processes, so a client-side config could not carry
  them faithfully even if we wanted it to.
- **One setup serves every client.** The connection is configured once, in the
  Agent UI. DSH, Codex, Claude Code, opencode and any future ACP/MCP client see
  the same tools without re-entering commands or secrets.
- **Governance has a single door.** Approval requirements, `mcp_downstream`
  risk level, capability registry generations, activation, and audit all live
  in one place. Expanding servers into the runtime profile would move tool
  visibility outside the code that decides whether a tool is allowed.
- **A downstream outage is contained.** A downstream that fails to start only
  removes its own tools; it cannot corrupt the runtime profile or block the
  other capabilities from loading.

## Downstream tool semantics

- Third-party tools are projected with `risk_level: mcp_downstream` and
  `approval_required: true`. There is no trusted risk contract for a tool we did
  not write, so it does not become auto-runnable.
- `fail_on_startup_error` (UI: **必须可用**) is per-connection. When it is off,
  a downstream that cannot be started is skipped and its tools are simply
  absent. When it is on, capability discovery reports
  `downstream_mcp_required_unavailable` and the session fails instead of
  silently running with a smaller toolset.
- `reconnect` (UI: **断开后自动重连**) is per-connection, and applies to *tool
  calls*: an invoke that fails at the transport layer reconnects and retries
  with 500 ms → 1 s → 2 s → 4 s backoff, capped at 30 s and 5 attempts. A tool
  that answers with a JSON-RPC error has already received the request, so that
  error is returned to the model immediately instead of being retried.
- These two settings are **connection** settings. The `failOnStartupError` and
  `reconnect` keys written into the DSH bridge entry are the *bridge's own*
  settings and describe the Agent process, not any individual downstream. They
  are different layers with the same names; do not conflate them.
- The bridge's `toolCallTimeoutMs` is derived from the slowest enabled
  downstream (`max(60 s, longest downstream timeout)`, capped at 10 minutes).
  DSH defaults to 60 s, which would otherwise truncate a slow downstream call
  and report it as a failure before the downstream could answer.

## Direct connection exemption

A direct client-to-server connection (a server configured straight into Codex,
Claude Code, DSH, ...) is still allowed when **all** of the following hold:

- the server is used by exactly one client;
- it needs no Agent-held credentials and reads no Agent-managed data;
- the user does not need approval gating, audit, or capability governance for
  it.

In that case the direct path is genuinely simpler, and the Agent does not try to
own it. Such a connection must be labelled as bypassing Agent governance, and
the option stays off by default. It exists to keep a documented escape hatch,
not as an equally recommended path.

## Alternatives considered

- **Expand each personal server into DSH config.** Rejected: leaks credentials
  into the runtime profile, duplicates setup per client, bypasses approval and
  registry generation handling, and lets a bad server break the runtime profile
  for the whole session.
- **Connect every client directly to every server.** Rejected for the general
  case: N clients × M servers of duplicated credentials and no governance. Kept
  only as the narrow exemption above.
