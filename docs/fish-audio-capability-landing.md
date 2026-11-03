# Fish Audio capability landing and MCP boundary

Status: design proposal, 2026-11-02

## Decision summary

HiMind uses stable capability contracts as the shared boundary. Vendor
protocols stay behind adapters. Producer workflows stay in domain extensions.

For Fish Audio:

- REST TTS and ASR remain the primary automated voice path.
- Fish MCP is an optional downstream tool surface, not a provider replacement.
- A Fish adapter is selected under a HiMind capability; a vendor tool name is
  never promoted into a public HiMind capability ID.
- OAuth, credits, remote links, and approval semantics are prerequisites for
  any product-managed Fish MCP integration.

## Is there an industry standard?

There is no cross-modal industry standard for AI providers. There are only
several de facto protocol families.

| Area | De facto compatibility | Remaining vendor differences |
| --- | --- | --- |
| Chat | OpenAI Chat/Responses, Anthropic Messages | tool-call formats, streaming events, stop reasons |
| Image generate/edit | OpenAI Images-compatible shape | masks, reference-image rules, response encoding |
| Video generate | Mostly no common shape | async job lifecycle, polling, result schema |
| Speech synthesis | OpenAI audio-speech shape | reference voices, prosody, formats, streaming |
| Speech transcription | OpenAI audio-transcriptions shape | timestamps, diarization, events, long-audio limits |
| Voice cloning | No common shape | sample requirements, consent, verification, licences |
| Realtime voice | No common shape | WebSocket events, turn detection, interruption |
| MCP | Common transport and tool-discovery envelope | tool names, arguments, billing, result semantics |

Consequently, an OpenAI-compatible label is a useful transport hint, not a
capability contract. Fish TTS/ASR needs a dedicated adapter because its request
shape (`model` header, `reference_id`, `prosody`, structured ASR fields) is not
the OpenAI audio protocol.

## Capability ownership matrix

The decisive questions are: Is this shared by multiple clients or extensions?
Does it require credentials, billing, audit, retries, or provider routing? Is
it one provider invocation rather than a production workflow?

| Concern | Agent core | Domain extension | Fish MCP |
| --- | --- | --- | --- |
| Provider/model registration | Own | Consume | Not a substitute |
| Credential storage and refresh | Own | Never receive plaintext | Requires OAuth handling |
| Billing confirmation and usage records | Own | Declare producer intent | Credits are provider-side |
| Retry, fallback, error normalization | Own | Consume normalized result | Provider-defined only |
| `media.audio.speech` invocation | Own | Call it | Not same route |
| `media.audio.transcribe` invocation | Own | Call it | Not same route |
| Voice search/list | Define contract | Present choices | May implement temporarily |
| Voice clone/design | Future stable contract | Consent and licence policy | May implement temporarily |
| Beat splitting and narration fitting | Not owner | Own | No |
| Voice licence and commercial-use policy | Not owner | Own | No |
| BGM, mixing, loudness QA | Not owner | Own | No |
| Storyboard, shots, timeline, export | Not owner | Own | No |
| Fish image/video/Story Studio | Not core by default | Optional dedicated extension | Optional tool surface |

### Why provider invocation belongs in the Agent

- Cloud credentials are shared and protected there.
- Billing, health, retries, and errors need one governance boundary.
- Multiple workflows need the same primitive without duplicating vendor logic.
- Provider fallback must happen only within the same capability.
- Normalized artefacts must use local paths, MIME, size, and hashes.

### Why producer workflows stay in extensions

The Agent must not own beats, voice rights, mix policy, subtitle alignment,
shot selection, or export. Those are creative and licensing decisions, not
provider primitives.

## Current implementation status

