# Release binary acceptance runs against the development profile

Status: accepted (2026-11-03)

## Context

Extension changes ultimately ship inside the production Agent binary. Debug
builds are faster to iterate and debug, but they do not exercise the embedded
frontend, release linker layout, Authenticode artifact, WebView2 startup,
single-instance behavior, or the same capability registration path. Treating a
Debug pass as final acceptance leaves a whole class of release-only failures
uncovered.

Using the production profile with a Release artifact fixes the fidelity problem
but introduces a worse one: extension build, activation, sidecar, and rollback
tests can overwrite the user's installed Agent state. The test must be
production-shaped without becoming production-destructive.

A second environment trap surfaced during R157: binaries under the
Codex-managed workspace inherit a Low Mandatory Label. Starting such a binary
directly gives the Agent a Low integrity token, which cannot write the real
development profile even though the filesystem ACL appears to grant access.
The same signed binary copied to `%TEMP%` receives the normal Medium label and
can write the profile. The MCP companion must be copied beside it as well;
otherwise the UI reports that AI/MCP sessions cannot start.

## Decision

**Final extension and page acceptance uses the signed Release Agent artifact,
the real `development` profile, fixed port `18082`, and a loopback-only
WebView2 DevTools port `9311`. Debug remains a pre-check, not final evidence.**

The canonical launcher is
`HiMind-Agent/scripts/start-test-agent.ps1 -Channel Release`. It must:

1. Build the frontend and the Release `himind-agent` / `himind-agent-mcp` pair
   when `-Build` is requested.
2. Verify the source artifact's Authenticode signature and copy
   `himind-agent.exe` plus `himind-agent-mcp.exe` to a `%TEMP%` launch folder.
3. Prove the copied Agent has the same SHA-256 as the repository artifact and
   does not carry a Low Mandatory Label.
4. Start with `--profile development --local-app --local-port 18082`; the real
   profile remains
   `%LOCALAPPDATA%\HiMindAgent\profiles\development`.
5. Refuse to reuse a listener whose SHA differs from the requested channel, so
   a Debug binary can never masquerade as the Release acceptance instance.
6. Require health to report `profile=development`, `local_port=18082`, and a
   non-zero capability count before acceptance starts.
7. Keep stdout/stderr on real files rather than inherited pipes. A GUI Agent
   may outlive the launcher; writing a warning to a closed pipe must not abort a
   capability task.
8. Leave the verified instance running after acceptance so the user can inspect
   it.

The production profile is not used for write-oriented extension tests. A
read-only production smoke requires explicit authorization and must not click
activation, distribution, disconnection, or other state-changing controls.

## Consequences

- The repository Release artifact and the `%TEMP%` launch copy are expected to
  have the same hash; the launch copy is an environment adapter, not a second
  product artifact.
- Release acceptance now depends on the companion binary being available. A
  missing companion is a test-environment failure, not a reason to use an empty
  profile or fall back to Debug.
- The launch folder and WebView2 data stay outside the user profile's real
  WebView cache, but Agent data (plugins, drafts, activations, ledgers) still
  comes from the real `development` profile.
- Debug builds remain useful for unit tests and fast interaction work, but
  cannot close a page or packaging acceptance item.
- A Release pass must still check stderr and Agent event logs. Passing UI while
  the audit ledger repeatedly reports lock/readonly failures is a residual
  runtime issue, not a clean acceptance.

## Rejected alternatives

- **Use the Debug binary for final acceptance.** Faster, but misses release
  embedding, signing, integrity, and binary-layout failures.
- **Use the Release binary with the production profile.** Highest fidelity, but
  violates the non-destructive data boundary for extension development.
- **Start the workspace Release artifact directly.** The inherited Low label
  makes AppData writes fail and produces misleading readonly database errors.
- **Create an empty temporary profile.** It removes real workspace, plugin,
  activity, and activation facts, so it no longer verifies the user's actual
  development environment.
- **Copy only `himind-agent.exe`.** The UI starts, but MCP/AI companion checks
  fail and the extension page shows a false runtime error.