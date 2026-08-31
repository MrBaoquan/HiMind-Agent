# HiMind Agent Business Integration Protocol v1

HiMind Agent uses a provider-neutral contract for optional business systems.
The stable protocol ID is `himind-agent.business-integration`, the current
major version is `1`, and the media type is:

```text
application/vnd.himind.business-integration+json;v=1
```

The catalog schema is maintained at
`contracts/business-integration/v1/catalog.schema.json`. A valid catalog
identifies both the protocol and the provider:

```json
{
  "protocol": "himind-agent.business-integration",
  "protocol_version": "1",
  "provider": { "id": "himind.dashboard", "kind": "control_plane" },
  "schema_version": "1",
  "generation": "sha256...",
  "items": []
}
```

Catalog and invocation requests carry these headers:

- `X-HiMind-Business-Integration-Protocol`
- `X-HiMind-Business-Integration-Version`
- `X-HiMind-Business-Integration-Provider`

Capability invocations additionally carry
`X-HiMind-Business-Capability-ID` and
`X-HiMind-Business-Capability-Version`. Providers must reject mismatched
protocol, provider, capability, or version identities.

Each catalog item declares a relative `route`, closed input schema, risk,
scope, execution mode, retry and idempotency policy, concurrency limits, and
approval requirements. Absolute URLs, path traversal, query strings, and URL
fragments are not valid routes.

`himind.dashboard` is the first trusted provider implementation. It is part of
the Agent's business integration layer rather than a normal `.hmpkg` plugin,
because it participates in device identity, OAuth, control-plane calls, and
audit context. Independent mode does not initialize a business provider.
Connected mode initializes the configured trusted provider and exposes only
capabilities whose catalog passes local validation.
