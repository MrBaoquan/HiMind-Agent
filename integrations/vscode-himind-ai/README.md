# HiMind AI for VS Code

HiMind AI makes the models assigned to the current employee available through the stable VS Code Chat Participant API. Connect it from **AI Services / My Connections** in the HiMind Dashboard, then type `@himind` in VS Code Chat.

The Dashboard never sends the gateway key through the browser. HiMind Agent creates a 60-second, single-use enrollment code and writes only that code to a same-user handoff file in its profile data directory. The extension discovers both the legacy `%LOCALAPPDATA%\\HiMindAgent\\data` directory and profile directories, atomically claims and deletes the newest handoff, exchanges the code over loopback, and stores the returned key in VS Code SecretStorage. Extension URI enrollment remains available as a compatibility fallback.

Use `HiMind AI: Select Model` or `@himind /model` to choose from the model aliases assigned by the organization. Requests always use the original gateway alias.

The extension uses VS Code's `chatProvider` proposal to expose enrolled HiMind models in Copilot's global model picker. On first import, HiMind Agent adds this extension to the current VS Code product allowlist (with a timestamped backup); subsequent launches and window restarts do not need `--enable-proposed-api`.

The extension is declared as a local `ui` extension. Import is supported from a desktop VS Code window on the same computer as HiMind Agent; Remote SSH, WSL and Dev Container extension hosts are intentionally not used for the loopback enrollment exchange.

VS Code discovery is local-only and does not require a running VS Code window. Agent checks `HIMIND_VSCODE_CLI` first, then a real `code`/`code-insiders` command from `PATH`, running VS Code processes, Windows App Paths/uninstall records, and standard per-user/system/Scoop locations. A portable copy that is neither running nor registered cannot be inferred safely; set `HIMIND_VSCODE_CLI` to its `bin\\code.cmd` (or `code-insiders.cmd`) path for that case.

For local product verification, `npm run package:provider-preview` still builds an isolated preview VSIX. The preview scripts may use `--enable-proposed-api` for development hosts, but that flag is not part of the end-user import flow.

For local provider verification, run `npm run dev:provider`. This compiles a separate staged development manifest, declares `chatProvider` and the `HiMind` model-provider contribution only in that stage, and opens an Extension Development Host with `--enable-proposed-api himind.himind-ai`. The development host reuses the same extension ID and SecretStorage enrollment and opens VS Code's language-model management editor after it verifies the registry. Enable the desired models under **HiMind** before expecting them in Copilot's normal model picker. This command is not an end-user installation path and does not alter the stable VSIX manifest.

For release verification only, start the VS Code test window with `HIMIND_VSCODE_SMOKE_TEST=1`. The next Dashboard enrollment sends one short completion through the assigned default model and records only the model ID and success/failure in the `HiMind AI` output channel. Credentials and response content are never logged. Normal installations do not run this request.

## Development

```powershell
npm ci
npm test
npm run package
code --install-extension dist/himind-ai.vsix --force

# Development host with HiMind models in the Copilot model picker
npm run dev:provider

# Installable local preview for a fresh VS Code process started with the proposal flag
npm run package:provider-preview
npm run start:provider-preview
```
