# The profile owns the data root, and a build output never opens the installed one

Status: accepted (2026-09-30)

## Context

One machine legitimately holds more than one Agent:

- the Agent the user installed;
- a build output a developer runs out of `target/release`;
- and the MCP companions and DSH sessions both of them spawn.

Until now every one of those processes derived its paths from the same
constants: `agent_home()` was `%LOCALAPPDATA%\HiMindAgent`, the WebView2 user
data directory was `%LOCALAPPDATA%\com.himind.agent`, and the local port without
`--local-port` was 18181 for anyone. `HIMIND_AGENT_HOME` and
`HIMIND_AGENT_PROFILE` existed, but only as test isolation; nothing made a build
output use them.

The failure that produced this record: a developer ran
`target\release\himind-agent.exe` with neither `--profile` nor `--state`. It
opened the installed Agent's data root, found no identity (a fresh process
always starts without one) and ran `store::workbenches::materialize` for a
connection with an empty identity, which clears the legacy files —
`agent-state.json`, `agent-state.device-id` and `agent-user-authorization.json`
together with their `.bak`. Recovering from that is a re-enrollment, i.e. an
outage of the installed Agent.

Two properties make this more likely than it looks:

- **A registration line outlives the build that wrote it.** `.claude.json`,
  `claude_desktop_config.json`, `.cursor/mcp.json`, `opencode.json` and the DSH
  overlay all store a complete `--state <data root>\agent-state.json`. An entry
  recorded months ago still names the production state file.
- **`cfg!(debug_assertions)` cannot tell the two apart.** The dangerous binary
  was a `--release` build.

## Decision

Let the binary decide its own data root, fail closed on the one combination
that is never intentional, and treat every stored launch line as a hint the
profile may overrule.

1. **Resolution order** (`store::paths::resolve_profile`): `--profile <name>`,
   then `HIMIND_AGENT_PROFILE`, then inference from the executable. A binary
   inside an installation (`install_layout::executable_is_installed`) runs
   `production`; anything else runs `development`. The test requires all three
   of: the executable lives under `<root>\versions\<version>\` or
   `<root>\current\`; the root carries `himind-agent-launcher.exe` and
   `himind-agent-updater.exe`; and the root carries the `active-version`
   pointer the launcher owns (or, for a pre-pointer install, a
   `current\himind-agent.exe`). `HIMIND_AGENT_HOME` still replaces the whole
   root.
2. **A flat output folder is never an installation.** `cargo build --release`
   copies `himind-agent-launcher.exe` and `himind-agent-updater.exe` next to
   `himind-agent.exe` in `target\release\`, so "the launcher and the updater sit
   next to it" cannot be the whole test: it read a build as the installed Agent
   and let a development process keep the `production` profile. The versioned
   path and the `active-version` pointer are what a build tree does not have.
3. **Non-production profiles are nested** under
   `%LOCALAPPDATA%\HiMindAgent\profiles\<name>`. `production` keeps the
   historical root, so an upgrade never moves installed data.
4. **A build output may not run `production`.** Without an explicit
   `--profile production` / `HIMIND_AGENT_PROFILE=production` **and** without
   `HIMIND_AGENT_HOME`, startup stops with exit code 2 and a window naming the
   two intended ways to continue. Failing to start costs one launch; the old
   behaviour cost the installed Agent's identity.
5. **`--state` may not cross profiles**
   (`store::paths::explicit_state_crosses_profiles`). A non-production process
   ignores an explicit `--state` that lands inside the installed root, keeps
   the profile-derived path and says so on stderr. `HIMIND_AGENT_HOME` disables
   the guard, because pointing a foreign binary at a chosen root is exactly
   what that variable is for.
6. **Everything process-scoped follows the profile**: the WebView2 data
   directory (`com.himind.agent` for production, `<profile home>/ebwebview`
   otherwise, split per port under `HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE=1`),
   the default local port (18181 for the installed Agent, 18082 otherwise), and
   the restart arguments the Agent writes for itself (`--profile <name>` is
   re-added by `app::system`, so a restarted process cannot drift off the
   profile it was started with).
7. **Registrations record the profile.** Every AI client the Agent registers
   itself into carries `HIMIND_AGENT_PROFILE` in the launch environment, so a
   companion started from a stored registration line lands on the profile that
   wrote it instead of on the default.
8. **Identity ownership is checked, not assumed.** `workbenches.json` is
   read-modify-written under a cross-process file lock, and `capture_into`
   refuses to fold a disk identity into a connection when another connection
   already claims that `agent_id`. `sync_active` then materialises the stored
   identity instead of adopting the foreign one.

## Consequences

- The installed Agent and a development Agent run side by side: two data roots,
  two WebView2 profiles, two ports (18181 / 18082).
- A development launch now fails loudly where it used to succeed quietly
  against production. That is the intended trade; `--profile` and
  `HIMIND_AGENT_HOME` keep the deliberate case available.
- A stored registration that names a build output together with the production
  data root stops working. The DSH session reports the bridge as unavailable
  (`failOnStartupError: false`) rather than corrupting anything, and the fix is
  to let the Agent rewrite the registration — not to relax the guard.
- `HIMIND_AGENT_PROFILE` becomes a selector rather than a wish: `production`
  from a build output is an error, not an instruction.
- `materialize` still clears the legacy files when the active connection has an
  empty identity, so that remains a reachable state — deliberately, through
  that connection's own switch, and no longer by accident from a foreign
  process.

## Alternatives rejected

- **One Agent, no isolation, the user picks the workbench.** ADR 0008 already
  made the workbench a stored property and that stays true; the two are
  orthogonal. The outage happened *before* any workbench was consulted: a
  process cleared identity files at the filesystem level, in a root it had no
  business opening.
- **Detect a development run from `cfg!(debug_assertions)`.** The dangerous
  binary was a release build: not a debug build, and not an installation
  either.
- **Ask the developer to remember `--profile`.** A convention is not a guard,
  and this incident is the evidence.
- **Back up the identity files before every start.** A backup does not make
  clear-on-empty correct; it shortens the outage while leaving a foreign
  process writing into the installed root.
- **One `agent_home` plus a lock so only one process may run.** Losing "a build
  next to the installed Agent" is a real cost for the developer who needs it,
  and it does nothing about the stale-registration path.
- **Remove `--state` entirely.** Packaging and automation need it; a narrow
  guard costs less than deleting the flag.
