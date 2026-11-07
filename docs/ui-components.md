# Shared UI components

Import public controls from `src/ui.tsx`. Their implementation lives in `src/ui-controls.tsx`, `src/Select.tsx`, `src/ui-controls.css` and `src/Select.css`. Use the existing neutral palette and Phosphor icons rather than introducing page-specific replacements.

## Control sizes

| Size | Height | Horizontal padding | Corner radius |
| --- | --- | --- | --- |
| `sm` | 28 px | 10 px | 6 px |
| `md` | 32 px | 12 px | 8 px |
| `lg` | 40 px | 14 px | 10 px |

`Button`, `IconButton`, `Input`, `SearchInput` and `Select` share these roles. Buttons are non-submitting by default; use `type="submit"` explicitly. Give icon-only controls an accessible name.

## Select: application-rendered dropdowns

All page dropdowns use the controlled, select-only `Select` component. The trigger is an HTML button with `role="combobox"`; options are HTML listbox items rendered in the WebView. There is no native or hidden `<select>` and no system menu.

```tsx
import { Select, type SelectOption } from "./ui";

const languages: readonly SelectOption[] = [
  { value: "auto", label: "Auto" },
  { value: "lua", label: "Lua" },
  { value: "json", label: "JSON" },
];

<Select
  aria-label="Editor language"
  controlSize="sm"
  value={language}
  onValueChange={setLanguage}
  options={languages}
/>
```

### Contract

- `value`: selected string. Empty-string options are supported.
- `onValueChange(value)`: receives the string directly, not a synthetic native change event. It runs when a different value is chosen.
- `options`: readonly `{ value, label, disabled? }[]`; values must be unique and labels are strings for typeahead and accessibility.
- `controlSize`: `sm`, `md` or `lg`.
- `variant`: `default` for bordered fields or `quiet` for toolbar controls.
- `placeholder`: displayed when the current value has no matching option.
- `disabled`: disables the whole field. Empty or entirely disabled option lists also disable it.
- Standard button attributes include `id`, `title`, `aria-label`, `aria-labelledby`, `aria-describedby`, `aria-invalid`, `autoFocus`, `className`, `style` and a button ref. A supplied `name`/`form` adds a hidden input for the controlled form value.

Use an explicit accessible name. Associate supporting errors with `aria-describedby`; `aria-invalid="true"` gives the field the shared error border. Form validation and error copy remain the form's responsibility.

### Interaction

- Clicking opens or closes the menu. Opening explicitly focuses the trigger, including on macOS Safari/WebKit.
- Arrow keys navigate available options; Home and End move to the limits. Disabled options are skipped.
- Enter or Space opens and confirms. Escape cancels and restores trigger focus. Tab commits the active option and leaves normally.
- Typing matches option labels with a 700 ms prefix buffer. Longer prefixes refine the current match; repeated single characters cycle matches. IME composition keys are ignored.
- Pointer selection commits and returns focus to the trigger. Outside click, resize and ancestor scrolling dismiss the menu.
- Long labels wrap inside options; the trigger truncates within its existing layout width. The menu flips above when needed and scrolls within the viewport.

The Popover API supplies the top layer and automatic dismissal, not OS menu artwork. A portal stays inside the nearest modal dialog or parent popover so items remain interactive. A plain HTML fallback handles environments without Popover support.

### Migration

Replace native event handlers and option children with data and a value callback:

```tsx
<Select
  aria-label="Project permissions"
  value={permission}
  onValueChange={(value) => setPermission(value as FilePermission)}
  options={[
    { value: "none", label: "No access" },
    { value: "read", label: "Read only" },
    { value: "write", label: "Read and write" },
  ]}
/>
```

Documentation section/topic selectors, project permissions, editor language, provider API, web search, simulator model/target/firmware and application source selection all use this API. The documentation language follows the App language from Settings and has no separate selection control. Searchable model selection remains a specialized chooser; it shares the menu surface tokens rather than discarding its search/effort behavior.

## FileBreadcrumbs: project file navigation

`FileBreadcrumbs` in `src/pages/FileBreadcrumbs.tsx` replaces the code pane's static path with project-scoped navigation. Import it directly from that module; it is not part of the `src/ui.tsx` export surface.

```tsx
import { FileBreadcrumbs } from "./pages/FileBreadcrumbs";

<FileBreadcrumbs
  files={files}
  path={activePath}
  disabled={busy}
  onOpen={openFile}
/>
```

### Contract

- `files`: `ProjectFile[]`, containing project-relative `path` values from the existing project file listing.
- `path`: the active project-relative file path. An empty path offers a root-level file chooser.
- `disabled`: blocks navigation while the workspace is unavailable or busy; an empty file list also disables triggers.
- `onOpen(path): Promise<boolean>`: the existing workspace file-opening callback. It saves pending edits before switching files and reports whether opening succeeded. Keep saving and file access in that callback rather than introducing another path to the filesystem.