| Fish capability | Current status | Boundary |
| --- | --- | --- |
| REST text-to-speech | Implemented (`fish-audio-tts`) | `media.audio.speech` |
| REST speech-to-text | Implemented (`fish-audio-asr`) | `media.audio.transcribe` |
| Model selection via header | Implemented | Adapter detail |
| Voice/reference ID | Implemented | `voice` maps to `reference_id` |
| Prosody speed | Implemented | `speed` maps to `prosody.speed` |
| Advanced TTS options | Pass-through | `provider_options` |
| Timestamps/diarization options | Pass-through | `provider_options` |
| Voice clone creation/management | Not implemented | Future stable voice capability |
| Voice design | Not implemented | Future stable voice capability |
| Streaming TTS/ASR | Not implemented | Separate execution mode |
| Inline reference audio cloning | Not implemented | Requires upload/consent contract |
| Story Studio, image/video tools | Not in Agent core | MCP or dedicated extension |

The speed mapping was corrected in this task: Fish accepts speed under
`prosody`, not as a top-level request field.

## REST versus MCP

| Dimension | Fish REST | Fish MCP |
| --- | --- | --- |
| Endpoint | `/v1/tts`, `/v1/asr`, model APIs | `/mcp` Streamable HTTP |
| Authentication | Bearer developer API key | OAuth account/team authorization |
| Billing | Developer API account | Fish account credits, web-app billing |
| Tool surface | Explicit documented API | Provider-discovered MCP tools |
| Stability | Versioned REST schema | Tool list may change by release |
| Result | Binary audio or JSON | Often permanent links plus tool-specific data |
| Local artifact | Agent writes and hashes file | Requires download/normalization step |
| Retry semantics | Agent-controlled | MCP transport retry only |
| Approval | Agent risk and billing policy | Every downstream tool is manual by default |
| Capability routing | Can participate in stable routes | Cannot directly join stable media routes |
| Credential lifecycle | Existing Agent AI-service store | OAuth refresh not currently implemented |

## Recommended landing

### Phase 1: Keep REST as the automated path

- Preserve `fish-audio-tts` and `fish-audio-asr`.
- Keep local file output, MIME, size, and SHA-256 guarantees.
- Keep provider options behind the common execution envelope.
- Do not make automated workflows depend on Fish credits or permanent links.

### Phase 2: Add voice primitives as stable capabilities

Define cross-vendor contracts before exposing Fish-specific operations:

- `media.audio.voice.list`
- `media.audio.voice.clone`
- `media.audio.voice.design`

Fish remains an adapter under those contracts. Consent, rights, and commercial
licensing stay in the consuming extension or control plane.

### Phase 3: Offer Fish MCP as an optional downstream tool surface

Prerequisites:

- MCP OAuth browser flow, DPAPI-sealed tokens, refresh, revocation, and team
  selection.
- Explicit credits/cost notice before billable tools.
- Tool results normalized or downloaded before entering media workflows.
- Tools exposed as `mcp.fish-audio.*`, with manual approval by default.
- No claim that MCP tools participate in `media.audio.speech` routing.

This surface is suitable for voice exploration, one-off cloning, Story Studio,
and Fish image/video features. It is not suitable as the default automated
voice provider.

## Rejected alternatives

- **Treat all OpenAI-compatible endpoints as one standard.** Rejected because
  authentication, fields, errors, output, and billing differ.
- **Put all Fish features into Agent core.** Rejected because Story Studio,
  images, video, and creative policy are not shared provider primitives.
- **Let every extension hold Fish credentials.** Rejected because credentials,
  retries, and billing governance would be duplicated.
- **Replace REST with Fish MCP.** Rejected because MCP uses account credits,
  links, a broader unstable tool set, and OAuth not yet supported by the Agent.
- **Expose Fish tool names as public HiMind capabilities.** Rejected because it
  couples workflows to one vendor and its release cadence.
- **Automatically route to a different vendor.** Rejected because billing,
  voice identity, licence, and output semantics are not interchangeable.

## Acceptance checks for future implementation

1. A public capability ID remains vendor-neutral.
2. Fish-specific fields stay in adapter code or `provider_options`.
3. Credentials never reach extension processes in plaintext.
4. Billable calls require the existing cost confirmation policy.
5. Artifacts expose local path, MIME, size, and hash where available.
6. MCP OAuth tokens have protected storage, refresh, revocation, and audit.
7. Domain workflows continue to own voice rights, beats, mixing, and export.
8. Fish MCP remains independently disableable without breaking REST voice routes.
