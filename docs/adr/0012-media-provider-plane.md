# Media providers are an Agent capability plane

Status: accepted (2026-10-23)

## Context

The Agent already owns `media.image.generate`, `media.image.edit`,
`media.video.generate`, and the local `media.job.*` lifecycle. At the same
time, domain extensions own the producer-facing workflows:

- `audio-production` owns beat-level TTS, voice selection, licences, BGM,
  mixing, manifests, and audio QA.
- `audio-intelligence` owns alignment, subtitle sync, and intelligibility QA.
- `media-visual` owns reference packs, continuity checks, and visual QA.

Two constraints make a purely plugin-owned cloud provider layer unsafe:

1. Plugins cannot currently receive plaintext credentials. A cloud TTS or
   video adapter cannot assume that declaring a credential makes it usable.
2. Provider credentials, model limits, health, retries, and output
   normalisation are shared across applications and extensions.

The existing local media plane already proves the split: image/video providers
are Agent capabilities while references, shots, quality rules, and projects
remain domain logic.

## Decision

The Agent owns a shared **media provider invocation and governance plane**.
Domain extensions keep ownership of producer-facing workflows.

The Agent plane owns:

- provider and model registration;
- capability bindings and model limits;
- credential-protected invocation;
- health and connection checks;
- `sync`, `stream`, and `async` execution modes;
- normalised inputs, outputs, errors, usage, and artefact references;
- retry/fallback policy and the reason a fallback was or was not allowed.

Public provider primitive IDs are:

- `media.image.generate`
- `media.image.edit`
- `media.video.generate`
- `media.audio.speech`
- `media.audio.transcribe`

These IDs mean **one provider invocation**, not a complete production
workflow. Domain IDs keep their existing meaning. In particular,
`audio.speech` is the beat-level, licence-aware audio-production capability;
it is not a replacement for `media.audio.speech`.

All media requests use a common envelope and may carry provider-specific
parameters under `provider_options`. The Agent must not grow fields such as
`beat`, `voice_license`, `mix`, `cover`, or `storyboard`.

Execution mode is selected by the provider/model capability:

- `sync` returns the normalised result directly;
- `stream` exposes progress or media chunks;
- `async` returns a job handle and uses `media.job.*`.

A provider adapter is an internal SPI for the first implementation. External
adapter packs may use the existing plugin capability contract later; this ADR
does not introduce a new extension kind. A plugin must never register a public
`media.*` ID directly.

The self-media workbench consumes media primitives and domain capabilities.
It may display provider/model/voice choices and generation policy, but it does
not store credentials, implement vendor HTTP calls, or maintain a second model
registry. For audio workflows it calls `audio-production` / `audio-intelligence`
capabilities, which in turn use the Agent provider plane.

## Rejected alternatives

**Put every provider in the consuming extension.** Rejected because credentials,
rate limits, retries, model metadata, and artefact semantics would be duplicated
and because the plugin credential boundary is not currently sufficient for
cloud providers.

**Put complete TTS/BGM/mix workflows in the Agent.** Rejected because it creates
two owners for `audio-production` and leaks producer-domain policy into the
generic Agent base.

**Make every media operation asynchronous.** Rejected because image and TTS
providers may be synchronous or streaming; forcing a job adds unnecessary state
and latency.

**Introduce a `provider` extension type now.** Rejected because the current
extension system has no such kind. The first implementation can use an internal
adapter SPI and the existing plugin capability model.

## Consequences

- `ai-services.json` must gain `audio.speech` and `audio.transcribe` slots and
  migrate existing image/video bindings without breaking callers.
- The old `media.audio.speech` / `media.audio.transcribe` workbench aliases need
  a deliberate migration window.
- Provider errors must be normalised into stable categories with repair hints.
- Tests must prove that domain plugins cannot obtain plaintext credentials and
  that provider-specific fields do not enter Agent core state.
- A future external adapter mechanism must be evaluated separately; it is not
  part of this decision.
