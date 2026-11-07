# Architecture decision records

Decisions that are hard to reverse, or that a future reader would otherwise
re-litigate from scratch. Each record states the context it was made in, the
decision, the alternatives that were rejected, and what the decision costs.

| ADR | Title | Status | Date |
| --- | --- | --- | --- |
| [0001](0001-mcp-single-bridge-for-downstream-tools.md) | One Agent bridge for every downstream MCP tool | accepted | 2026-09-29 |
| [0002](0002-agent-config-backup-and-restore.md) | Agent state backup is a configuration layer, not a directory copy | accepted | 2026-09-29 |
| [0003](0003-mcp-catalog-entry-format.md) | MCP catalog entries use the server.json format | accepted | 2026-09-29 |
| [0004](0004-mcp-catalog-source-architecture.md) | One catalog source abstraction, official registry as the axis, self-hosted inside the intranet | accepted | 2026-09-29 |
| [0005](0005-mcp-catalog-sync-and-cache.md) | Incremental catalog sync with a local snapshot | accepted | 2026-09-29 |
| [0006](0006-mcp-catalog-trust-boundary.md) | Trust boundary, install target and ownership of catalog entries | accepted | 2026-09-29 |
| [0007](0007-market-capability-discovery-in-chat.md) | The market is a capability of the Agent, not only a screen in it | accepted | 2026-09-30 |
| [0008](0008-workbench-connections.md) | Workbench connections are first-class local records | accepted | 2026-09-30 |
| [0009](0009-local-multi-instance-topology.md) | The profile owns the data root, and a build output never opens the installed one | accepted | 2026-09-30 |
| [0010](0010-user-visible-copy-states-consequences.md) | User-visible copy states consequences, and a label is not a manual | accepted | 2026-10-01 |
| [0011](0011-mcp-registration-key-follows-the-profile.md) | The MCP registration key follows the profile | accepted | 2026-10-09 |
| [0012](0012-media-provider-plane.md) | Media providers are an Agent capability plane | accepted | 2026-10-23 |
| [0013](0013-control-plane-provider-plugin.md) | The workbench is a built-in control-plane provider plugin | accepted | 2026-10-24 |
| [0014](0014-ai-capability-provider-landing.md) | AI capabilities have explicit ownership and provider landing zones | accepted | 2026-10-24 |
| [0015](0015-ai-service-model-routing-ia.md) | AI services and capability routes are separate settings surfaces | accepted | 2026-10-25 |
| [0016](0016-ai-capability-execution-contract.md) | AI capability execution is one explainable contract | accepted | 2026-10-25 |
| [0017](0017-agent-credential-center.md) | Agent credentials have one catalog and a brokered runtime boundary | accepted | 2026-10-25 |
| [0018](0018-agent-bundled-pinned-runtime.md) | Runtime binaries are pinned and bundled with the Agent | accepted | 2026-10-26 |
| [0019](0019-extension-brand-icons.md) | Extension brand icons are package assets with local overrides | accepted | 2026-10-26 |
| [0020](0020-local-ai-gateway-aggregation.md) | 本机 AI 网关聚合 Profile 与模型路由 | accepted | 2026-10-27 |
| [0021](0021-user-data-location.md) | User data can move to another local disk, the program stays put | accepted | 2026-10-31 |
| [0022](0022-mcp-discovery-liveness-is-process-scoped.md) | MCP discovery liveness is decided by the process, never by probing the pipe | accepted | 2026-11-04 |
| [0023](0023-capability-route-selects-service-and-model.md) | 能力路由的选择单位是「服务 + 模型」 | accepted | 2026-11-05 |
| [0024](0024-plugin-capability-dependencies.md) | Plugins call registered capabilities through declared dependencies | accepted | 2026-11-03 |
| [0025](0025-release-binary-development-profile-acceptance.md) | Release binary acceptance runs against the development profile | accepted | 2026-11-03 |
| [0026](0026-local-ui-read-audit-sampling.md) | Local UI reads stay out of the Agent Core run ledger | accepted | 2026-11-03 |
| [0027](0027-application-capability-source.md) | Application capabilities use an explicit source without entering Agent Core | accepted | 2026-11-05 |

## How these relate

0003–0006 are one chain and are read in order: the entry format, where entries
come from, how they are kept locally, and what is trusted and where it installs.
0006 depends on 0001 (single bridge, credentials, risk level) and 0002
(secrets are DPAPI-sealed and travel only through a passphrase). Nothing in the
MCP catalog series changes either of them.

0008 is independent of the MCP catalog series. It changes where the Agent's
*identity* lives (per workbench connection instead of per install) and leaves
capability storage alone; 0006's install-target and ownership rules still hold
once the active connection changes.

0009 constrains 0008 rather than extending it. 0008 says which connection owns
the identity; 0009 says which *process* is allowed to touch the root those
files live in, and it is what keeps a second Agent on the same machine from
materialising into the first one's root.

0011 completes 0009 on the client surface. 0009 isolated the data root, the
port and the WebView2 directory but left the MCP registration key shared; 0011
makes that key follow the profile too, so the two Agents can be registered into
one client at once and a development run can no longer overwrite — or be
mistaken for — the installed Agent's registration.

0013 builds on 0008 and the capability boundary in 0001: 0008 owns the
per-workbench identity records, while 0013 makes workbench services and
control-plane capabilities a provider lifecycle instead of a global Agent mode.

0010 is about the words on the screen and touches only the frontend. It has no
dependency on the MCP catalog series: it leaves 0007's market discovery and the
metaguide's extension-description contract alone, and constrains the Agent's own
labels, hints and empty states instead.

0022 is the companion of 0001 on the process side. 0001 says there is exactly one
Agent bridge per client; 0022 says how a bridge decides that the service behind
that bridge still exists, and why the answer must come from the process rather
than from the endpoint.

0023 carries 0016's contract down to the model field. 0016 already declared
`model` a first-class part of the request and the plan, but the implementation
stopped at service granularity; 0023 removes that gap and accepts a hard
migration of the persisted selection to do it. It deliberately does not touch
0020's `GatewayModelRoute`: that record owns the model aliases exposed to
external clients, while 0023 owns the model HiMind's own capabilities use.

0024–0027 were already developed in the working tree while 0022/0023 landed upstream. They keep their original decisions but use non-conflicting ADR numbers: 0024 plugin capability dependencies, 0025 release acceptance, 0026 local UI read sampling, 0027 application capability ownership.

## Adding a record

Next number is the highest in this directory plus one. Use
`NNNN-kebab-case-title.md`, a `Status: accepted (<date>)` line under the title,
and keep the rejections in — an ADR without alternatives considered is a note,
not a decision. Records are written in English to match the codebase's
documentation.
