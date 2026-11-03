# ADR 0021: user data can move to another local disk, the program stays put

Status: accepted (2026-10-31)

## Context

A production install currently keeps program files and user data in the same
directory, `%LOCALAPPDATA%\HiMindAgent`. Measured on a working install that is
about 24 GB, of which 21 GB is extension development drafts, 918 MB is eight
retained program versions and 473 MB is the bundled runtime; the actual user
data (configuration, credentials, ledgers, media) is under 1 GB.

Two problems follow from that layout:

1. `%LOCALAPPDATA%` lives on the system drive. A machine whose C: is small has
   no way to put user data somewhere else; the only documented escape hatch is
   `HIMIND_AGENT_HOME`, which is a process-wide override rather than a user
   setting.
2. `HIMIND_AGENT_HOME` alone is not enough even today: the launcher always
   passes `--state <install root>\data\agent-state.json`, so the state file
   would stay on C: while everything else moved. The data root would split in
   two, which is worse than not moving at all.

## Decision

**Separate the install root from the data root, and let the user move the data
root to any local NTFS folder. The program never moves.**

- `agent_install_root()` (`%LOCALAPPDATA%\HiMindAgent`) keeps `versions/`,
  `runtimes/`, `resources/`, `previous/`, `staging/`, `trusted-keys/`,
  `profiles/`, the launcher, the updater and `active-version`. Updaters,
  protocol registration, autostart and shortcuts all keep working unchanged.
- `agent_home()` returns the data root: the explicit `HIMIND_AGENT_HOME` first,
  then `data-location.json` for the production profile, then the historical
  default. Development profiles keep living under `profiles/<name>` and are not
  affected by the pointer.
- `data-location.json` sits next to the launcher and records
  `{version, data_root, previous_root, updated_at}`. It is the only anchor that
  can find the data after a move, so it must stay in the install root.

### Migration runs at startup, never in a running process

The running process holds SQLite, logs and a WebView2 environment. Copying its
data live produces a mixed snapshot that "looks complete". Therefore:

1. The settings page validates the target and writes `data-migration.json`
   (queued), then asks the user to restart.
2. The next process, before any store opens, copies the data root with
   `robocopy`, verifies file count and byte count per top-level entry, rewrites
   absolute paths recorded in top-level `*.json` and `data/*.json`, and only
   then writes `data-location.json`.
3. Until that pointer lands, the old directory remains authoritative; a failure
   leaves it untouched and records `data-migration-result.json`.
4. The old directory is **not** deleted. Disk usage doubles during a migration
   and the user removes the old copy after checking the app.
5. Program entries are never copied. A target must be empty, local and NTFS
   (a non-NTFS volume is a warning because ACLs and integrity levels are lost).

### Old launch lines follow the pointer

AI client registrations and shortcuts keep the old `--state` path. The launcher
therefore overrides `--state` whenever a pointer exists, and `Options::from_env`
remaps any `--state` that falls inside `previous_root`. Cross-profile
protection (`explicit_state_crosses_profiles`) now knows both the default and
the configured production root.

## Consequences

- The user-facing setting is "数据位置", not "安装位置". Moving the install is
  deliberately out of scope: it would invalidate the updater's health check,
  the `InstallLocation` registry value and every registered launch line.
- `repair-storage.ps1` follows the pointer, otherwise post-migration integrity
  repair would target an empty default directory.
- The data root must be online to start. A missing pointer target is not
  silently replaced by the default path: starting with an empty root would look
  like data loss and start writing a second copy.
- Peak disk usage during migration is the size of the data root plus the same
  amount at the target; the planner refuses targets without 10 % headroom.
- WebView2 user data stays in `%LOCALAPPDATA%\com.himind.agent` (about 110 MB).
  It is browser state, not Agent data, and moving it would only risk login
  state for no meaningful space gain.

## Rejected alternatives

- **Move the whole `%LOCALAPPDATA%\HiMindAgent`.** Requires rewriting shortcuts,
  HKCU `Run`, protocol registration and the updater's health expectations;
  breaks every installed copy that must keep its version directory.
- **Directory junction.** `mklink /J` works with zero code but needs
  administrator rights, fails when the target disk is offline, and hides the
  real location from the user who asked for it.
- **Copy while running.** See above: mixed snapshot, SQLite WAL, half-written
  logs.
- **Delete the old directory automatically.** A migration that silently deletes
  the only verified copy on a failed verification is unrecoverable.
