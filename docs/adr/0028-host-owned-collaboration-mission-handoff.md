# ADR-0028: Cross-client collaboration handoff creates a host-owned Mission

Status: accepted (2026-11-10)
Deciders: Product owner and HiMind architecture

## Context

Codex and other clients are effective places to discuss a goal and shape an
implementation plan. A client-local long-task loop, however, does not provide a
shared, host-enforced control plane for multiple roles, runtimes, approvals,
recovery and independent verification. HiMind already has a persistent Mission
model and reuses WorkflowRunner/LocalRun for task attempts; reusable Workflow
Packages have their own versioning and distribution lifecycle. These layers
need to connect without creating two execution owners, duplicating a scheduler
in a skill, or requiring every client to implement its own assignment and
authorization UI.

The current Mission UI can edit a task's role ID, executor and model, but there
is not yet a client-neutral handoff contract, unified assignment surface,
Mission-to-collaboration-workflow binding, or host-owned scheduler that
continues independently of a Codex/MCP session. This ADR records the target
architecture; it does not claim those capabilities are already implemented.

## Decision

1. **Keep three distinct lifecycles.** A collaboration Workflow Package is a
   reusable, versioned method; a Mission is one user's goal and execution
   instance; a LocalRun is an execution attempt. A Mission pins the workflow ID,
   version and content digest it uses. A workflow update never mutates an
   existing Mission.
2. **Make Mission handoff client-neutral and host-owned.** Agent UI, Codex and
   future clients submit the same structured Mission draft to the HiMind
   control plane. The draft records an idempotency key, source client/reference,
   workspace identity, goal, plan mode and relevant context. The host validates
   the workspace and owns canonical Mission state. A client conversation is a
   source, not an implicit filesystem grant or execution authority.
3. **Separate handoff from assignment and execution.** Handoff creates an
   `awaiting_assignment` draft and never dispatches work. The Agent owns one
   authoritative assignment UI. When available, it opens without stealing
   focus; otherwise HiMind raises a notification or returns a stable deep link
   and assignment handle. Clients without interactive UI support use the same
   pending state and fall back to the Agent UI rather than inventing a
   client-specific approval flow.
4. **Configure workflow roles, executors and model services separately.**
   Roles and responsibilities come from the selected workflow, not free-form
   role IDs in the normal user path. The user can confirm which defined roles
   are used and map proposed tasks to those roles. Each role may have a
   recommended execution binding and a per-Mission override. The binding distinguishes executor/
   runtime from service profile and model. If an executor controls its own
   model configuration, the UI says so and does not imply HiMind changed it.
   Credentials remain in the Agent's service store and are never embedded in a
   workflow package.
5. **Provide recommended bindings, not bundled consent.** The assignment UI
   offers “Use recommended services and models” across roles, with per-role
   customization. This action applies configuration only: it does not approve
   a plan, grant read/write access, or start execution. Starting requires
   confirmation of the current plan digest, selected tasks, required checks and
   exact read/write scopes, including explicit decisions for absolute paths.
6. **Use one execution owner per task.** Before handoff, the source client may
   plan and edit. After HiMind accepts execution, Mission is the source of truth
   and the source client must not modify the same workspace in parallel. A
   Codex native Goal may continue as a read-only monitor, but must not dispatch
   or implement the delegated Mission. HiMind may invoke Codex CLI as a child
   executor through LocalRun without transferring Mission ownership. If the
   source Codex session remains the execution owner, use the long-task driver's
   native Goal and do not create a competing Mission.
7. **Keep dispatch independent of the source client.** A persistent HiMind host
   scheduler owns queued dispatch, verification, recovery and bounded follow-up.
   The MCP process and Codex session are control/observation channels, never the
   durable worker. On host restart, the scheduler reconciles persisted
   Mission/LocalRun identities before resuming and preserves the existing
   single-writer workspace claim. Work cannot progress while its host is
   powered off, but it must recover without the originating client session.
8. **Support two explicit plan modes.** `imported` preserves a user-reviewed
   source-client plan as a proposal for host validation. HiMind may report
   defects but must not silently rewrite it. `generated` asks the selected
   collaboration workflow to propose a dynamic task graph from the goal. In
   both modes, the user confirms the exact host-validated plan before dispatch.
9. **Reuse Workflow Package distribution.** A collaboration workflow declares
   reusable role contracts, planning/execution policy, recommended bindings,
   acceptance rules, input/output contracts and supported runtimes. It contains
   no Mission IDs, private conversation payloads, machine-local paths,
   credentials, grants, run logs or usage. Export is explicit and reviewable;
   installation creates a new Mission with fresh workspace validation and
   consent.
10. **Keep skills as orchestration guidance.** Add a separate
    `collaboration-task-handoff` skill to prepare a structured handoff, invoke
    the HiMind control plane, return the Mission reference and query status.
    `long-task-driver` only routes between Codex-owned continuation, HiMind
    handoff and fixed Workflow execution. It may prepare a partial-progress
    handoff but does not duplicate Mission state or scheduling.

