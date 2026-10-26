# Agent credentials have one catalog and a brokered runtime boundary

Status: accepted (2026-10-25)

## Context

The Agent now owns several kinds of authentication state:

- workbench OAuth identities and per-connection refresh state;
- AI service API keys and provider routing;
- GitHub distribution authorization;
- hard-coded inner-platform and SVN credentials;
- plugin-declared credentials;
- workflow connector secrets and file paths.

They have different lifecycles, but users experience them as one question:
"what account or key does this Agent need, and which capability can use it?"
Today the answer is scattered across Settings, AI Connections, plugin details,
and workflow preflight. Two extension stores already overlap:
`plugin-credentials.json` and `connectors/credentials.json`.

The plugin credential store already proves declaration, DPAPI sealing, status,
and uninstall cleanup. It does not yet provide a runtime broker, so a plugin
cannot actually use a credential it declared. A manifest declaration also is
not an authorization grant.

## Decision

Introduce one **logical credential layer** with four nouns:

1. **Descriptor** — untrusted metadata declared by a plugin, connector, or
   built-in integration. It names credential fields and marks each field as
   public or secret.
2. **Profile** — the user's saved values for a descriptor. Secret values are
   DPAPI-sealed; public values may be shown as account metadata.
3. **Grant** — the consumer allowed to use a profile. In this phase a profile
   is consumer-private. Cross-consumer sharing is deliberately deferred.
4. **Broker** — the Agent-owned runtime gate that checks identity, permission,
   declaration freshness, and user configuration before returning values.

The host exposes one credential catalog snapshot to the UI. The snapshot may
aggregate adapters for existing stores, but it never contains secret values.
Physical storage convergence is a later migration; the first phase is logical
convergence with byte-for-byte compatibility adapters.

### Ownership

| Surface | Owner | Exposure |
| --- | --- | --- |
| Workbench OAuth / refresh token | Agent core | Never exposed to external plugins |
| AI service API key | Agent core | Only the model/provider plane may resolve it |
| GitHub distribution credential | Agent core | Only the distribution/publisher plane may resolve it |
| SVN credential | built-in connector | Resolved through the generic consumer boundary |
| Inner-platform credential | built-in plugin | Resolved through the generic consumer boundary |
| Plugin credential | external plugin | Consumer-private, brokered |
| Workflow connector credential | workflow package | Consumer-private, brokered |

The account center shows summaries and navigation for core-owned identities.
It does not flatten workbench OAuth or AI routing into a generic key-value form.

### Plugin declaration

Credential fields are typed descriptors, not bare strings. A field declares:

- stable key and user-facing label;
- `secret` versus public metadata;
- text/password/file/cookie/url input type;
- requiredness and optional help text.

A credential descriptor is identified by `(owner_kind, owner_id,
credential_id)`. IDs and field keys must be unique within the owner. Saving a
profile rejects fields not declared by the current manifest.

The persisted profile records a fingerprint of the declaration. A plugin update
that changes the declaration makes the old profile "needs review" rather than
silently reusing it. Declarations are untrusted data; installation review is
still part of the trust boundary.

### Runtime broker

External plugins remain short-lived process-per-capability invocations. When a
plugin declares credentials and has `secret.broker.acquire`, the Agent starts a
loopback-only, per-invocation broker session. The child receives only:

- a loopback URL;
- an unguessable bearer token bound to this invocation;
- the current invocation's capability and declaration set.

The plugin requests a credential with its ID and the current capability as
purpose. The broker requires all of the following:

1. valid, unexpired session token;
2. plugin identity and capability match the invocation;
3. credential ID is declared by the current manifest;
4. saved declaration fingerprint matches;
5. the plugin has `secret.broker.acquire`;
6. the requested field set is configured;
7. the request budget and session deadline are respected.

The broker returns public and secret fields only to that child process. It logs
an audit event containing consumer, credential ID, capability, result, reason,
and time. It never logs secret keys, values, cookies, or file contents.

For OAuth-capable core providers, a broker may later return a refreshed,
short-lived access token. Static API keys cannot be shortened by the Agent; the
security benefit is per-process delivery, explicit grants, rotation, revocation,
and audit rather than a false promise of unlinkability.

Workflow connector resolution uses the same consumer authorization and audit
semantics. Because workflow execution happens inside the Agent, it resolves the
secret directly instead of opening a plugin broker session.

### User interface

Settings has one **账号与凭据** catalog with filters:

- base identities;
- built-in integrations;
- plugins;
- workflows.

Rows show owner, credential label, configured/needs-review/invalid state, public
account metadata, recent use, and one manage action. Core-owned rows link to
their specialized surfaces. Plugin and workflow rows use the same descriptor
form component. Plugin details and workflow preflight link back to the same
catalog/API instead of defining a second credential contract.

## Consequences

- The Agent gains a stable catalog and broker boundary without pretending all
  credentials have identical lifecycle semantics.
- Existing credential files remain readable through adapters and can be migrated
  incrementally behind a rollback point.
- A plugin can no longer call a generic store API; it must cross the broker.
- Plugin manifest compatibility must accept legacy string fields while emitting
  normalized field descriptors.
- Shared accounts, generic OAuth device flow, organization-managed credential
  sync, and secretless egress proxying remain out of scope for this phase.

## Rejected alternatives

- **Use one flat key-value vault for every credential.** Rejected because
  workbench identity, AI routing, OAuth refresh, and plugin fields have different
  state machines and failure modes.
- **Give plugins plaintext values in environment variables at startup.**
  Rejected because environment variables outlive the specific request, are
  visible to the process tree, and cannot support per-use audit.
- **Let a manifest declaration imply permission.** Rejected because plugin
  updates could silently expand their credential access.
- **Share every saved credential across extensions by default.** Rejected
  because isolation is the safe default and sharing needs an explicit grant.
- **Migrate every credential file before shipping the catalog.** Rejected
  because a failed migration would block unrelated local capabilities; adapter
  compatibility is the safer first step.
