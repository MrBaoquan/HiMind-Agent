# AI capabilities have explicit ownership and provider landing zones

Status: accepted (2026-10-24)

## Context

HiMind Agent already routes text models through OpenAI Chat, OpenAI Responses
and Anthropic Messages, and the media provider plane already owns image,
video, speech and transcription bindings. Domain extensions own the workflows
that consume those primitives:

- `media-workbench` owns projects, shots, timeline assembly and rendering;
- `audio-production` owns beat-level speech, voice rights, BGM, mixing and
  audio manifests;
- `audio-intelligence` owns alignment, subtitles and audio QA;
- `media-visual` owns reference packs and deterministic visual QA.

The provider landscape has no single cross-modal protocol. Text has several
broad compatibility families, image and audio have de facto OpenAI-compatible
shapes, and video providers mostly expose vendor-specific asynchronous jobs.
Adding every vendor to the Agent would make the core a media application;
putting provider calls in plugins would duplicate credentials, limits, retries,
pricing and error semantics across extensions.

The immediate integration case is Fish Audio. It exposes REST TTS and ASR with
Bearer authentication, an HTTP `model` header, `reference_id` for voices and
vendor-specific timestamp/diarization fields. That request shape is not the
OpenAI audio protocol, so it requires its own adapter rather than another
"OpenAI-compatible" flag.

## Decision

AI capability ownership is split into six layers.

### 1. Agent provider plane

The Agent owns shared invocation and governance:

- provider, adapter and model registration;
- DPAPI or host-managed OAuth credentials;
- capability bindings, execution mode, model features and limits;
- health, retry, fallback, usage, cost and provider request identifiers;
- normalised errors and artefacts;
- `media.job.*` for genuinely asynchronous work;
- capability exposure through the existing gateway.

The Agent does not own creative workflow state.

### 2. Control plane

The workbench/control plane owns the authoritative provider and model catalog,
entitlements, pricing snapshots, organisation policy, managed credentials,
audit and distribution. The Agent caches the catalog for offline use and must
not maintain a second hand-written vendor list.

### 3. Domain extensions

Extensions own producer-facing decisions and workflows. They call stable
Agent capability IDs rather than vendor HTTP APIs. They must not store cloud
API keys or register duplicate public `media.*` capabilities.

### 4. Provider adapters

Adapters are an internal Agent SPI. They translate the common envelope into a
vendor request and normalise the result. An adapter does not choose shots,
voices, licences, BGM or output quality policy.

### 5. Assets and market

Models, voices, styles, prompts, reference packs, licences, SHA-256 values and
commercial-use flags belong in asset packs or market records. A provider model
ID is bindings metadata; a licensed voice or style is an asset.

### 6. MCP and connectors

MCP is for optional tools and account-scoped integrations. It is not the core
provider invocation plane. A vendor MCP server must not be used to bypass the
Agent's credential, billing, error and artefact normalisation boundaries.

## Capability IDs

Existing stable primitive IDs remain unchanged:

- `media.image.generate`
- `media.image.edit`
- `media.video.generate`
- `media.audio.speech`
- `media.audio.transcribe`

The following are future IDs and are not implemented by this decision:

- `media.audio.voice.clone`
- `media.audio.voice.design`
- `media.audio.music`
- `media.audio.sfx`
- `media.video.understand`
- `media.video.extend`
- `media.video.lipsync`

A provider is a binding under a capability, never a new public capability ID.

## Capability landing matrix

| Capability | Agent | Extension / control plane |
| --- | --- | --- |
| Text and tool calling | provider, routing, usage | prompts and workflows |
| Vision understanding | provider and response normalisation | review and domain rules |
| Embedding / rerank | provider, cache, usage | knowledge-base policy |
| Image generate/edit | provider, credentials, artefacts | shots, references, style, QA |
| Video generate | provider, async jobs, artefacts | shot choice, fallback, timeline |
| Speech synthesis | provider, audio artefact | beat production, voice rights, mixing |
| Transcription | provider, timestamps, segments | captions, alignment, audio QA |
| Voice cloning | future provider primitive | consent, licence and role binding |
| Music / SFX | future provider primitive | audio-production orchestration |
| Lip sync | future async provider primitive | media-workbench / media-visual workflow |
| Deterministic media QA | not Agent-owned | `media-visual` and audio QA extensions |
| Rendering and Story Studio | not Agent-owned | workbench, workflows and plugins |

The Agent's provider catalog must expose, per model, at least:

```text
capability, adapter, execution_modes, features, limits, health, pricing/usage
```

The catalog must not contain business fields such as `beat`, `voice_license`,
`mix`, `storyboard` or `shot`.

## Vendor strategy

- Direct providers for primary text and media paths.
- Aggregators for long-tail text models.
- Direct REST adapters for asynchronous media, voice and video because their
  jobs, credentials and artefacts need explicit governance.
- A second provider is required for every product-critical capability before
  claiming production resilience.
- Fish Audio is the first new voice provider, through `fish-audio-tts` and
  `fish-audio-asr`, not through its MCP server.

## Rejected alternatives

- **Put every AI capability in the Agent.** It makes the core own creative
  policy, licenses, timeline editing and renderer-specific behaviour.
- **Put provider calls in every extension.** It duplicates keys, quota, retry,
  pricing and error handling and defeats the provider plane.
- **Pretend all vendors share one protocol.** OpenAI compatibility is useful but
  does not cover video jobs, Fish TTS, Gemini native calls or vendor-specific
  voice and timestamp fields.
- **Use MCP as the core provider plane.** MCP is a tool integration contract,
  not a uniform billing, health or media-job contract.
- **Add all vendors at once.** The first step is to make adapter truth and model
  metadata correct; vendor breadth follows verified provider slices.

## Consequences

- New providers are implemented as adapters and catalog bindings, not new
  extension kinds.
- Domain extensions can use multiple providers without learning vendor APIs.
- Model capabilities, limits and prices become data rather than UI guesses.
- Fish Audio TTS/ASR can ship without moving voice licensing or subtitle policy
  into the Agent.
- External adapter packs and additional vendors remain future work.
