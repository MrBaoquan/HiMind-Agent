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

## Adding a record

Next number is the highest in this directory plus one. Use
`NNNN-kebab-case-title.md`, a `Status: accepted (<date>)` line under the title,
and keep the rejections in — an ADR without alternatives considered is a note,
not a decision. Records are written in English to match the codebase's
documentation.
