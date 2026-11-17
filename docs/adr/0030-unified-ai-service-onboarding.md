# One service connection, one model catalog, explicit operations

Status: accepted (2026-11-17)

## Context

A provider account can expose chat, image, video and audio models using one
connection and credentials. The old add form put model discovery under chat,
required separate model-ID fields for each operation, and repeated protocol
configuration in advanced settings. It presented one account as several kinds
of service and made media-only discovery unnecessarily hard.

Discovery currently returns model IDs, without authoritative operation metadata.
A listed ID proves neither that it supports a particular operation nor that an
inference request succeeds. Legacy vision fallback also made a disabled checkbox
reappear after saving, and a late catalog response could belong to a connection
the user had already changed.

## Decision

1. Keep one service entity with endpoints and credentials. Do not split storage
   into mutually exclusive chat/media service types. Keep ADR 0023's operation
   routes and per-operation model inventories.
2. Use a two-step modal: **connect service**, then **select models**. A preset
   supplies the endpoint/protocol. The user ordinarily enters only a key; the
   name is optional. Custom connections expose address/protocol. Second endpoints,
   manual IDs, per-operation defaults, adapters and balance queries remain
   available as disclosures.
3. Offer one discovery action independent of selected operations. Deduplicate
   model IDs and allow each selected model to have multiple uses. The selected
   inventory is projected back into the existing chat/media fields; unused
   discovered IDs are transient, not saved as chat models.
4. Show catalog provenance separately from operation selection and validation.
   Exact, curated preset metadata may suggest uses; unknown IDs require the user
   to choose their uses. Never infer verified capabilities from name patterns.
   All entries state that inference has not been verified. Reopening shows
   **saved configuration**, not historical proof of discovery. Credential changes
   label retained choices as **previous selection**, not a fresh service catalog.
5. Changing endpoint/protocol clears the old model setup. Changing credentials
   invalidates discovery, retaining selected models for deliberate reuse. A
   revision guard rejects old success/error/finally callbacks after connection
   changes, preset switches, close, reopen or a newer discovery request.
6. Editing with an empty key may use the saved-key discovery API only while the
   endpoint and protocol still match the saved service. That API intentionally
   queries the saved endpoint; it must not appear to test an edited address.
   Unsupported catalogs remain usable through presets and manual configuration.
   Agent Plan's billable inference probe is not triggered by onboarding discovery.
7. Persist optional `enabled_capabilities` alongside existing service fields.
   Missing/null preserves historical inference, including implicit vision.
   An explicit list limits enabled uses; the form sends it on save. Save rejects
   empty/unknown uses and enabled operations without models, and removes disabled
   bindings. Explicit mixed uses bypass the legacy “media model cannot be chat”
   heuristic. Catalog and binding resolution both respect the list. Old upsert
   clients preserve an existing explicit list. All settings are saved under the
   existing directory lock; credentials remain protected as before.
8. Saving does not write global primary/fallback routes. Show an optional
   **set defaults and backups** action afterwards. Existing automatic selection
   still applies when a global route is absent; adding a candidate can therefore
   affect automatic candidate resolution without rewriting the route table.

## Alternatives considered

- Separate media/chat services: rejected because it duplicates credentials and
  fragments a single account. Adapter differences belong to operations.
- Only rename the old capability cards: rejected because it leaves discovery,
  duplicate model entry and implicit vision semantics unchanged.
- Automatically classify every model by its name: rejected because provider
  aliases and incomplete catalogs would look like verified capabilities.
- Replace the entire service store with a new provider/model registry: deferred;
  the existing model bindings plus an optional operation list express this flow
  without a broad migration.
- Force all setup into one flat form: rejected because custom adapters/account
  configuration would burden the ordinary provider/key path.

## Consequences and limits

Unknown models need a user-selected use until providers expose reliable metadata.
Generic directory discovery is not universally available; adapter selection for
custom native APIs may still require advanced configuration. A directory failure
does not prevent manual save. This change adds no new provider adapters and does
not certify inference, pricing, quotas or all template model IDs.

Discovery provenance is session-local; the persisted configuration records
selection and enabled uses, not an inference-validation ledger. Existing records
are not bulk-migrated. Saving an old service in the new form makes its displayed
uses explicit. Disabled operations can invalidate existing pinned routes, which
remain visible for the user to repair rather than silently choosing replacements.

## Validation

- `npm run check:ai-service-onboarding`: model merging, unknown uses, default
  fallback, multi-operation IDs, media/chat isolation and legacy inference.
- `npm run check:ai-service-onboarding-browser`: actual React components in
  Chromium with mocked host callbacks; nine scenario groups cover minimal preset
  setup, vision-only and media configurations, bridge argument mapping, routing
  navigation without route writes, failure/retry, request races, edited saved-key
  discovery, advanced settings, no-catalog providers and ordinary narrow layouts.
- `npm run build`, routing, preset and balance-matching checks.
- `cargo test --bin himind-agent store::ai_services::tests -- --test-threads=1`:
  48 storage/routing tests, including explicit operation persistence, legacy
  callers, rejected invalid writes and unchanged route-file bytes.

Browser results certify component behavior and API argument construction, not
installed-host acceptance or external provider inference. No deployment is part
of this change. Screen readers and 200% zoom are explicitly outside this scope.
