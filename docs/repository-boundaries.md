# Repository Boundaries

`HiMind-Agent` is an independently buildable and runnable Windows client.
Dashboard is an optional business enhancement, not a startup or local-capability
dependency.

## Installation modes

| Distribution source | First-install default | Build command |
| --- | --- | --- |
| GitHub Release | `independent` | `./scripts/build-installer.ps1` |
| Dashboard distribution | `connected` | `./scripts/build-installer.ps1 -DefaultMode connected` |

The default is a signed installer build property, not something inferred from
the download URL. The installer only creates `data/agent-preferences.json` when
that file does not exist, so upgrades preserve the user's selected mode.

## Dashboard integration

Dashboard should consume this repository as a submodule at `himind-agent/` and
pin an reviewed commit. Dashboard owns the server-side implementation of the
business integration protocol. This repository owns the protocol schema,
client validation, trusted provider adapter, local gateway, UI, runtime,
installer, and GitHub release pipeline.

Dashboard release automation may call this repository's public scripts, but it
must explicitly pass `-DefaultMode connected`. GitHub release automation does
not pass that option and therefore produces `independent` installers.

Official release workflows require `HIMIND_WINDOWS_CODE_SIGNING_PFX_B64` and
`HIMIND_WINDOWS_CODE_SIGNING_PFX_PASSWORD`. The workflow signs all four
portable binaries and the NSIS installer, and fails instead of publishing an
unsigned fallback when signing material is unavailable.
