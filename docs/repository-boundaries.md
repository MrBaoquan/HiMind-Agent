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
must explicitly pass `-DefaultMode connected`. Independent GitHub releases use
`./scripts/publish-github-release.ps1`, which always produces an `independent`
installer and does not depend on GitHub Actions.

The local publisher requires the update signing private key, public key and key
ID by default. The public key is embedded in the Agent and installed into the
trusted key directory; the private key is used only by the local signing step.
Windows Authenticode can be enabled independently through the existing
certificate parameters and environment variables.
