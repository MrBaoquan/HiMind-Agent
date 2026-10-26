# AI services and capability routes are separate settings surfaces

Status: accepted (2026-10-25)

## Context

The AI connection page currently combines provider inventory, model
definitions, capability defaults, tool distribution and balance checks in one
screen and one service modal. The result works for four services, but it hides
several different objects behind the same words:

- a provider connection (Base URL, protocol and credential);
- the models available through that connection;
- the capability a model can serve;
- the default route for a capability;
- the tools that received a model source;
- account balance and usage diagnostics.

The combination creates concrete failures. A video-only Seedance service is
forced to store its video model in the global chat-model field. Automatic media
selection does not reveal which service it will resolve to. "Not distributed"
is displayed next to service actions as if it were a health state. Media
capabilities are only visible after opening the edit modal.

The stable AI settings tabs are `mcp`, `services` and `acp`. The workbench and
AI error copy already deep-link to these identifiers, so their meaning cannot be
replaced without a compatibility layer.

## Decision

AI settings use four task-oriented tabs:

1. `mcp` — tool connection, model distribution and execution wiring;
2. `services` — AI service connections and their models;
3. `routes` — capability defaults and automatic-resolution results;
4. `acp` — execution runtimes.

The display labels become `工具接入`, `AI 服务`, `能力路由` and `运行环境`.
The `services` and `acp` identifiers remain stable. `routes` is additive.

### Service inventory

The service inventory answers only:

- whether the connection has credentials and is healthy;
- which capabilities the service supports;
- whether it is used by a route;
- how many tools it has been distributed to.

The workbench-managed service is a separate group or connection banner. It is
not counted as a local service in the local inventory.

Services remain a dense list. Each row shows capability chips, connection
health, route usage and tool-distribution status as independent facts. A row
must never use "not distributed" to imply that the provider is unavailable.

### Service editor

Creating or editing a service is a three-step form:

1. **Connection** — preset, name, Base URL, protocol and credential;
2. **Capabilities and models** — select chat, image generation, image edit,
   video generation, speech synthesis and transcription, then configure only
   the selected capability bindings;
3. **Advanced and usage** — service ID, balance profile and other advanced
   settings.

A chat model is required only when the chat capability is selected. A
media-only service may be saved with an empty global chat model when at least
one media capability has a model. The persistent `model` field remains for wire
compatibility, but it is no longer a universal required field.

### Capability routes

The route view renders one row per capability. Each row contains:

- the selected service or `automatic`;
- the resolved service and model for automatic mode;
- the execution mode;
- a status of configured, ambiguous, or missing.

Automatic resolution is explained in the UI: a unique candidate is shown by
name; multiple candidates require an explicit choice; no candidates show a
repair action. Routes affect HiMind's own calls, not tool distribution.

### State vocabulary

The UI separates four state families:

- connection: configured, untested, healthy, authentication failure, unreachable;
- capability: configured or not configured;
- route: default, not selected, ambiguous, missing;
- distribution: distributed to N tools or not distributed.

Colors alone do not carry these distinctions. Every state has a text label.

## Compatibility

- `mcp`, `services` and `acp` tab identifiers and existing deep links remain.
- Existing `ai-services.json` records are unchanged. A service with a legacy
  chat model continues to support chat.
- A service with no chat model is excluded from chat routing and tool model
  distribution, but remains available to media routes.
- Old error copy that names `模型来源` is updated together with the tab label
  so the destination still exists under a predictable name.
- Presets may infer known capabilities and models, but users can remove any
  inferred capability before saving.

## Rejected alternatives

- **Keep routing inside the service page.** It preserves the current cognitive
  load and makes the source list grow without a stable place for automatic
  route diagnostics.
- **Use a card wall for services.** It reduces information density and makes
  protocol, health and distribution harder to compare.
- **Keep a global required model.** It forces media-only services to persist a
  misleading chat model and prevents honest capability ownership.
- **Show only the selected state and hide automatic resolution.** It moves a
  deterministic configuration failure to runtime, where the repair is harder.
- **Replace the stable tab IDs immediately.** Existing scripts, deep links and
  error repair text depend on them.

## Consequences

- Service editing becomes longer by steps but shorter per step.
- The route view becomes the single source of truth for defaults.
- Tool distribution can filter to chat-capable services without guessing.
- Tests must cover the new `routes` tab, media-only saves and automatic route
  diagnostics.
- The UI gains one more tab in exchange for removing a long mixed section from
  the service page.
