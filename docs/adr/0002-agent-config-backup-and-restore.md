# ADR 0002: Agent state backup is a configuration layer, not a directory copy

Status: accepted (2026-09-29)

## Context

A HiMind Agent installation writes to `%LOCALAPPDATA%\HiMindAgent` (or
`profiles/<name>` for non-production profiles) and to several places outside
it. Measured on a working install, the home directory is about 3.9 GB, but
almost none of it is user data:

| Category | Examples | Size |
| --- | --- | --- |
| Configuration | `data/agent-state.json`, `data/extension-sources.json`, `approval-settings.json`, `skill-deployments.json`, `svn-connections.json` | ~1 MB |
| Content | `plugins/`, `skills/`, `*-drafts/`, `local-runs.sqlite3` | ~360 MB |
| Rebuildable | `versions/`, `runtimes/`, `staging/`, `profiles/`, `logs/` | ~3.5 GB |

Copying the directory is therefore wrong twice over: it drags gigabytes of
re-downloadable payload, and it still misses what actually breaks on a new
machine. Two of those gaps are hard requirements, not nice-to-haves:

1. **Credentials are sealed to this machine.** `agent-state.json`,
   `agent-user-authorization.json`, `svn-connections.json`, `ai-services.json`,
   `github/`, `connectors/` and `acp/` store secrets as
   `dpapi:v1:<base64>`. DPAPI derives its key from the Windows user profile, so
   a byte-for-byte copy restores a file that cannot be decrypted on any other
   machine or user account.
2. **The Agent writes outside its home.** MCP registration edits Claude
   Desktop, Cursor, VS Code, Codex, opencode and other client configs; skill
   deployment writes into `%USERPROFILE%\.codex\skills` and into project
   directories recorded in `skill-deployments.json`. A backup that only covers
   the home directory silently loses the parts the user can actually see
   working.

Separately, the existing diagnostics bundle
(`src/app/diagnostics.rs`) looks like a backup and is not one: it is
deliberately redacted, so it can be sent to support but never restored from.

## Decision

`backend: data backup` exports and restores **the configuration layer only**,
with credentials re-sealed to a user passphrase.

```
himind-agent-backup.json   manifest: format version, machine, categories, checksums, skips
payload/<relative path>    configuration files, relative to agent home
credentials.enc            optional, PBKDF2-HMAC-SHA256 + AES-256-GCM sealed secret map
checksums.sha256           per-entry digests, verified before restore
```

Rules that define the layer:

- **Include:** everything under `data/` and the `acp/`, `connectors/`,
  `github/`, `trusted-keys/`, `approval-owners/` and `extension-data/` trees,
  plus `*.json` at the home root. Their relative paths are preserved so restore
  is a copy, not a translation.
- **Exclude, and say so:** lock files, `*.bak*`, `*.tmp`, `*.cache`, `*.log`,
  `*-outbox/`, `extension-source-cache/`, `extension-transactions/`, update and
  draft directories, `versions/`, `runtimes/`, `staging/`, `profiles/`,
  `logs/`, `plugins/`, `skills/`, `manual-backups/` and `backups/`. Every
  skipped top-level entry is written into the manifest with a reason, and the
  export result reports the list, so "what is in the package" is never a guess.
- **Unclassified entries are skipped, not included.** The classifier is
  fail-safe: an entry matching no rule stays out of the package and shows up in
  the skip list. A test (`backup::tests::every_agent_home_storage_path_is_classified`)
  scans `src/` for `with_file_name("...")` and `agent_home().join("...")` and
  fails when a new storage location is neither included nor explicitly
  excluded.

### Credentials travel, but only through the passphrase

Export walks each payload file's JSON, replaces every string that DPAPI can
decrypt with a `backup:<n>` marker, and writes `{n: plaintext}` as
`credentials.enc`. Restore reverses it: decrypt the map with the passphrase,
then re-protect each value with DPAPI for the *current* user. A value that
fails to decrypt is left untouched and reported instead of being silently
rewritten or dropped.

The passphrase is the user's responsibility and cannot be recovered: losing it
means the credentials in the package are gone. The UI states this before the
export runs, not after.

### Device identity is opt-in

`data/agent-state.json` carries `agent_id`, the device credential and the
access token. Restoring it onto a second machine makes two devices present the
same identity. It is therefore excluded by default, and only included when the
user explicitly enables the advanced option for a same-machine reinstall.

### Restore never overwrites someone else's file

- A snapshot of the current configuration is written to
  `<home>/backups/auto-<timestamp>/` before anything is replaced.
- Only files that exist in `payload/` are written; an entry absent from the
  package is left as-is on disk. This is what makes "device identity not in the
  package" mean "keep this machine's identity".
- Client-side landing spots (MCP entries in third-party configs, deployed skills) are
  **not** overwritten by the package. The Agent re-pushes them from the
  restored configuration, reusing the existing registration and deployment
  paths, so a stale path in the package cannot corrupt a client config.
- Absolute paths inside restored files (`rendered_root`, `workspace_root`,
  Unity editor path) are resolved against the new machine and reported as
  missing when they do not exist, instead of failing silently later.

### Scope of the first release

| Phase | Content |
| --- | --- |
| P0 (this ADR) | Config layer, manifest, passphrase-sealed credentials, export, inspect, restore with auto snapshot and missing-path report |
| P1 | Content layer (`plugins/`, `skills/`, drafts), re-push of MCP registrations and skill deployments after restore, path re-binding UI |
| P2 | Scheduled automatic backup of the config layer, retention limit, off by default |

Out of scope by decision: `versions/` and `runtimes/` (re-downloadable),
Cloud sync (the package format is left upload-friendly, but no endpoint is
defined here), and third-party client config files (they belong to those
applications).

## Alternatives considered

- **Copy the whole home directory.** Rejected: ~3.9 GB, mostly re-downloadable,
  and it still restores credentials that cannot be decrypted on the target
  machine.
- **Share `agent_id` and the device credential across machines by default.**
  Rejected: two live devices presenting one identity is a support and security
  problem. Same-machine reinstall keeps an explicit opt-in instead.
- **Keep using the diagnostics bundle as the migration path.** Rejected: it is
  redacted on purpose. Making it restorable would mean making every support
  attachment a credential leak.
- **Store credentials in plaintext inside the package.** Rejected: a backup
  file copied to a share or attached to a ticket would hand over SVN, internal
  admin, AI service and workbench tokens at once.
- **Rely on the OS keychain instead of a passphrase.** Rejected as the only
  mechanism: the target machine has no access to the source keychain, which is
  the entire point of moving the package. A passphrase is the one secret a user
  can carry between machines.
