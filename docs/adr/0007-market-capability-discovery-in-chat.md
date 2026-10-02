# ADR 0007: The market is a capability of the Agent, not only a screen in it

Status: accepted (2026-09-30)

## Context

The Agent ships an extension market (`src/app/market.rs`) that lists skills,
plugins and workflows from three kinds of source: the HiMind workbench, a local
extension source, and a GitHub-backed extension source. Until now that market
was reachable only from the desktop UI. A conversation that needed a skill the
user had not installed had no way to say "the market has this, shall I install
it?" — the model could only report that the capability was missing, which reads
as "this tool can't do it" even when the capability is one click away.

Two constraints shape the answer:

1. **Installing writes to the user's machine.** Installing a skill copies files
   into a global location or a project directory and may fan out to several AI
   clients. It is not a simulation, and it must not become something a model can
   do on its own initiative without the user agreeing to it.
2. **The Agent is not the only client.** DSH, Codex, Claude Code and other MCP
   clients reach the Agent through the MCP surface (`src/mcp.rs`) and, from
   there, the capability gateway (`src/capability/service.rs`). A feature that
   only exists in the desktop UI does not exist for those clients, and the
   user's own request ("can this serve other AI clients?") fails.

The pieces that already exist and must be reused rather than paralleled:

- `src/app/market.rs` — one catalog, one plan shape, one installer. Its
  `install()` recomputes the plan before executing and refuses when the plan is
  not `ready`, so "what the model saw" and "what happens" cannot diverge.
- `src/app/operation_plan.rs` — the single plan vocabulary
  (`capability`, `item`, `ready`, `blocked_reasons`, `dependencies`, `targets`).
- `src/app/skill_manager.rs`, `plugin_manager.rs`, `extension_source.rs`,
  `workflow_manager.rs` — the only writers of installed content.
- `CapabilityAvailability::Local` and the approval policy in
  `src/approval/policy.rs` — the existing risk vocabulary, which maps
  `local_write` to R2 and reserves R3 for the operations that need a human.

## Decision

**The market is exposed as four capabilities, and they are part of the MCP
bootstrap.**

| Capability | Risk | What it does |
| --- | --- | --- |
| `market.search` | `read_only` | Search skills, plugins and workflows by kind, keyword and category. |
| `market.installed` | `read_only` | Inventory what this machine already has, including skill install locations. |
| `market.install.plan` | `read_only` | Produce the install plan: version, source, artifact digest, dependencies, target, blockers. |
| `market.install` | `local_write` | Execute the plan. Recomputes it first; refuses when not ready. |

They are added to `BOOTSTRAP_TOOL_IDS` because the market must be visible
*before* anything is installed. A client that has to activate a capability in
order to discover that capabilities can be installed cannot solve the problem
the capability exists for. The initialize instructions carry the same guidance
in prose: when a capability is missing, look in the market and plan an install
instead of stopping.

**Installing is explicitly a user-confirmed operation.** `market.install` is
added to the `approval_required` conditions in `apply_registry_metadata`
rather than relying on its R2 rating, and both the capability description and
the MCP instructions say the model must not approve on the user's behalf. The
read-only trio stays unapproved, so browsing the market never blocks on a
prompt.

**Authorization is per source, not per capability.** The market stays usable
without a workbench account, because local and GitHub extension sources do not
need one. The gateway therefore uses a lenient `paired_agent_id()` for market
capabilities (empty string when not authorized); a workbench-only source then
reports "HiMind 账号尚未授权，无法读取工作台技能" as a `blocked_reason` in the
plan, which is a fact about one install rather than a reason to disable the
whole market. The strict `load_paired_agent()` remains for capabilities that
genuinely require a workbench identity.

**The CLI and MCP share one implementation.** `himind-agent market
<search|installed|plan|install>` calls the same `src/app/market.rs` functions
with `InvocationSource::Cli`, so a terminal user, the MCP surface and the
desktop UI cannot drift into three different notions of "install".

## Alternatives considered

- **UI-only.** Ship the feature as a desktop dialog and leave MCP out. Rejected:
  it answers "the Agent can do this" but not "my AI client can do this", which
  is the scenario that motivated the work.
- **Expose only `market.install`, no plan step.** Rejected: it collapses the
  decision and the action, so the user is asked to confirm an install before
  seeing which version, source or target it means.
- **Keep the market out of the bootstrap and require
  `capability.catalog.activate` first.** Rejected: discovery is the whole point
  of the feature, and bootstrap activation is exactly the step a client is
  least likely to perform at the moment it has just decided it is stuck.
- **Rely on the R2 rating for approvals.** Rejected: R2 is the ordinary local
  write tier. Installing a plugin or a workflow, or fanning a skill out to
  several clients, is a larger change to the machine than that tier is meant to
  cover, so the capability is named explicitly instead of being inferred.
- **Hard-disable the market when no workbench account is paired.** Rejected:
  most locally developed extensions and the GitHub source have nothing to do
  with the workbench; disabling the market would punish a valid offline setup.

## Consequences

- One more writer can change what `tools/list` shows; the install path already
  calls `invalidate_capability_discovery()`, so a freshly installed plugin's
  capabilities appear without restarting the client.
- The bootstrap projection grew from five tools to nine. The
  `session_projection_keeps_bootstrap_small_and_requires_activation` test now
  bounds it at nine and asserts the market tools are present, so future growth
  still has to be deliberate.
- `market.install` triggers an approval prompt in clients that implement the
  approval center. Headless or non-interactive callers see the plan and the
  refusal; they do not get a silent install.
