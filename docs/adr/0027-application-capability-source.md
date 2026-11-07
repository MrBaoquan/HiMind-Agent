# Application capabilities use an explicit source without entering Agent Core

Status: accepted (2026-11-05)

## Context

HiMind Agent is a general AI capability substrate, not a media-production runtime.
Applications need their own capabilities to be discoverable and invokable through
the same capability gateway, but shipping every application capability as an
independent extension would multiply package count, trust boundaries and release
lifecycles. The Agent also must not learn canvas, shot, storyboard, timeline or
other application-specific types in order to route those capabilities.

The existing plugin manifest had a free-form `governance` field but no explicit
package role. Capability descriptors exposed a concrete `source` string, while
consumers had to infer whether a source was core, an application or an extension.

## Decision

1. Plugin manifests declare `package_role`:
   - `application`: capabilities ship and version with an application;
   - `extension`: independently distributed optional capability provider.
   The default is `extension` for backward compatibility.
2. Capability source identity is explicit:
   - `app:<application-id>` for application-owned capabilities;
   - `plugin:<extension-id>` for extension capabilities;
   - `mcp:<server-id>` for downstream MCP capabilities;
   - any other source is treated as `core`.
3. Capability discovery and describe projections expose a domain-neutral
   `sourceKind` of `core`, `app`, `extension` or `mcp`.
4. Agent Core remains domain-neutral. It may validate generic package roles,
   source identities, risk, approval, run, artifact and workflow contracts, but
   it must not import application schemas or implement canvas/media domain rules.
5. Application documents and operations, such as CanvasDocument and
   CanvasActionPlan, stay in the application layer. The Agent only executes the
   capabilities declared by those documents.
6. `extensions.json` schema version 2 requires ownership metadata:
   `ownership`, `consumer_ids`, `reuse_scope`, `lifecycle`, `disable_safe` and
   `owner`. An item owned by `extension` must have at least two independent
   consumers, ecosystem reuse scope, independent versioning and safe disable.

## Rejected alternatives

- **Infer application ownership from ID prefixes.** Rejected because it hardcodes
  application namespaces into the generic Agent.
- **Make every application capability an extension package.** Rejected because it
  creates artificial install/version boundaries and violates extension reduction.
- **Keep source as an unstructured string.** Rejected because consumers cannot
  mechanically distinguish core, app, extension and MCP ownership.
- **Put canvas or media workflow schemas in Agent Core to simplify routing.**
  Rejected because it turns the general substrate into one application runtime.
- **Require every installed package to adopt v2 metadata immediately.** Rejected;
  schema v1 remains readable while v2 enforces the stricter gate.

## Consequences

- Applications can register capabilities without adding new extension packages.
- Agent consumers gain a stable, domain-neutral ownership signal.
- Existing extension packages default to `extension` until explicitly migrated.
- Application capability registration must remain versioned with its application.
- v2 extension catalogs that lack consumer or lifecycle evidence fail validation.
- Future application runtimes can reuse this contract without teaching Agent Core
  their domain types.
