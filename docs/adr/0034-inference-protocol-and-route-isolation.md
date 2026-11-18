# Protocol compatibility and route-isolation resilience for the local inference gateway

Status: accepted (2026-11-18)

## Context

Codex resumed a long tool-heavy conversation through the local aggregation gateway. The final Responses request was rejected by the upstream Responses dialect with a generic HTTP 400. The request still contained a prior `input[]` item whose type was `reasoning`. Repeating the request after removing only that item succeeded while preserving messages, function calls, function outputs, tools, top-level reasoning settings and included reasoning data.

A second class of failure appears in Chat-compatible SSE dialects. Some streams end with a complete final chunk carrying a non-null `finish_reason` and close without sending `[DONE]`. Treating that as truncation breaks otherwise complete Responses-to-Chat sessions. Conversely, a null finish reason, unfinished SSE event or bare socket close must remain a transport failure.

The existing process-local route circuit breaker was keyed by endpoint URL and model. Two credential-backed services can share both while having independent quotas, authentication state and protocol behavior. A failure in one could cool down the other, and protocol differences were not represented in the key.

## Decision

1. Responses compatibility is a protocol transformation. After a Responses-dialect 4xx, the gateway removes only opaque `input[].type = "reasoning"` history items and retries before applying more destructive tool-history changes. It preserves top-level reasoning configuration, included reasoning data, ordinary messages and structured tool history.
2. Runtime capability observations may pre-apply the same protocol transformation only after repeated successful fallbacks. The feature name is `reasoning_history`; it is not a vendor-name branch.
3. A Chat SSE stream with a complete, dispatched chunk that contains at least one non-null `finish_reason` may terminate at EOF without `[DONE]`. Incomplete JSON, unfinished event framing or a missing/non-null terminal reason remains failure. The gateway still never synthesizes completion after an upstream failure and never replays after response bytes have been committed to the client.
4. Circuit-breaker identity is the credential-backed service, normalized endpoint, upstream model and wire protocol. A shared URL/model is insufficient identity. Health state remains process-local and non-persistent.
5. Automatic failover remains limited to routes sharing the selected upstream wire protocol. Direct passthrough also requires an equivalent Provider/model contract because it reuses the selected serializer. The Responses→Chat translator provides candidate-specific request construction, model rewriting and compatibility normalization, so it may fail over across services, accounts and upstream models before the first client byte. Mixed wire protocols remain a separate acceptance batch and must not receive another protocol's serialized body.
6. Every attempt has bounded retry/failover scope. A route is traversed once for failover; the single transient retry for a route occurs inside that route's attempt. There is no whole-batch retry and no replay after any response content reaches the client.

## Alternatives considered

- Vendor-specific branches (for example `if provider == ...`): rejected. The failure is a protocol-dialect capability and must be testable across compatible providers.
- Globally removing reasoning history from all Responses requests: rejected. Standard Responses supports it and it can carry provider-bound context.
- Treating any clean EOF as success: rejected. It would turn truncated model output into a successful response.
- Treating `[DONE]` as the only valid Chat terminator: rejected. It breaks compatible upstreams that provide an explicit final choice and close.
- Switching to an unrelated model after response bytes were sent: rejected because it can duplicate text or trigger duplicate tool execution.
- Keying health only by URL/model: rejected because separate credentials and protocols are independently recoverable/quota-bound.
- Sending a payload translated for the primary route to every backup: rejected. Translation and model rewriting are per candidate.
- Immediately adding mixed-wire-protocol failover in the same patch: rejected for this batch because it requires candidate-specific request construction and response conversion, plus separate tests, and an incompletely migrated implementation cannot safely ship.

## Consequences

- Long Codex conversations using compatible Responses dialects can continue after the first opaque-format rejection while keeping structured tool history.
- Complete Chat streams without `[DONE]` are accepted; broken streams still fail closed and are not recorded as successful.
- Health cooling is isolated across accounts and protocols sharing a URL/model.
- Equivalent direct routes remain available. Responses→Chat failover may cross providers, accounts and models when each candidate adapts successfully. Mixed-wire-protocol failover remains a separate batch rather than an accidental behavior.
- Every retry remains bounded. The gateway does not append a fake terminal event after an error.
