# ADR-0032: Sustainable coordination, incremental Mission changes and a collaboration workbench

Status: accepted (2026-11-17; implementation and acceptance tracked separately)
Deciders: Product owner and HiMind architecture

## Context

Missions already persist plans, expert bindings, parallel attempts, budgets and
candidate verification. Feedback is observational, and whole-plan replacement is
intentionally rejected during active work. Users need to ask questions and add
requirements during execution without stopping unrelated work. The current
detail view exposes too much configuration before explaining actual teamwork.

## Decision

1. **Extend the existing ownership model.** Mission remains the host-owned
   instance; Workflow is a reusable method; Task and Attempt retain their current
   lifecycle. Coordinator is a Mission responsibility using an allowed service,
   not a duplicate expert asset or another scheduler. UI, MCP/client skills and
   workflow reuse call the same application services.
2. **Persist communication separately from execution.** Distinguish questions,
   human instructions and expert observations. Questions read observed state and
   never mutate plans; observations cannot grant authority. Record attributable
   requests and answered/analyzing/awaiting-confirmation/applied/partially-applied/
   not-applied/failed outcomes. Model work runs outside dispatch locks and long
   database transactions. Charge actual coordinator usage to the Mission budget;
   deterministic status, cancel, pause and budget extension remain available.
3. **Add an incremental change protocol.** Keep the active whole-plan replacement
   gate. Changes record source/request/idempotency identity, base revision/digest,
   requirement, task/dependency/priority/assignment/acceptance deltas, affected
   artifacts, authorization/budget differences, in-flight policy and actual
   outcome. Validate again against the latest state and commit a revision with
   durable scheduling intent atomically or through recoverable reconciliation.
   Reject stale proposals; replays neither call models nor dispatch twice.
4. **Bound dynamic dispatch.** Responsibilities may be bound just before
   admission from real installed experts, skills, executors, services and models.
   Pin exact input and binding identities for each Attempt. Add experts/tasks and
   adjust unstarted work within current grants, service rules, budget and a
   bounded scheduling policy. Access mode and automatic-adjustment permission
   are separate. Ask only for additional authority, not already granted work.
5. **Keep unaffected work valid.** Unrelated branches continue through analysis
   or pending consent. Affected queued work cannot start with obsolete inputs.
   Use real adapter capabilities: checkpoint delivery if supported, otherwise
   finish then replace or cancel and re-dispatch. Do not release conflicting
   resources before confirmed stop. Accept older unrelated results by immutable
   task/input signature, not solely the latest Mission revision; isolate invalid
   or cancelled late results. Invalidate affected artifacts and downstream
   verification, retaining historical applicability. Verify the current integrated
   candidate. Reconcile cancellations, changes, usage and admission after restart.
6. **Use a modern, truthful workbench.** Detail has Collaboration and Delivery
   tabs, defaulting respectively during work and after completion. A stable
   header states one main status, current work/wait/action, completed count,
   actual concurrency and recent activity. Expert task cards show parallel work
   and handoffs; completed groups collapse. Pending user action is directly
   visible. Chat and task details share on-demand space; configuration, budget,
   attempts, raw events and full dependency graph are secondary disclosures.
   Animate real transitions only. Heartbeats indicate liveness, never progress;
   stale/disconnected data is explicit. Preview all key states before production
   components; retain stable identity, selection and scroll positions.
   This is a desktop workspace: the board fills the available window height;
   execution and coordination are peer work areas. On first opening an active
   Mission, the coordination assistant is visible in a right dock with an
   independently scrolling transcript and dock-local anchored composer, never
   appended below the board. Workspace tools switch that single dock between
   coordination and task details, preserving chat drafts and reading positions.
   Close uses a conventional icon button with a tooltip and accessible name.
   Narrow windows overlay that single context pane instead of stacking it
   beneath the board. Collaboration/Delivery tabs remain about task content;
   workspace tools are separate from that tab set.
   Keep status and required actions outside either scroll region; show phase
   history, plan hashes and other diagnostics only in contextual details.
7. **Make reuse explicit.** Temporary methods stay private to a Mission. Only
   explicit save creates a reusable Workflow, removing machine paths, private
   conversations, credentials, prior grants and execution history. A new launch
   validates its own workspace and permissions. Terminal Missions do not reopen
   from chat; explicit extensions create linked follow-up instances.
   Explicit local save preflights and installs an immutable method in My
   Capabilities through the shared UI/MCP service (`mission.workflow.save`).
   Exporting an authoring candidate remains a separate operation and must not
   claim that the method is already installed or reusable from the catalog.

## Relationship to earlier records

This extends ADR-0028 decisions 3–5 and 8: initial reviewed consent remains, but
later work can be incrementally authorized and bound rather than requiring every
role and all future work to be fixed at initial confirmation. Decisions 1–2 and
6–10 remain the ownership/distribution baseline. Its evidence invariant now
includes per-attempt compatibility across plan revisions.

This extends ADR-0029 decisions 2–4 with bounded runtime responsibilities and
authorization deltas, 5–9 with revision-compatible admission, local invalidation
and recovery, and 10 with the two-tab workbench and separate coordinator state.
The parallel isolation and final-candidate verification rules remain. Historical
ADRs and execution records are not rewritten. Expert retirement was handled by
the preceding consolidation; this ADR does not revive retired assets.

## Alternatives considered

- Chat-only forwarding to workers: cannot establish durable plan changes or
  consistent multi-executor outcomes.
- Stop all workers and replace every plan: wastes unrelated work and invalidates
  trustworthy evidence unnecessarily.
- Unbounded autonomous team growth: lacks authorization, budget and completion
  bounds. Use explicit policy limits and host validation.
- A second coordination platform or default full DAG: duplicates existing
  lifecycles or overloads normal task understanding.

## Consequences and acceptance

Persistent coordination adds conflict, recovery and evidence-compatibility cases,
but preserves a single control plane and useful parallel work. Do not promise
mid-run injection where adapters do not support it. Full OS access does not imply
automatic plan or external-side-effect consent.

The [Chinese implementation attachment](../collaboration-runtime-workbench-implementation.zh-CN.md)
defines P0–P4 and A1–A14. Completion requires effective coverage, a verified
artifact deployed locally, and installed HiMind AI + Codex execution with a
runtime requirement change. Evidence distinguishes new execution from proven
reuse. Distributed scheduling, 3D, a rewrite of all Workflows, screen readers and
200% zoom are outside this decision. Accepted architecture is not an assertion
that all functionality or acceptance has already shipped.
