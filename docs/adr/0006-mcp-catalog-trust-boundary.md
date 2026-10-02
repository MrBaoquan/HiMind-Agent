# ADR 0006: Trust boundary, install target and ownership of catalog entries

Status: accepted (2026-09-29)

## Context

ADR 0001 fixes a single bridge: the Agent owns every downstream server, and a
third-party server is projected with `risk_level: mcp_downstream` and
`approval_required: true` because there is no trusted risk contract for code we
did not write. ADR 0002 later established that credentials are sealed with
DPAPI and travel only through a passphrase. Both hold.

What a public catalog adds is content from strangers, which raises three
questions that ADR 0001 did not have to answer: **who published this entry, where
does an installed server land, and where do its secrets live.**

The pieces that already exist and should be reused rather than paralleled:

- `trusted-keys/` under the Agent home, used by the launcher and updater
  (`src/launcher.rs:25`, `src/updater.rs:269`), currently holding
  `himind-production-2026.pem`.
- `ExtensionSourceVerification` with `requires_signature()`
  (`src/app/extension_source.rs:47`, `:59`), i.e. required/optional signature
  checking per source.
- The DPAPI channel for server `env` and `headers`
  (`src/app/mcp_settings.rs::protect_secrets` / `::reveal_secrets`, `dpapi:`
  prefix), which seals values on write and reveals them only inside the Agent
  process.
- The install file itself: `settings_path` =
  `state_path.with_file_name("himind-ai-mcp.json")` (`mcp_settings.rs:67`).

## Decision

**Trust is a property of the source, and it is visible.**

- Entries from an internal source must pass signature verification through the
  existing `requires_signature()` path and the existing `trusted-keys/` root.
- Entries from a public source are labelled **未验证来源**. They are not
  auto-enabled on install; the first enable is the user's explicit action.
- A source is a trust unit. The acknowledgement for unverified entries is
  remembered **per source, once**, not per entry and not per install. Repeated
  per-entry prompts train the user to click through, which is worse than one
  honest prompt.

**Secrets reuse the existing channel.**

A catalog entry declares its inputs (an API key env var, an authorization
header). The user supplies the values in the existing server form, and they are
sealed by `protect_secrets` exactly as a hand-added server would be. Nothing new
is written to disk, no value is stored in the catalog entry, and no plaintext
path is added. A catalog entry that cannot be satisfied without a secret and
without a place to enter it is listed as not installable.

**The install target does not change.**

Installing from the catalog appends a `McpServerConfig` to
`himind-ai-mcp.json` and triggers the existing reconnect push. There is no
second store, no second registration path, and no catalog-specific file. This
is the same file ADR 0001 depends on.

**"Already added" is a reverse lookup, not an index.**

The catalog keeps no install index and writes no marker. An entry counts as
installed when `himind-ai-mcp.json` already holds a matching `McpServerConfig`,
and the rule is the exact mirror of what `build_config` writes: a package entry
matches on runtime command plus package identifier (allowing a pinned
`identifier@version`), a remote entry matches on the static URL prefix up to the
first `{variable}` placeholder. Because it is symmetric, a server the user added
by hand is recognised too, and deleting the connection clears "已添加" with no
extra bookkeeping.

**Scanning, if enabled, is advice.**

An optional pre-install scan (e.g. `mcp-scan`, Apache-2.0) may be shown as
information for an unverified entry. It is never a gate, and its absence never
blocks an install.

**Ownership: catalog entries belong to HiMind AI 工具, not to 我的能力.**

An MCP catalog entry appears in **HiMind AI 工具** and nowhere else. It is not
listed in **我的能力**. Three reasons:

1. ADR 0001 already decided that downstream MCP servers are not capability
   registry entries. Listing them under 我的能力 would contradict that decision
   in the one place users can see it.
2. An MCP connection is shared across every client (DSH, Codex, Claude Code,
   opencode, ...). 我的能力 has no field that can express "installed once,
   consumed by many clients", because extension capabilities are per-client
   deployments.
3. HiMind AI 工具 is already titled as the place where the assistant gets
   local tools. A user who wants the assistant to reach an external service is
   already looking there.

The market may carry a *pointer* to that place ("想给分身接外部工具？去 HiMind AI
→ 工具"), but not a listing. This keeps discovery without duplicating the
inventory.

This conclusion changes only if MCP servers themselves become extensions —
distributed with versions, signatures and distribution constraints through the
extension system. Then a catalog entry is an extension and belongs in 我的能力.
Until that happens, it is a connection.

## Alternatives considered

- **Treat public source entries as trusted.** Rejected: tool poisoning and
  prompt injection through server metadata are the documented attack surface of
  exactly this feature. `approval_required` per call already exists; the entry
  level should not be weaker than the call level.
- **Give catalog entries their own store and loading path.** Rejected: it
  breaks the single bridge in ADR 0001, which is what keeps credentials out of
  runtime profiles and keeps governance to one door.
- **Store secrets declared by an entry into the entry or the runtime profile.**
  Rejected: plaintext on disk, and the runtime profile is readable by any
  process running as the user.
- **List catalog entries in 我的能力 as well.** Rejected: two inventories of
  the same thing, one of which cannot represent the shared-connection model.

## Consequences

- Every catalog entry is honest about who published it, and an install from an
  unverified source costs exactly one acknowledgement per source.
- Credentials stay inside the Agent; the runtime profile stays clean; nothing
  about the existing bridge, approval or audit changes.
- Cost: an extra confirmation step for public entries, plus whatever a scanning
  dependency pulls in if it is enabled.
- Risk: acknowledgement fatigue. Mitigated by remembering per source and only
  prompting on the first enable, and by keeping the label on the entry visible
  after install rather than only at install time.
