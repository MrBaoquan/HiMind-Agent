# Extension brand icons are package assets with local overrides

Status: accepted (2026-10-26)

## Context

Plugins, skills, workflows, experts and instruction packs all appear in the same
market and capability surfaces. A type glyph tells users *what kind* of
extension they are looking at, but it cannot help them recognize a concrete
product across versions and sources. Allowing arbitrary remote image URLs would
make market rows depend on third-party availability and would expose the
webview to untrusted content.

Plugin views already use `views[].icon` as a semantic action/navigation key.
Overloading that field for brand artwork would break host-side view discovery.

## Decision

- The five extension manifests accept one optional top-level `icon` field. Its
  value is a package-local relative path to a PNG or WebP file.
- The effective icon order is: local user override, author icon, view semantic
  icon (where applicable), then the extension type glyph.
- Author artwork is validated before caching: maximum 256 KB, 64–1024 px,
  decoded PNG/WebP, package-contained path, and no remote URL. A damaged author
  icon falls back without preventing the extension from loading; an unsafe path
  is rejected at manifest validation.
- Release publication uploads the icon as a SHA-256-addressed sidecar and records
  `file_name`, `size_bytes`, `sha256`, and `content_type` in the release
  manifest. Market catalogs point to the Release asset of the same origin.
- Remote catalog icons are downloaded only when their URL shares the artifact
  origin and their bytes match the declared SHA-256. The webview receives the
  Tauri-mapped opaque URL `http://extension-icon.localhost/<sha>.<ext>`, never
  a filesystem path.
- User overrides are keyed by `kind:id`, survive extension upgrades, and are
  removed when the extension is uninstalled. They never enter a release package.
- The type tabs and default glyph remain type-based; custom artwork does not
  replace type navigation.

## Rejected alternatives

- **Accept arbitrary HTTPS image URLs.** Rejected: availability, privacy, mixed
  content, and cache poisoning cannot be bounded by the Agent.
- **Support SVG in v1.** Rejected: script-capable documents and external
  references add a much larger parser and sanitization surface.
- **Reuse `views[].icon` for brand artwork.** Rejected: that field is a semantic
  key consumed by host UI, not a package asset path.
- **Cache only by extension version.** Rejected: equal versions may be repacked
  or served by different sources; SHA-256 content addressing is stable across
  both cases.
- **Let the frontend read local paths directly.** Rejected: it leaks filesystem
  paths and bypasses path containment, size, and format checks.

## Consequences

- Authors get recognizable products without changing navigation semantics.
- A malformed icon degrades to the type glyph rather than breaking an install.
- Release publishers must upload one optional sidecar; offline and private
  catalogs can omit it.
- WebP support adds the `image` decoder dependency to the Agent.
- Icon override state is machine-local and intentionally not portable through
  extension packages or configuration backup unless that backup includes
  `agent_home/extension-icons`.
