# Local UI reads stay out of the Agent Core run ledger

Status: accepted (2026-11-03)

## Context

Atelier and the extension-development page load dozens of read-only capability
responses while rendering: asset lists, asset reads, candidate lists, project
reads, creative reads, and reusable-asset resolution. Agent Core previously
recorded every capability invocation as a run with an interaction, start event,
completion event, run update, and projection.

That created four write transactions per poll. During a normal canvas render,
concurrent reads exceeded the best-effort SQLite busy timeout and printed
`database is locked` repeatedly. The page still worked, but the ledger grew to
hundreds of thousands of runs and the audit noise obscured real failures.

Recording every UI poll is not useful audit: it describes rendering frequency,
not a user or AI decision. Disabling read auditing globally would be too broad
because MCP and CLI callers may need an attributable read trail.

## Decision

**Local UI calls to R0/R1 capabilities are excluded from the Agent Core run
ledger. Important sources and every mutating call remain recorded.**

The sampling rule is evaluated after capability resolution and risk
classification:

- `InvocationSource::LocalHttp` and `InvocationSource::Tauri` are local UI
  transports for this rule.
- Effective risk `R0`/`R1` is treated as read-only and is not inserted into the
  capability-run ledger.
- `Mcp`, `Cli`, `Workflow`, `Scheduler`, `DashboardWorker`, and other sources
  keep their run records.
- Any mutating risk tier (`R2` or above) is recorded regardless of transport.
- `system.health`, capability discovery, and capability activation continue to
  follow the existing explicit exclusions.

The capability result, approval checks, plugin call, and domain writes are
unchanged. This decision affects only the Agent Core run audit projection.

## Consequences

- Canvas polling no longer creates a stream of write transactions, projections,
  lock conflicts, or stderr noise.
- The local run ledger grows with meaningful operations rather than UI render
  frequency.
- Per-poll timing and failure telemetry must come from existing process/Capability
  logs, not from `local_runs`.
- A future aggregate metric such as “UI read count per minute” should be emitted
  as sampled metrics, not one run row per call.
- Mutating local UI operations remain auditable through the same local run,
  event, projection, and approval chain as before.

## Rejected alternatives

- **Keep recording every poll.** Preserves theoretical completeness at the cost
  of sustained lock contention, ledger growth, and misleading activity history.
- **Skip all read-only capabilities.** Would remove audit for MCP, CLI, and
  workflow reads that may be meaningful and attributable.
- **Only switch SQLite to WAL.** Improves writer concurrency but still multiplies
  rows and projections for render polling; it does not address the data model.
- **Asynchronous in-process queue.** A larger change requiring durable queue,
  shutdown drain, ordering, and failure policy. It can be added later for
  mutating or sampled metrics without changing this boundary.