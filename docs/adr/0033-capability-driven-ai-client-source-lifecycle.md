# Capability-driven AI client source lifecycle

Status: accepted (2026-11-17)

## Context

A service connection is reusable across clients, but client authentication,
configuration precedence and model selection are not uniform. Codex distribution
changes its default provider and API login policy; OpenCode registration adds a
provider without selecting it; Claude Code user settings can be overridden by
project, environment or managed settings. Removing a binding restores history,
which may be another third-party provider, rather than selecting an official
subscription. Provider presence alone proves neither selection nor authentication.

## Decision

1. Retain the service inventory, operation routes, inference gateway and existing
   client adapters. Native subscription credentials remain owned by the client;
   they are not exported as reusable Agent services or media credentials.
2. Publish each adapter's registration effect, supported source actions, selection
   instructions and reload instructions. Additive providers, default-changing
   clients, model slots, configuration profiles and external managers remain
   distinct. Unsupported actions return an error before any write.
3. Observe registration, Agent management, configured source and default model
   separately. Authentication and actual loaded-session state remain unknown
   until supported runtime evidence exists. State the exact configuration path
   and scope; no claim that this covers project, process or organization overrides.
4. Add source.plan/source.apply to the existing capability gateway, shared by
   Tauri and MCP. Plans contain affected fields, scope, warnings and a revision
   over the requested target, input files and binding record. Apply requires that
   revision; external changes require explicit drift consent. No token values are
   returned in plans. The source API does not replace existing import contracts.
5. Codex native-account activation selects OpenAI and ChatGPT authentication,
   clears incompatible default model/catalog/API policy/endpoint overrides from
   the selected config, removes Agent provider registration, and stops its Agent
   binding synchronization. Claude Code activation clears local endpoint/API/model
   overrides and credential helper policy, then delegates login to Claude Code.
   Process environment and managed policy are not rewritten. Preview names the
   affected keys; a timestamped config backup permits manual rollback.
6. OpenCode registration remains additive. An explicit default-model action must
   select an exact registered Agent model. Preserve the pre-selection model for
   removal/restoration, including migration of older bindings without a snapshot.
   If a user subsequently selects a different provider, removal preserves it.
7. Historical restore is separate from native account activation. New source
   restoration is exposed only for the three validated configuration surfaces
   with a snapshot. Existing removal remains available for other adapters, with
   accurate conservative-cleanup wording. Native switching retains saved Agent
   service definitions, but does not retain an active synchronization binding.
8. UI is capability-driven. Native account controls are available even without
   an Agent service. A file write is reported as requiring client verification;
   running clients are never silently restarted.

## Alternatives considered

- Universal official/Agent toggle: rejected; multi-provider clients need no such
  exclusive mode, and some clients have no native subscription mechanism.
- Represent subscriptions as ordinary API services: rejected; login entitlement,
  token ownership and supported delegation differ from generic API access.
- Rewrite all adapters or build an account center: deferred; the existing import,
  snapshot, drift and gateway mechanisms support this bounded extension.
- Assume registered means active: rejected; inactive providers and external
  configuration overrides invalidate that inference.

## Consequences and limits

New clients can declare their mechanism without adding frontend client-name
branches for every operation. Per-client transformations still need independent
validation. Codex/Claude user configuration and OpenCode file defaults are the
first writable source surfaces; broad instance enumeration, organization-policy
editing, live account entitlement detection and in-session hot switching are not
implemented. CLI and desktop may share a directory; changes affect consumers of
that directory, not a promised single running conversation. Tests use isolated
homes/config files and mocked host UI; no production subscription is switched by
the implementation verification.
