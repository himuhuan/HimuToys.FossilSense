# v1.2.2 Regression Checklist

Status: current for Phase A. Use this checklist when a later phase touches server state, caches, completion, query, extension wiring, or packaging.

## Behavior Freeze

- [ ] Confirm the change is behavior-preserving or explicitly stop for OpenSpec/design updates.
- [ ] Confirm best-effort candidates are not documented or presented as compile-accurate semantic bindings.
- [ ] Confirm confidence, fallback, ambiguity, open scope, truncation, and cache invalidation behavior remains visible where applicable.

## indexing/status

- [ ] Startup indexing reaches the same visible states: discovering, checking, parsing, indexing, finalizing, ready, or failed.
- [ ] `Refresh Index` preserves dirty index behavior and status notifications.
- [ ] `Full Rebuild Index` preserves full scan, SQLite rebuild, read-model publication, and failed-state behavior.
- [ ] Config changes that affect indexing still trigger the same restart or rebuild path.

## definition

- [ ] Go to definition preserves candidate ordering by current/reachable/external/unknown/global semantics.
- [ ] Debug candidate reasons remain opt-in and do not change returned locations.
- [ ] Ambiguous and unresolved include cases preserve fallback and open scope behavior.

## ordinary completion

- [ ] `isIncomplete=true` for non-empty, empty, and truncated ordinary completion results.
- [ ] short-prefix behavior is unchanged for 1-2 character prefixes.
- [ ] truncation is recalculated for the current full prefix.
- [ ] prefix narrowing works when the user keeps typing.
- [ ] evidence-aware ranking preserves local binding, current-file overlay, indexed, and local word ordering.
- [ ] history boost remains bounded and does not outrank high-confidence current/local evidence beyond the existing policy.
- [ ] raw text fallback remains labeled as text/fallback and does not become a semantic binding.
- [ ] no per-keystroke SQLite access, full workspace scans, or unbounded parsing are introduced.
- [ ] Perf and verbose logs remain metadata-only.

## include completion

- [ ] Quote and angle include routing is unchanged.
- [ ] Same-directory, sibling/component edge, recent include, basename frequency, and path-depth evidence remain bounded ranking signals.
- [ ] Missing or invalid include paths warn/degrade without compile-style errors.
- [ ] Include perf logs do not expose raw include paths by default.

## member completion

- [ ] Member completion still routes only for `.` and `->` contexts.
- [ ] Resolved receiver results preserve owner-scoped fields and first-version method evidence.
- [ ] Weak receiver inference remains narrow and labeled.
- [ ] Fallback requires at least a 2-character prefix and returns prefix-matched members only.
- [ ] Empty or 1-character member prefixes return empty incomplete results.

## references

- [ ] Whole-word search scope, cap, and truncation reporting remain unchanged.
- [ ] Role grouping remains definition/declaration/call/write/type/read with read fallback.
- [ ] Standard references still return `Location`; grouped references command still displays role-labeled rows.
- [ ] Reference cache invalidates after document and index changes.

## semantic coloring

- [ ] Coloring remains limited to macros, types, enum constants, and best-effort current-function parameters/local variables.
- [ ] Member fields remain uncolored unless a future behavior change is explicitly accepted.
- [ ] Uncertain parse cases degrade quietly instead of over-coloring.

## hover/signature

- [ ] Hover preserves candidate signature, path, confidence/reason display, and comment fallback behavior.
- [ ] Signature help preserves exact-name candidate ranking and non-goals: no overload resolution, template inference, namespace lookup, or function-pointer binding.
- [ ] Oversized or unreadable source files degrade to current behavior.

## configuration/conflicts

- [ ] `fossilsense.mode`, completion mode, completion history mode, semantic coloring mode, include scoping, include paths, trace, and debug candidate reasons preserve defaults.
- [ ] clangd, cpptools, and ccls conflict prompt behavior remains one-time and overall-mode based.
- [ ] Bad config still falls back to defaults with visible warning.

## privacy

- [ ] Completion history remains local, bounded, anonymous, and clearable.
- [ ] No telemetry, cloud sync, ML ranker, or automatic upload is introduced.
- [ ] Debug/perf logs do not include candidate names, source snippets, accepted labels, or raw include paths by default.

## VSIX packaging

- [ ] `pnpm run package` runs the release packaging flow.
- [ ] `dist/fossilsense-vscode-1.2.2_BUILD*.vsix` exists for the release.
- [ ] The VSIX contains the release `fossilsense.exe` under extension `bin/`.
- [ ] Release notes state the behavior-preserving scope, verification performed, artifact path, and known non-goals.
