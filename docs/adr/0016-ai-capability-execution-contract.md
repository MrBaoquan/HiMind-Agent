# AI capability execution is one explainable contract

Status: accepted (2026-10-25)

## Context

HiMind already has stable primitive IDs for chat, image generation/editing,
video generation, speech synthesis and transcription. It also has a media
provider catalogue with adapter features, limits, execution modes and pricing
metadata. Those pieces are not yet one execution model:

- service selection produces one service or an ambiguity error;
- there is no explicit same-capability fallback order;
- media capabilities have no safe, capability-level self check;
- image, video and speech outputs expose similar facts under different shapes;
- the route UI can explain the selected service, but not skipped candidates or
  fallback attempts;
- video currently has one adapter, making the route resilient in appearance
  only.

The provider landscape has no cross-modal wire protocol. Video, voice and image
providers use different authentication, parameter names, task lifecycles and
error bodies. Modelling all of them as "OpenAI-compatible" would hide those
differences and move vendor failures into runtime.

## Decision

### One internal contract, adapter-specific wire protocols

Every AI invocation is represented internally as a capability execution with:

```text
request:  capability, service, model, input, provider_options, constraints
plan:     primary + ordered same-capability fallbacks + candidate reasons
result:   capability, service, adapter, model, execution_mode,
          request_id, provider_request_id, usage, artifacts, warnings, attempts
```

Provider adapters translate this envelope to their own HTTP protocol. Callers
must never branch on vendor-specific request/response fields. Unknown vendor
parameters belong under `provider_options`.

The six current capability IDs are:

- `chat`
- `image.generate`
- `image.edit`
- `video.generate`
- `audio.speech`
- `audio.transcribe`

The five media IDs remain the same stable public IDs from ADR 0012. `chat`
joins the same routing metadata now; replacing the three existing text wire
protocol paths is deliberately incremental and must not change their semantics.

### Routing is explainable and same-capability only

Resolution order is:

1. an explicit service in the request;
2. the persisted capability default;
3. the active service when it supports the capability;
4. the only configured candidate;
5. otherwise `ambiguous` or `missing`.

Fallbacks are an explicit ordered list persisted per capability. A fallback is
eligible only when it supports the same capability and has a credential. The
Agent never substitutes a different modality or capability. Every skipped
candidate carries a reason, and every attempt is returned in the result.

Billable media fallback is allowed only when the user configured that fallback
order. It is never inferred from unrelated services. Real provider self-checks
require `confirm_cost=true`; without confirmation they return a structured
`confirmation_required` result and perform no network call.

### Results and jobs are normalised

Synchronous results always expose a local request id and an explicit usage
state, even when the provider reports neither. Provider request ids are carried
when available. Artefacts are represented as paths plus media ids, MIME, size
and hashes where known.

Asynchronous media results are represented by `media.job.*` records containing
the selected service, adapter, model, provider task id, progress, attempts and
artefacts. Cancellation and retry remain provider-specific internally but use
one public state vocabulary: `queued`, `running`, `succeeded`, `failed`,
`canceled`.

### Health has two levels

- **eligibility** is local and free: saved service, capability binding, model,
  credential and limits are checked without provider I/O;
- **health** is an explicit test: a non-billable request (for example model list)
  or a confirmed, minimal-cost provider request.

The UI must not present "credential present" as "provider healthy".

## Consequences

- Route plans can explain selection without decrypting credentials.
- The media adapter catalogue becomes the source for model features and limits.
- A second video adapter can be added without changing the public video
  capability or job lifecycle.
- The capability gateway can add envelope fields without breaking existing
  inline/media payloads.
- Text chat keeps its existing request paths in this phase, but is included in
  the same route metadata, fallback order and self-check vocabulary.

## Rejected alternatives

- **Wait for a universal provider protocol.** None exists across text, image,
  video and speech, and waiting would leave provider behavior unnormalised.
- **Automatic fallback to any configured service.** It can change modality or
  silently incur unexpected cost.
- **Treat a stored credential as healthy.** It cannot detect auth, quota,
  region or model availability failures.
- **Put high-level creative policy in the provider plane.** Shot selection,
  licensing, mixing and workflows still belong to extensions.
