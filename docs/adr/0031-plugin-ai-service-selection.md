# Plugins select AI services without owning provider configuration

Status: accepted (2026-11-16)

## Context

ADR 0012 permits domain plugins to display provider/model/voice choices. ADRs
0016 and 0023 require a service/model execution identity, while ADR 0024 supplies
declared view and backend capability access. The global capability routes are
appropriate defaults for conversational calls, but cannot represent a plugin
project's chosen image or speech model.

The implementation also had a correctness gap: the media bus planned by service
alone, then overwrote an invocation's model with the route's model. A picker
would have displayed a choice that did not reach the provider.

## Decision

1. Agent remains the owner of provider inventory, credentials, adapters, global
   defaults, execution and artifacts. Plugins receive safe discovery metadata
   and execute registered capabilities; no new provider registry is introduced.
2. A per-invocation `selection` is one of `agent_default`, `service_default`
   (service ID), or `pinned` (service ID and model). A service-only selection
   follows that service's capability binding, independent of the model pinned
   in the global route table. A pinned model missing from the inventory is
   unavailable, never silently replaced with a service default.
3. Legacy `service`/`model` map to these semantics. A model-only legacy request
   pins the resolved global primary. Combining nonempty legacy selectors with
   `selection` is an error. Unknown fields in a selection are errors.
4. `fallback` is `none` or `configured`. Agent-default calls retain configured
   same-capability fallbacks. Explicit service/model choices default to `none`;
   `configured` is a caller's explicit opt-in. Every fallback uses its own model.
   This intentionally tightens old explicit-service behavior. A fallback may
   change the chosen service/model only under this explicit policy.
5. `media.route.plan` is a public, read-only, local and nonbillable capability.
   It uses the exact resolver used by execution, optionally preflighting a
   payload without contacting providers. It is a preview, not a reservation.
   Results preserve the requested selection, its advisory preference source,
   selected route, eligible attempts and fallback policy. Successful execution
   adds `actual`, attempt history, usage and artifacts to the existing envelope.
6. Extend `media.provider.catalog` with optional capability, execution mode,
   required adapter features and local-ready filters. The public plugin catalog
   excludes endpoints and unrelated chat inventories when scoped by a filter;
   empty legacy requests retain their original fields. It contains capability
   model alternatives, local eligibility, adapter metadata scope and voice
   catalog support. Credentials present means configured, not tested healthy.
   Stream/async configuration is ineligible when the adapter cannot execute it.
   Limits/features are adapter metadata, not per-model certification.
7. Preference order is invocation override, plugin project preference, plugin
   default, Agent default. The framework-neutral client persists plugin-owned
   preferences; other languages/clients can implement this order using the same
   capability contract. `selection_source` is diagnostic input, not identity.
8. Ship a native-control selector and client as versioned, embedded SDK assets
   beneath an enabled plugin view's `_himind/ai-v1/` URL. Accessing this library
   grants no capability. Every call still passes the existing view declaration
   check or backend invocation broker. Plugins cannot change global routes through
   the selector. The UI retains unavailable saved selections and blocks execution
   until corrected; it does not silently reset them.
9. Voice identity is provider-specific and separate from synthesis model identity.
   The selector invalidates dependent choices when a target changes. The client
   resolves voice catalogs against the selected speech service and explicitly
   reports unsupported catalogs. Domain plugins own voice selection/licensing.
10. Batch callers resolve and pin their selection at submission. Built-in visual
    tagging persists the exact service/model and reuses it after restart. Legacy
    jobs lacking this identity require recreation with an explicit selection;
    they cannot silently inherit a changed global default.

## Alternatives considered

- **Global route settings in each plugin:** rejected because editing one project
  would affect unrelated sessions and plugins.
- **A new service registry and vendor SDK in each plugin:** rejected because it
  duplicates credentials, adapters and compatibility rules.
- **Only add a UI picker:** rejected because the old execution bus could overwrite
  its selected model, and headless/back-end clients need the same contract.
- **Only accept a concrete service/model:** rejected because following Agent or
  service defaults is useful and should remain intentional.
- **Unconditional cross-service recovery:** rejected because a fixed selection
  may express cost, privacy, output or voice requirements.
- **A server reservation/lease for every preview:** deferred; ordinary calls
  resolve once at execution. Explicit batch pins cover the present drift risk.

## Consequences and boundaries

Existing global routing and envelopes remain compatible; explicit service-only
fallback behavior changes as described above. Old clients may continue sending
legacy fields. The new selector requires an Agent build containing this SDK.
Preference storage is local to the plugin client; it is not cross-device sync.
No installed third-party plugin is automatically rewritten. The shipped
image/audio reference demonstrates adoption without adding a business plugin
or changing its installation state. New adapters still require real upstream
compatibility testing before claiming model-specific support.

Focused validation covers exact image/audio HTTP request models, legacy and
structured selection, invalid models/fields, fallback planning, public Gateway
contracts, plugin permissions, SDK preference isolation, browser races, stale
preferences and offline refresh. The integration guide records test provenance.
