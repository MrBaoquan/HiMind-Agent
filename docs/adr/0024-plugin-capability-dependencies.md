# Plugins call registered capabilities through declared dependencies

Status: accepted (2026-11-03)

## Context

Plugins are first-class capability producers: every capability declared in
`plugin.json` is registered with the Agent Gateway. Domain extensions also need
to consume other registered capabilities. `media-workbench` already composes
`audio-production` and `audio-intelligence`, and its UI declares external
capability patterns. That composition is intentional.

The missing symmetric mechanism is for a plugin backend process to call the
same registry safely. Letting plugins call each other as raw processes would
bypass Agent identity, approval, audit, capability aliases and fallback policy.
Letting every plugin receive the full Gateway endpoint would make the manifest
declaration meaningless and could expose management capabilities.

Credentials are a separate concern. The credential broker gives a plugin its
own declared secret for the current invocation. It must not expose the Agent's
AI provider keys. A plugin that needs `media.audio.speech` asks the Gateway for
that capability; the provider plane resolves the Fish or another model key.

## Decision

Plugins may declare capability dependencies:

```json
{
  "capability_dependencies": [
    "media.audio.speech",
    "media.audio.transcribe",
    "audio.*"
  ]
}
```

The Agent validates each pattern with the same namespace rules as plugin view
`external_capabilities`. Management namespaces such as `system.*`,
`plugin.invoke`, `workflow.run.start`, `market.install` and wildcard `*` are
forbidden.

For each plugin invocation with declared dependencies, the Agent starts a
loopback-only capability broker bound to:

- plugin ID;
- the parent plugin capability;
- the invocation deadline;
- the declared dependency patterns;
- a per-invocation token;
- a bounded request budget and recursion chain.

The plugin receives only:

```text
HIMIND_CAPABILITY_BROKER_URL
HIMIND_CAPABILITY_BROKER_TOKEN
```

It posts `{capability, input}` to the broker. The broker validates the token,
deadline, declaration and recursion policy, then calls the normal Agent
Gateway with `InvocationSource::Plugin`. The plugin never learns the Agent
HTTP endpoint, provider credential or other plugins' process endpoints.

Nested calls use the same Gateway resolution rules: aliases, approval,
availability, input schemas, provider routing and audit. The parent capability's
approval covers its declared child calls; child calls cannot request approval
for the user themselves. The broker rejects a cycle or excessive depth.

Capability dependencies do not replace plugin dependencies. `plugin_dependencies`
controls installation/version order. `capability_dependencies` controls runtime
invocation authorization. A plugin may select a compatible installed provider
without embedding its implementation.

## Consequences

- `media-workbench` can continue composing `audio.speech` and other registered
  domain capabilities.
- `audio-production` can call `media.audio.speech` and `media.audio.transcribe`
  without receiving Fish keys or implementing vendor HTTP.
- Cloud TTS providers remain exchangeable; Fish is an Agent adapter.
- Plugin backend code becomes testable against a fake Agent capability.
- The broker is generic and applies to current and future plugin families.
- A plugin's manifest becomes an explicit part of the trust and review surface.

## Rejected alternatives

- **Let plugins call each other directly.** Rejected: bypasses identity,
  approvals, audit, aliases and provider routing.
- **Inject the full Gateway URL and credentials.** Rejected: a manifest
  declaration would become decorative and management capabilities would leak.
- **Give the plugin the AI provider key.** Rejected: credentials remain owned by
  the Agent provider plane; plugin credentials are consumer-private.
- **Put audio production into an Agent super-plugin.** Rejected: project,
  character, beat, mixing and QA policy are domain workflow state.
- **Only expose reverse calls in plugin UI.** Rejected: headless workflows and
  backend batch generation need the same governed contract.
