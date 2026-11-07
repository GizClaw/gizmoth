---
version: 1
slug: "src-pages-workspace-tsx"
primary_target: "src/pages/Workspace.tsx"
related_targets: ["src/pages/AgentPanel.tsx","src/pages/AgentModelPicker.tsx","src/pages/WorkspaceNavigation.css","src/pages/CodeWorkspace.tsx","src/pages/FileBreadcrumbs.tsx","src/pages/FileBreadcrumbs.css","src/project-files.ts","src/select-options.ts"]
---

# Project detail page

Mode: Operate. Scope: the project detail page (`src/pages/Workspace.tsx`) including the assistant panel, composer, model/effort menu, code pane chrome and a new project-information column. Preserve the pinned compact native shell, Phosphor icons, AI-chat-first layout with code hidden by default, chat left/code right, the single Run target chooser, editor tabs/tree/format, current-project write scope, and all agent lifecycle behavior.

## Direction contract

THESIS: Follow the user's Claude Code desktop references: the conversation is a quiet centered reading column, tool activity collapses into one muted summary line, and only things the user can change appear under the input. Non-editable facts move into a toggleable information column.

OWN-WORLD: Existing system type, neutral canvas, white input surface, hairline separators, shared 28/32px controls. No new colors; muted text-2 for summaries and metadata.

STORY: Read the conversation; scan what the assistant did in one line and expand only when needed; type in a single-line input that grows; change model or effort from one trigger; open the information column for app ID, version, files, package check, run target and write scope.

FIRST VIEWPORT: 40px chat toolbar with information, Run, code and More icons at its right. Messages and composer share a centered column capped at 760px. Under the input, right-aligned: one model trigger showing model name plus a muted effort word, and a context-usage ring only when real usage is known. The model menu lists two-line rows (name · provider, context window) with a check, then a separated Effort row with a submenu. No standalone provider label and no static scope label in the footer. Remove the workspace bottom status bar entirely, including with code visible. Idle shows no saved label. Saving, busy operations, results and errors use PageHeader actions in the shared titlebar, with truncating text and a spinner, check or warning glyph. Non-error results clear after six seconds; errors remain until replaced by the next operation or result.

FORM: User-directed local redesign from Claude Code screenshots and the complaint that unchangeable information sits in the footer. Inherits the pinned native world; no seed, comp or new identity is claimed.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## File navigation

Replace the static code path with reusable FileBreadcrumbs beside the existing language select. Each segment opens an HTML tree popup of siblings under that segment's parent; folders expand within the popup and files invoke the existing async onOpen callback, preserving save-before-switch. Reuse the pure tree/path helpers in src/project-files.ts, shared menu surface tokens, and selectPopupPosition with minimumWidth=240. Keep this project-scoped: no system-native menus, rename controls or outside-project access in the chooser.

Arrow keys and Home/End navigate visible rows; Right/Left expand, descend, collapse or move to a visible parent. Typeahead matches visible names. Enter/Space toggles folders or opens files, Escape closes and restores trigger focus, and successful file selection restores focus to the current breadcrumb. Preserve composition guards, outside-click dismissal and viewport bounds.

## Acceptance

For implementation validation, inspect 1200, 1000 (native minimum) and 390px in chat-only, code-visible, information-open, model menu and effort submenu states using demo data, plus the native integration suite. Verify keyboard access to the effort submenu, per-project effort persistence, tool-group running/error/skipped states without stray spinners, and no overflow. Check that the workspace has no bottom status bar in any pane configuration; idle titlebar feedback is absent, progress/results/errors truncate without displacing controls, non-error results clear after six seconds and errors remain. Exercise breadcrumbs for nested siblings, folder expansion, file opening with unsaved edits, empty/disabled states, narrow/short placement, keyboard/typeahead navigation and focus restoration. Native workspace coverage lives in test/integration/workspace-navigation.test.mjs, imported by test/integration/agent.test.mjs: macOS no-click-focus, root/nested siblings, folder arrows, Escape focus, save-before-switch and absent idle titlebar feedback, with no model request. These requirements and coverage references do not claim a passing result for the current refinements.