The host-owned Mission control surface uses the registered "mission" capability
domain in `contracts/capability-naming-rules.json`. Its `mission.*` IDs follow
ADR-0119 naming rules and remain distinct from reusable `workflow.*` methods.

## Required invariants

- Retrying handoff with the same idempotency key cannot create a duplicate
  Mission.
- No task is dispatched before the user confirms the current plan digest, task
  selection and required scope grants.
- Service/model choice, task-plan confirmation and filesystem authorization are
  distinct decisions.
- A Mission pins workflow and resolved execution-binding identities;
  unavailable bindings fail closed rather than silently switching providers or
  models.
- Source context is minimized, attributable and treated as untrusted task
  input; it cannot grant tools, paths or permissions.
- Mission events, checks and verification evidence remain bound to the plan and
  current candidate under the existing host-verification contract.
- Closing or restarting Codex/MCP cannot cancel an accepted Mission. Terminating
  HiMind preserves state and allows safe recovery on restart.
- A workspace has at most one active Mission writer under the current MVP
  policy.

## Rejected alternatives

- **Put the full Mission protocol into `long-task-driver`.** Rejected because a
  long task can remain Codex-owned and a collaboration handoff can be short;
  combining their state machines would make the skill both a client continuation
  loop and a second Mission controller.
- **Let each client implement its own role/model/approval dialogs.** Rejected
  because client capabilities differ and would produce inconsistent
  authorization, configuration and audit behavior. The Agent is the canonical
  assignment surface.
- **Let the source client run the Mission after handoff.** Rejected because
  closing the client would stop progress and make Mission durability depend on
  the transport session.
- **Treat each Mission as a Workflow Run or export the completed Mission
  directly.** Rejected because a Mission is mutable instance state with grants,
  evidence and repair history; a Workflow Package is a reusable, distributable
  definition.
- **Make “use defaults” also confirm and start the task.** Rejected because
  service selection is not consent to filesystem access or to a specific plan.
- **Allow Codex and HiMind to remain concurrent writers.** Rejected because the
  current host lease cannot control edits made outside the Mission runtime,
  risking candidate drift and lost updates.

## Consequences

### Positive

- One Mission can be handed off from multiple clients and observed from any
  client connected to the same HiMind host.
- Users get one consistent place to assign workflow roles, apply recommended
  service/model bindings and grant task-specific access.
- HiMind can continue and recover work without the originating Codex or MCP
  session.
- Reusable collaboration methods can be versioned, reviewed and distributed
  through existing Workflow Package channels without publishing task-private
  state.
- `long-task-driver` remains useful as a general Codex skill and gains a small,
  explicit routing/handoff responsibility rather than becoming a second
  scheduler.

### Negative

- The control plane needs Mission draft/assignment APIs, idempotency and source
  references, client notifications/deep links, workflow-version pinning and
  persistent scheduling/recovery.
- Executor and model-service capabilities vary. The assignment UI needs
  capability-aware options and must distinguish unsupported model overrides
  from successful bindings.
- The source client must stop editing after handoff. The HiMind host must
  resolve workspace identity rather than trust a client path string.
- This is more complex than a single Codex Goal and should be used when
  multi-role execution, host policy, independent verification, client handoff
  or reusable collaboration justifies that cost.

### Risks and mitigations

- **MCP retry or reconnect duplicates work:** enforce a unique idempotency key
  and return the existing Mission on replay.
- **A stale workflow or service profile changes execution:** pin version/digest
  and resolved binding at confirmation; fail closed when unavailable.
- **A client over-shares conversation context:** send a minimal reviewed
  snapshot and message references; never forward credentials or treat
  transcript text as authorization.
- **The Agent window is unavailable:** retain `awaiting_assignment`, notify
  with a deep link and do not dispatch.
- **The source client writes after transfer:** surface the ownership transition
  and use host candidate/claim checks; conflicting external edits require
  revalidation before continuing.
- **Host shutdown is mistaken for background execution:** communicate that the
  Mission is durable and recoverable, not executable while the host is powered
  off.

## Implementation gates

1. Define the versioned Mission handoff/assignment contract and expose it to
   connected clients, with idempotent draft creation and source/workspace
   identity validation.
2. Implement the Agent assignment UI and canonical role-to-executor/service/
   model binding, including recommended defaults, per-role overrides and
   capability-aware failure states.
3. Bind Missions to immutable collaboration Workflow Package versions; support
   imported and generated plans without silently changing imported plans.
4. Move dispatch/recovery ownership into the persistent HiMind host so
   client-session closure cannot stop an accepted Mission. Validate restart
   recovery, workspace exclusion and bounded follow-up.
5. Add `collaboration-task-handoff` and a narrow `long-task-driver` routing
   integration only after the control-plane contract is callable from Codex.
   Validate handoff, assignment, execution and result retrieval end to end from
   at least two client surfaces.
6. Add explicit, sanitized Workflow export from a completed Mission and route it
   through existing validation/review/distribution gates; do not auto-publish.
