# Runtime binaries are pinned and bundled with the Agent

Status: accepted (2026-10-26)

## Context

DSH is evolving quickly and HiMind generates profile patches, expert presets,
tool rows and capability checks against a specific Runtime release. The Agent
used to resolve Runtime binaries from `agent_home()/runtimes`, which meant every
profile owned a separate installation. A production update could therefore move
to 0.2.1 while the development profile remained on 0.1.5 indefinitely, even
though that old Runtime was "ready" and compatible enough to pass the health
check. New Agent code then ran against an untested Runtime.

The Agent already had two useful pieces: installers can carry a signed Runtime
ZIP under `resources/runtime`, and the Runtime distribution layer verifies size,
SHA-256, RSA-PSS and package contents before installing. The missing invariant
was that the *version* of the installed Runtime was never compared with the
version embedded into the current Agent build.

## Decision

- Runtime binaries are machine-wide, not profile-wide. Every profile resolves
  executables and `state.json` from
  `%LOCALAPPDATA%\HiMindAgent\runtimes\deepseek-harness`.
- Mutable DSH data (`homes`, run homes, profile snapshots and sessions) remains
  under `agent_home()/runtimes/deepseek-harness`, so profiles stay isolated.
- `resources/runtime/himind-runtime-release.json` is tracked in the Agent repo
  and compiled into the binary. Its `version` and `sha256` are the expected
  Runtime identity for that Agent build.
- Build scripts require a signed Runtime release directory matching that
  manifest. They stage the manifest, ZIP and signature next to the built Agent
  and package them into portable and installer artifacts.
- Startup checks exact version equality, not only `status=ready`. A mismatch
  installs from the bundled signed package before AI sessions can launch.
- The installer must carry the Runtime. Updating the Agent therefore updates the
  Runtime as one transaction, while keeping profile data separate. The updater
  keeps the previous Runtime bundle until the new Agent passes its launch
  health check and restores it if the Agent update rolls back.
- Runtime upgrades remain build-time decisions. "Latest" means latest at Agent
  build/release time, not a floating online lookup during user startup.

## Rejected alternatives

- **Keep Runtime per profile and rely on compatibility ranges.** Rejected: the
  0.1.5 / 0.2.x expert preset breakage proved that a runnable old Runtime can
  still be incompatible with newly generated Agent configuration.
- **Always fetch the latest Runtime at startup.** Rejected: builds stop being
  reproducible, offline startup becomes fragile, and a server-side release can
  change behavior without an Agent build.
- **Embed the ZIP bytes directly in the EXE.** Rejected: it adds roughly 172 MB
  to the executable and makes every binary update carry the full Runtime twice.
  The installer remains the distribution unit that carries the ZIP.
- **Let each build discover any local Runtime.** Rejected: a developer machine
  can contain multiple historical versions. The tracked manifest and signed
  package must match exactly.

## Consequences

- Development, portable and installed Agent builds must provide a matching
  signed Runtime release. Builds fail closed when it is missing.
- Runtime first-use no longer depends on a separate install step or network.
- Version alignment can add one extraction pass after an Agent upgrade, but
  healthy matching installs remain a cheap state read.
- Testing a deliberately old Runtime now requires an explicit override; normal
  development profiles cannot silently keep stale Runtime binaries.