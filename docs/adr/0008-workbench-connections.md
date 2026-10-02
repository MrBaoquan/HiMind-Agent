# Workbench connections are first-class local records

Status: accepted (2026-09-30)

## Context

The Agent talks to a HiMind Dashboard ("AI 工作台") over one HTTP API base.
Until now that base was a process-level fact:

- `--api` / `DASHBOARD_API_BASE` decided it once at startup;
- a debug build silently defaulted to `http://127.0.0.1:18083` and a release
  build to `http://localhost:8080`, so "which environment am I talking to"
  depended on how the binary had been compiled;
- identity was a single set of files (`agent-state.json`,
  `agent-state.device-id`, `agent-user-authorization.json`) with no record of
  which workbench they belonged to.

That is wrong for two real user situations:

1. A developer must test against a local Dashboard and then work against
   production, without reinstalling the Agent or losing the other identity.
2. An ordinary user must be able to point the Agent at a different workbench
   (another team, another deployment) and keep both authorizations.

The hard constraint is the opposite of the usual one: **identity must never be
shared across workbenches.** If a single `agent-state.json` is reused while the
API base changes, the Agent presents environment A's agent credential to
environment B. That is account crossover, and it is a correctness bug, not a
configuration mistake.

## Decision

Model the local install as three layers, and persist them in one new file.

```
Agent                     one instance, one local dataset
  已装能力 / AI 服务 / 审批档位 / 工作区 / 备份 / 本地会话

Connection (工作台连接)    N records, each with its own identity
  id / 显示名 / 用途标签 / api_base
  agent_id / credential / device_id
  user_id / scope / refresh token / 授权时间
  active_connection_id      exactly one connection is active
```

Storage is `<agent_home>/data/workbenches.json`. It is the source of truth for
connection address and identity.

Rules:

1. **Identity is stored per connection.** Every connection carries its own
   snapshot of the onboarding artifacts it needs: the `agent-state.json` body,
   the device id, and the OAuth authorization file. Credentials keep their
   existing DPAPI protection; the store moves sealed blobs, it never re-encodes
   or logs them.
2. **Local data exists once.** Installed capabilities, AI services, approval
   posture, workspaces, backups, and local conversations belong to the Agent,
   not to a connection. Switching connections must not duplicate, hide or
   re-download capabilities.
3. **One connection is active at a time.** `active_connection_id` is a single
   value, and the live API base is read from it on every use.
4. **Legacy files become a materialised view.** On switch the Agent captures
   the outgoing identity back into the store, flips `active_connection_id`,
   then materialises the incoming identity into `agent-state.json`,
   `agent-state.device-id` and `agent-user-authorization.json`. Existing code
   that reads those paths keeps working untouched; the store stays
   authoritative because the capture step runs before every write.
5. **Environment is a connection property, not a build property.** `--api`
   and `DASHBOARD_API_BASE` still exist as explicit overrides for packaging
   and automation, but nothing infers a workbench from `cfg!(debug_assertions)`
   or from `HIMIND_AGENT_PROFILE` any more.

### Adding a workbench is a pairing, not just an address

Inspecting the existing flow settles an open question: device authorization
alone cannot bootstrap a new connection. `begin_device_authorization` calls
`load_agent_state`, so the Agent must already hold an agent identity registered
with that workbench; registration requires an enrollment token that only the
Dashboard can mint (`/api/me/agent/enrollment` → local `/enroll` → `register_agent`).

So "添加工作台" is one flow with three steps, in order:

| Step | What the user does | What the Agent does |
| --- | --- | --- |
| 1. 登记 | Copies a pairing code / enrollment token from that workbench | `POST /enroll`-equivalent against the candidate `api_base`, stores the returned identity under the new connection |
| 2. 探活 | — | `GET /api/health` on the candidate base; never assumes reachability |
| 3. 授权 | Approves the device code in the browser | Device authorization, then the OAuth snapshot lands in the same connection |

A connection created by step 1 but not step 3 is legitimate and must render as
"已登记 · 未授权", not as an error.

### Switching while work is in flight

Switching stops the old worker and starts the new one. Tasks already running
finish against the connection they started on, because the task execution path
reads the credential captured at claim time; the UI asks for confirmation
before switching while a task is executing.

Capabilities that the previous workbench managed stay installed and are marked
as "未在当前工作台受管" instead of being removed. Activation and revocation
stay the only operations that add or drop workbench-managed capability.

## Consequences

- `Options.api_base` becomes shared mutable state behind an `RwLock`; the
  process keeps one `Options` value and every call site reads the live value.
- `HIMIND_AGENT_PROFILE` and `HIMIND_AGENT_HOME` keep their meaning, but only as
  engineering isolation for tests and parallel verification. They no longer
  imply "this is the development environment".
- Migration is idempotent and runs on first load: an install with only legacy
  files gets exactly one connection, seeded from the effective API base and the
  existing identity.
- `profiles\development` stays a separate instance and is deliberately *not*
  merged into the production store; merging two independent installs would
  reintroduce the crossover this ADR removes.
- The consumer-side (Agent UI) contract grows: list, add, rename, remove,
  switch and probe a connection, plus authorize/re-authorize a specific
  connection rather than "the" connection.

## Alternatives rejected

- **One Agent per workbench (separate `agent_home`).** It keeps identity
  separate for free, but it also forks installed capabilities, AI services,
  approvals and backups. The user would have to install the same capability
  twice and would lose the "one digital twin" model.
- **Rewrite the API base on switch and keep a single identity.** Smallest
  diff, and exactly the account-crossover bug described above.
- **Device authorization as the only bootstrap step.** It cannot work: the
  device flow needs a registered agent identity, and only the workbench can
  issue the enrollment token that produces one.
- **Keep `cfg!(debug_assertions)` deciding the default environment.** It makes
  "which workbench am I on" a property of the build, which is precisely what
  the user cannot see and cannot control.
