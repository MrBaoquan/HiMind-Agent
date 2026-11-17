# ADR-0029: Goal-first expert teams and bounded parallel Mission execution

Status: accepted (2026-11-16; architecture accepted, implementation tracked separately)
Deciders: Product owner and HiMind architecture

## Context

ADR-0028 established one host-owned Mission, pinned Workflow definitions,
separate assignment/consent and reusable team methods. The current create UI
requires users to pick an installed collaboration Workflow before describing
their need. All declared roles must be assigned, even if unused. Mission
dispatch holds an exclusive Mission lock for an entire execution and
`next_ready_task` refuses admission while any task is running or awaiting
verification. A DAG therefore executes as a serial queue. The expert directory
also deduplicates versions by filesystem enumeration order, which can hide the
version needed by a historical pin.

The product owner now requires a simple goal/workspace/access entry, clear
progress, expert identity consolidation and parallel execution wherever
dependencies and resources allow it. This extends the MVP single-writer policy;
it does not remove its protection for a shared mutable directory.

## Decision

1. **Use one definition/instance hierarchy.** A collaboration Workflow is a
   versioned team method, a Mission is one goal's mutable runtime state, a Task
   is a work unit and a LocalRun is one attempt. Team methods use existing
   Workflow packaging/distribution. Ordinary deterministic Workflows keep their
   lightweight execution path. Both share execution, authorization, budget,
   event and result projections; neither creates a second Mission owner.
2. **Make the normal entry goal-first.** UI and MCP submit the same requirement,
   workspace and access intent to a host application service. It chooses an
   applicable installed method or prepares a private, versioned method
   candidate, selects only necessary responsibilities and recommends installed
   experts and compatible execution bindings. The actual method and experts
   are pinned before execution. A missing dependency is an explained pending
   condition, never an invented expert or silently substituted executor.
3. **Keep consent explicit.** Recommendation and default-model selection are
   configuration actions. A user may review them with the latest plan and
   access scope in one flow, but execution still binds the confirmed plan
   digest, tasks and grants. Existing authorization may be reused only where
   it actually covers the new action. A scope expansion requires a presented
   difference. `full_access` is an executor capability, not administrator
   elevation or a promise that HiMind can intercept all native client tools.
4. **Separate expert identity from responsibilities.** Experts own professional
   methods and deliverable standards; Workflow roles express work required
   for this run. Optional unused responsibilities do not require binding.
   Technical specializations belong to an expert configuration unless they
   have independent methods and acceptance standards. New recommendations
   consolidate overlapping project/system architect identities under technical
   architecture. All old IDs, versions and exact pins remain resolvable.
5. **Make parallelism the default scheduling policy.** Admit the ready set
   allowed by successful dependencies, resource compatibility, configured
   executor capacity and remaining budget. Initial new-task concurrency is
   bounded at three and configurable. Existing plans without a concurrency
   contract retain their legacy serial behavior; changing a confirmed plan's
   policy is explicit. Independent work must not wait for an unrelated review.
6. **Isolate execution and then integrate.** Read-only workers inspect the same
   immutable input candidate and own separate outputs. Code writers work in
   host-managed task workspaces/worktrees with a recorded source baseline.
   Approved scope is mapped to those workspaces without broadening grants.
   The shared source directory still has one integrating writer. Worktrees do
   not by themselves isolate shared databases, devices, ports, generated
   directories or external side effects: declare and claim these resources.
   Incompatible changes/resources are serialized with an explicit wait reason.
   Non-Git or dirty sources require a validated snapshot strategy; otherwise
   keep write operations serial and explain the limitation.
7. **Use short admission transactions and per-attempt ownership.** Replace the
   whole-run dispatch bottleneck with atomic task/resource reservations and
   attempt leases. Keep bounded worker execution outside the admission lock.
   Distinguish task/run/workspace identities, record each isolated workspace
   and reconcile all active attempts after restart. Multi-client dispatch must
   not duplicate work. Cancel one branch independently; global cancel stops
   and reconciles every owned worker before releasing its resources.
8. **Bind verification to candidates.** Each branch preserves its baseline,
   approved changes, checks and delivery evidence. Integration is an explicit,
   idempotent step; conflicting changes are not silently overwritten. Independent
   verification and required integration checks operate on the integrated
   candidate. Branch checks alone cannot prove the final result. A failed branch
   blocks its dependants while unrelated work can continue under the confirmed
   failure policy.
9. **Govern concurrent consumption.** Admission, elapsed time and usage updates
   share the Mission budget. Known limits reserve capacity before launch;
   providers with delayed/unknown usage expose that limitation and bounded
   overshoot rather than claiming an exact hard token ceiling. Exhaustion
   prevents new admission and follows the declared in-flight policy. A budget
   increase can resume eligible work without resetting attempts or evidence.
10. **Project understandable parallel progress.** The task summary states current
    work, who/what it waits for, last activity and the next action. Detail shows
    parallel lanes and dependencies, with raw runtime details on demand. Distinguish
    queued review, running review, review failure and environment failure.
    Animation follows actual activity; heartbeats do not count as progress.

## Migration and implementation boundaries

Use additive versioned contracts and SQLite expand/contract migrations. Do not
rebind running/historical plans, rewrite expert packages in place, inherit old
grants into a new Workflow run or delete task workspaces before results and
recovery evidence are durable. Production workflows use MCP/application services,
CLI/ACP/SDK execution adapters and persisted events; UI automation is for testing.

The first implementation retains a single OS dispatch owner per Mission wave;
workers within that wave run concurrently outside SQLite admission transactions.
Integration and verification freeze the shared candidate under the same lock.
This is not yet a distributed attempt-lease scheduler. Finite token or monetary
limits conservatively reduce admission to one active task until executors support
hard usage reservations; usage settlement and explicit budget extension remain
durable. Initial write isolation requires a clean, complete Git checkout without
untracked/ignored inputs or initialized submodules. Unsupported sources fall back
to serial execution with a reason. Deletions are rejected during integration;
isolated workspaces are retained for recovery rather than automatically removed.

`mission.intent.create`, `mission.assignment.options/apply`,
`mission.plan.generate` and `mission.plan.confirm` form the shared MCP entry.
Plan generation requires the current revision and checks its input snapshot again
in the save transaction. It produces an unconfirmed proposal and never starts a
worker. Existing pinned-Workflow handoff remains available for imported plans.

The current Codex task implements these changes from the main workspace on its
feature branch. Its own development loop remains source-client-owned. Missions
created for acceptance are isolated test runs, never a second writer to this
development checkout. See [the implementation and acceptance plan](../collaboration-architecture-implementation.md).

## Alternatives considered

- Only improve labels/animations: insufficient while the actual dispatcher is serial.
- Remove the running-task gate without isolation: loses baseline and candidate integrity.
- Parallelize by role order: roles are identities/responsibilities, not dependency edges.
- Treat every Workflow as a Mission: adds team semantics to simple deterministic runs.
- Add a separate team package/scheduler: duplicates versioning, consent and recovery.
- Let worktrees imply all resource isolation: ignores ports, devices and external writes.
- Replace old expert IDs or infer newest versions lexically: breaks exact pins and version ordering.

## Acceptance

The implementation plan defines reproducible scenarios for both deterministic
concurrency semantics and installed cross-executor runs. Evidence records source,
dependency/tool identities and whether a result was executed or reused. Final
candidate changes rerun affected checks; no requirement mandates every result
come from a single monolithic full-suite run. No read-screen or 200% zoom work is
included in this scope, as explicitly excluded by the product owner.