Clicking any segment lists the folders and files under that segment's parent. Clicking a folder segment therefore lists its siblings, not just its own children. Folders expand within the popup; files invoke `onOpen`. The chooser offers navigation only: rename remains in the file-actions menu, and there is no outside-project navigation.

### Interaction and reuse

The popup is an HTML tree in the WebView, with `treeitem` rows, expansion state, selected-file state and an active descendant. Click or Enter/Space opens a segment; ArrowDown also opens it. Within the tree, Up/Down and Home/End move through visible rows; Right expands a folder or enters its first child, and Left collapses a folder or moves to its visible parent. Enter/Space expands a folder or opens a file. A 700 ms typeahead buffer matches visible names, including repeated-character cycling; IME composition keys are ignored.

Escape closes the popup and restores the opening segment's focus. File selection also restores breadcrumb focus through the existing async open flow; a successful path change can focus the new current-file segment. Tab leaves normally. Outside click, resize, ancestor scrolling and an unavailable workspace dismiss the popup. Long segment labels truncate with full paths in their titles; the breadcrumb row scrolls horizontally and long popup names wrap.

`src/project-files.ts` owns the pure helpers shared with the code pane: `projectFileTree` builds a folders-first, alphabetically sorted tree; `projectPathSegments` supplies each segment's path and parent; `projectTreeChildren` finds a parent's children; `visibleProjectTree` flattens expanded nodes with their depth. These helpers consume project metadata and perform no filesystem access. Typeahead reuses `matchSelectValue` from `src/select-options.ts`; tree arrow navigation stops at the first or last visible row. Placement reuses `selectPopupPosition(anchor, viewport, count, 240)`: the optional `minimumWidth` is 240 px for this tree, still constrained to the viewport. The Popover API provides top-layer placement and dismissal with an HTML fallback; it never creates a system-native menu.

## Menu tokens

`src/ui-controls.css` defines `--menu-radius` (12 px), `--menu-padding` (6 px), `--menu-item-radius` (6 px), `--menu-item-height` (32 px) and `--menu-shadow`. Shared Select, FileBreadcrumbs, project/file action menus and the model picker reuse the appropriate surface roles. Keep action menus, listbox selects, file trees and searchable model dialogs semantically distinct.

## Workspace feedback and page layout

Workspace saving, busy operations, results and errors render through `PageHeader.actions` in the shared titlebar. There is no workspace bottom status bar or idle saved label. Progress uses a spinner, settled results a check, and errors a warning glyph. Text truncates within the available titlebar space, with the full message in its title. Non-error results clear after six seconds; errors remain until the next operation or result replaces them.

Ordinary `.page` scroll areas fill the main workspace; only their centered child content is capped at 1040 px, or 712 px for `.page.narrow`. Do not cap the scroll container itself. The documentation grid also fills the main workspace, with a 224 px navigation column and the reader scrollbar at the far right. Only the reading column is centered and capped at 72ch; ordinary page content caps do not constrain the documentation root.

## App appearance and language

Settings uses the same in-app Select for theme (`system | light | dark`) and App language (`system | zh-CN | en`), both defaulting to `system`. Components subscribe with `useTheme()` or `useI18n()`; translation uses Chinese source keys and named placeholders, with English messages in `src/locales/`. Never translate arbitrary user data or mutate DOM text. Documentation reads the resolved App language rather than maintaining its own setting.

`src/theme.css` owns all theme-dependent interface colors, including status fills, selection, focus and menu shadows. Feature CSS consumes these roles instead of declaring another palette. The editor changes syntax theme and semantic surfaces without remounting. Hardware artwork and the black framebuffer retain their physical colors.

Native getters are available to main and simulator windows through focused read permissions; setters belong only to main. Persisted `desktop.json` values are authoritative. App events synchronize independent simulator origins; localStorage is a first-paint cache. Browser demos use localStorage plus storage events. Preference writes preserve other desktop settings and unknown fields.

## Verification

Existing Select unit tests cover disabled/empty choices, wraparound, typeahead refinement and viewport placement. Browser checks exercise real option clicks, keyboard and IME behavior, modal layering and narrow/short windows. Native integration tests confirm that macOS uses HTML listboxes, that explicit focus works without platform click-focus, and that Escape closes a dropdown before its modal dialog. Tests read `data-value` from the combobox and drive its actual option menu instead of mutating a native element.

`test/integration/workspace-navigation.test.mjs`, imported by `test/integration/agent.test.mjs`, adds native workspace coverage for explicit focus when macOS clicks do not focus buttons, root and nested sibling navigation, folder arrows, Escape focus restoration, save-before-switch and absent idle titlebar feedback, without a model request. This records scenario coverage, not a passing result for the current refinements. Implementation validation must also cover typeahead and the remaining keyboard paths, empty/disabled states, viewport bounds, progress/result/error feedback and a far-right reader scrollbar in wide windows.
