# The workbench is a built-in control-plane provider plugin

Status: accepted (2026-10-24)

## Context

The Agent currently persists one `AgentMode` bit (`connected` / `independent`)
and uses it as a proxy for several unrelated facts:

- the user wants to participate in a control plane;
- an active workbench connection exists;
- the device is registered with that workbench;
- the delegated account authorization is valid;
- the control-plane services should run;
- workbench-owned capabilities are visible.

Those facts are not equivalent. A registered but unauthorized connection is a
normal onboarding state. A valid authorization can be temporarily unreachable.
The current bit also appears in roughly eighty call sites across worker,
capability, projection, approval, update, AI runtime and UI code, so changing
the bit changes the whole product instead of one provider boundary.

At the same time, the capability layer already has the right primitives:
`CapabilityAvailability::ControlPlane`, `control_plane_required`, provider
plugin identities, plugin-held capability declarations and a dynamic Dashboard
catalog. The missing piece is a provider lifecycle above those capabilities.

## Decision

Model the workbench as a built-in, in-process control-plane provider plugin:
`com.himind.workbench.dashboard`.

The provider owns the workbench-specific adapter surface:

- connection and enrollment status;
- delegated authorization status;
- workbench capability availability;
- worker, projection, policy-reconcile and remote-approval service lifecycle;
- workbench status and settings contributions.

The Agent core continues to own the security and governance boundaries:

- DPAPI-sealed refresh tokens and access-token refresh;
- the approval engine and capability gateway;
- the generic workbench connection store;
- OAuth device-flow mechanics;
- plugin lifecycle, permissions and capability registration.

Provider availability is derived rather than persisted:

```text
provider_available =
    provider_enabled_by_user
    && active_connection_exists
    && device_is_registered
    && delegated_authorization_is_valid
```

An online worker is a runtime signal on top of that availability, not a second
application mode. `independent` becomes the user-visible result of having no
available control-plane provider.

## Provider services

The built-in provider declares four service groups. They are host-managed and
must not be copied into arbitrary external plugins in this iteration:

1. `task-worker`: claim tasks from the active workbench and execute them.
2. `projection-sync`: drain the local run ledger to the workbench.
3. `policy-reconcile`: synchronize managed extension policy.
4. `remote-approval`: provide grants and remote approval context.

Each service starts only while the provider binding is available. Stopping a
service never removes local capabilities, local runs or local approvals.

## Compatibility

- `agent-preferences.json` keeps its existing `mode` field for one migration
  window. It becomes a provider enable preference, not the authority for
  capability visibility or service lifecycle.
- Existing `com.himind.dashboard-business` and `com.himind.knowledge` IDs stay
  unchanged. The new provider owns their compatibility grouping.
- Existing workbench APIs, connection records, capability IDs, approval rules
  and local data ownership do not change.
- If the provider is disabled, missing, unregistered, unauthorized or expired,
  local capabilities remain available and control-plane capabilities return a
  machine-readable unavailable reason.

## UI

There is one application shell. The workbench is shown as a provider card and
settings panel with states such as `未配置`, `已登记 · 未授权`, `授权已过期`,
`不可达` and `已连接`. The host must not switch to a second home page merely
because the provider is unavailable.

## Rejected alternatives

- **Keep `AgentMode` and add more fallback branches.** This preserves the
  circular state model and makes every new service another special case.
- **Ship the workbench as an external `.hmpkg` provider now.** The current
  plugin runtime is request/response capability invocation, while the provider
  needs long-lived services, delegated OAuth and host-held secrets. External
  execution should follow only after those protocols are stable.
- **Let plugins hold refresh tokens.** This moves the highest-value credential
  outside the Agent's DPAPI and revocation boundary.
- **Delete the legacy mode field immediately.** Existing installs, MCP child
  processes and tests still read it. A compatibility read and migration window
  is required.

## Consequences

- The core gains a provider registry and availability model, but loses most
  workbench-specific `if connected` branches.
- Provider state changes become testable without opening the UI or starting a
  network worker.
- Multiple control-plane providers are possible later, but this ADR keeps one
  active provider and one active connection.
- A workbench outage degrades only workbench services and capabilities; it
  cannot make local workflows, skills, plugins or personal AI services disappear.
