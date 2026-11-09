---
"name": "GizMoth"
"description": "Current desktop interface for GizOS device controls and Lua project development."
"colors":
  "bg": "#f7f7f8"
  "side": "#f2f2f4"
  "card": "#fff"
  "line": "#e6e6ea"
  "line-soft": "#efeff2"
  "text": "#1d1d1f"
  "text-2": "#6e6e73"
  "text-3": "#a1a1a6"
  "hover": "#ebebee"
  "ok": "#1f9d55"
  "warn": "#d68a1a"
  "bad": "#d93f33"
  "control-hover": "#f4f4f6"
  "primary-hover": "#3a3a3c"
  "focus": "#3478f6"
  "field-focus": "#73737b"
  "bad-hover": "#b82d25"
  "application-success": "#23683f"
  "application-warning": "#805600"
  "application-delete-hover": "#fbe8e6"
  "nav-text": "#333336"
  "row-hover": "#fafafb"
  "simulator-screen": "#000"
  "simulator-screen-text": "#c2c8d2"
  "simulator-screen-muted": "#9da7b7"
  "simulator-key-active": "#e1ebff"
  "simulator-key-active-text": "#2058bc"
  "simulator-selection": "#c6dbff"
  "simulator-selection-text": "#172b4d"
"typography":
  "body":
    "fontFamily": "Monda, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "13px"
    "fontWeight": 400
  "headline":
    "fontFamily": "Montserrat, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "22px"
    "fontWeight": 400
    "lineHeight": 1.3
    "letterSpacing": "-0.4px"
  "title":
    "fontFamily": "Monda, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "14px"
    "fontWeight": 400
  "application-title":
    "fontFamily": "Montserrat, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "16px"
    "fontWeight": 400
    "lineHeight": 1.35
  "row-title":
    "fontFamily": "Monda, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "14px"
    "fontWeight": 400
  "label":
    "fontFamily": "Monda, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "12.5px"
    "fontWeight": 400
  "metadata":
    "fontFamily": "Lato, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "12px"
    "fontWeight": 400
  "chip":
    "fontFamily": "ui-monospace, \"SF Mono\", Menlo, Consolas, monospace"
    "fontSize": "11.5px"
  "editor-tab":
    "fontFamily": "Monda, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "12px"
    "fontWeight": 400
  "code":
    "fontFamily": "'SFMono-Regular', Consolas, 'Liberation Mono', monospace"
    "fontSize": "13px"
    "lineHeight": 1.85
  "device-metadata":
    "fontFamily": "Monda, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "12px"
    "fontWeight": 400
    "lineHeight": "18px"
  "settings-group":
    "fontFamily": "Lato, \"PingFang SC\", \"Microsoft YaHei\", system-ui, sans-serif"
    "fontSize": "13px"
    "fontWeight": 400
    "lineHeight": "20px"
"rounded":
  "keycap": "4px"
  "tag": "5px"
  "chip": "6px"
  "icon-button": "6px"
  "control": "8px"
  "row-icon": "9px"
  "preset": "10px"
  "radius": "12px"
  "tile": "14px"
  "identity-tile": "16px"
  "identity-card": "20px"
"spacing":
  "4": "4px"
  "6": "6px"
  "8": "8px"
  "10": "10px"
  "12": "12px"
  "14": "14px"
  "16": "16px"
  "20": "20px"
  "24": "24px"
  "28": "28px"
"components":
  "button-default":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.control}"
    "padding": "0 12px"
    "height": "32px"
  "button-primary":
    "backgroundColor": "{colors.text}"
    "textColor": "{colors.card}"
    "rounded": "{rounded.control}"
    "padding": "0 12px"
    "height": "32px"
  "button-primary-hover":
    "backgroundColor": "{colors.primary-hover}"
  "button-danger":
    "backgroundColor": "{colors.bad}"
    "textColor": "{colors.card}"
    "rounded": "{rounded.control}"
    "height": "32px"
    "padding": "0 12px"
  "button-danger-hover":
    "backgroundColor": "{colors.bad-hover}"
  "button-ghost":
    "backgroundColor": "transparent"
    "rounded": "{rounded.control}"
    "height": "32px"
    "padding": "0 12px"
  "input":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.control}"
    "height": "32px"
    "padding": "0 12px"
  "select":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.control}"
    "height": "32px"
    "padding": "0 34px 0 12px"
  "navigation":
    "backgroundColor": "transparent"
    "textColor": "{colors.nav-text}"
    "rounded": "{rounded.control}"
    "height": "32px"
    "padding": "0 10px"
  "tag":
    "backgroundColor": "{colors.side}"
    "textColor": "{colors.text-2}"
    "rounded": "{rounded.tag}"
    "padding": "1px 6px"
  "chip":
    "backgroundColor": "{colors.control-hover}"
    "typography": "{typography.chip}"
    "rounded": "{rounded.chip}"
    "padding": "3px 8px"
  "card":
    "backgroundColor": "{colors.card}"
    "rounded": "{rounded.radius}"
  "settings-row":
    "padding": "10px 16px"
  "settings-select":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.chip}"
    "height": "28px"
    "width": "192px"
    "padding": "0 32px 0 10px"
  "settings-input":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.chip}"
    "height": "28px"
    "width": "192px"
    "padding": "0 10px"
  "status":
    "textColor": "{colors.text-2}"
  "simulator-key-pressed":
    "backgroundColor": "{colors.simulator-key-active}"
    "textColor": "{colors.simulator-key-active-text}"
    "rounded": "{rounded.control}"
    "height": "32px"
    "padding": "0 8px"
  "simulator-popover":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.radius}"
    "padding": "16px"
    "width": "312px"
  "simulator-error":
    "textColor": "{colors.bad-hover}"
  "simulator-screen":
    "backgroundColor": "{colors.simulator-screen}"
    "rounded": "{rounded.control}"
  "application-dialog":
    "backgroundColor": "{colors.card}"
    "padding": "20px 20px 12px"
    "width": "min(420px, calc(100vw - 32px))"
  "application-dialog-title":
    "typography": "{typography.application-title}"
  "application-dialog-error":
    "textColor": "{colors.bad-hover}"
  "application-dialog-success":
    "textColor": "{colors.application-success}"
  "application-dialog-warning":
    "textColor": "{colors.application-warning}"
  "application-dialog-delete-hover":
    "backgroundColor": "{colors.application-delete-hover}"
  "pair-dialog":
    "width": "440px"
  "pair-results":
    "backgroundColor": "{colors.card}"
    "rounded": "{rounded.control}"
  "pair-device-row":
    "padding": "10px 12px"
  "editor-tabbar":
    "backgroundColor": "{colors.bg}"
    "height": "40px"
    "padding": "0 6px"
  "editor-tab-active":
    "backgroundColor": "{colors.hover}"
    "textColor": "{colors.text}"
    "typography": "{typography.editor-tab}"
    "rounded": "{rounded.chip}"
    "height": "29px"
  "editor-language-select":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.chip}"
    "width": "136px"
    "height": "28px"
    "padding": "0 32px 0 10px"
  "editor-tree":
    "backgroundColor": "{colors.side}"
    "width": "244px"
  "agent-model-popover":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.radius}"
    "padding": "6px"
    "width": "312px"
  "agent-model-effort-menu":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "10px"
    "padding": "6px"
    "width": "176px"
  "agent-composer-input":
    "backgroundColor": "{colors.card}"
    "rounded": "14px"
    "padding": "8px"
  "agent-attachment-thumb":
    "rounded": "{rounded.control}"
    "size": "56px"
  "agent-attachment-remove":
    "backgroundColor": "{colors.text}"
    "textColor": "{colors.card}"
    "rounded": "50%"
    "size": "18px"
  "agent-message-image":
    "backgroundColor": "{colors.bg}"
    "rounded": "{rounded.control}"
    "width": "160px"
  "agent-context-popover":
    "backgroundColor": "{colors.card}"
    "textColor": "{colors.text}"
    "rounded": "{rounded.radius}"
    "padding": "12px"
    "width": "248px"
  "workspace-info-permission-select":
    "rounded": "{rounded.control}"
    "width": "112px"
    "height": "28px"
  "workspace-info-column":
    "backgroundColor": "{colors.bg}"
    "width": "264px"
  "workspace-info-floating":
    "backgroundColor": "{colors.card}"
    "rounded": "{rounded.radius}"
    "width": "264px"
  "editor-status":
    "textColor": "{colors.text-2}"
    "height": "28px"
  "device-identity":
    "backgroundColor": "{colors.card}"
    "rounded": "{rounded.identity-card}"
    "padding": "16px"
  "device-identity-tile":
    "backgroundColor": "#f2f2f7"
    "rounded": "{rounded.identity-tile}"
    "size": "112px"
  "device-identity-name":
    "typography": "{typography.headline}"
  "device-identity-meta":
    "textColor": "{colors.text-2}"
    "typography": "{typography.device-metadata}"
  "device-remove":
    "backgroundColor": "transparent"
    "textColor": "{colors.bad-hover}"
    "rounded": "{rounded.icon-button}"
    "height": "28px"
  "device-remove-hover":
    "backgroundColor": "{colors.application-delete-hover}"
    "textColor": "#a5221e"
  "device-vital-low":
    "textColor": "#8a5a12"
  "device-info-toggle-open":
    "backgroundColor": "transparent"
    "textColor": "{colors.text}"
    "rounded": "{rounded.icon-button}"
    "height": "28px"
---

# Design System: GizMoth

## Overview

This record describes the currently implemented React interface inside the GizMoth desktop application. It captures the shared titlebar and collapsible primary sidebar, project discovery and nested navigation, device controls and installed-application library, Lua editor/pi-agent views, simulator profile manager and independent windows, provider forms, and dialogs. Planned serial flashing and direct firmware development do not yet have an implemented visual system in the sampled sources.

The interface follows the user's compact magpie reference with locally served regular Montserrat, Monda and Lato, neutral surfaces, and compact actions. Device detail follows their explicit local iPhone/Apple Home direction: compact square controls expand into dark panels with an emissive screen, a rounded faceted hemisphere lamp, or an upward sound cone tied to the selected level; outside clicks dismiss them. Independent application icons open an action dialog from the home-screen grid. It displays actual metadata and offers device run, simulator profile selection with matching local source, and confirmed deletion. This remains an implementation record, not a new global visual identity.

**Key Characteristics:**

- Shared titlebar, collapsible desktop sidebar, and independently scrolling work areas.
- Compact controls, rounded containers, and thin neutral borders.
- Recognizable application icons and semantic status alongside a neutral editor and shell.

Evidence: `src/styles.css`, shared controls in `src/ui-controls.tsx` / `src/ui-controls.css` re-exported by `src/ui.tsx`, `src/App.tsx`, `src/Editor.tsx`, and `src/pages/` (especially Projects, Workspace, Devices, DevicePrograms, DeviceKeyEditor, GemEmitter, SimulatorManager, Simulator and GamePreview). Profile and window mechanics are in `src/simulator-profiles.ts`, `src/simulator-window.ts` and `src-tauri/src/simulator.rs`; native shell dimensions and macOS traffic lights are in the Tauri configuration files. Earlier browser checks covered accessory/application layouts from 320–3200px; captures include `.impeccable/review/ios-device-1200.png`, `ios-device-2400.png`, `ios-device-390.png`, `ios-device-adjuster.png`, and the 11-06 `device-identity-{320,390,780,1000,1200,2400}.png` and `device-identity-expanded.png`, which are historical captures of the previous device header. These captures do not verify the current native window behavior. The pairing and model-picker sources are `PairDialog.tsx` / `.css`, `AgentModelPicker.tsx` / `.css` and `AgentPanel.tsx`; pairing direction is recorded in `.impeccable/surfaces/src-pages-pairdialog-tsx.md`. Supplied browser captures are `.impeccable/review/pairing-final-1200.png`, `results-340.png` and `agent-model-{full,split,mobile}-open.png`; the model-picker captures predate the flat model list and effort submenu. Synthetic IME events were verified; OS IME input and hardware pairing were not. Packaged macOS firmware installation/start and the earlier provider-grouped model picker have been observed without Vite. The breathing-light application package was accepted for installation, but the selected Trial firmware rejected its run request; successful application execution remains unverified. Real-device controls, full gameplay, Windows and Intel Mac simulator execution remain unverified.

The device identity card's sources are `src/pages/DeviceIdentity.tsx` / `.css`, integrated by `DeviceDetail` in `src/pages/Devices.tsx`; `statusText`, `statusTone`, `vitalsOf`, `batteryLevel` and `offlineVitalsText` live in `src/device-model.ts`; the model view is `src/pages/DeviceModelView.tsx`. The direction contract is `.impeccable/surfaces/src-pages-deviceidentity-tsx.md`. Its 11-07 evidence in `.impeccable/review/` is `desktop.png` (1200×800, online), `mobile.png` (390×844, offline), `device-identity-340.png`, `device-identity-expanded-{1200,1000,390,340}.png`, the state crops `device-header-after-*.png` and `device-header-before-*.png`, `device-identity-native.png` and `device-identity-native-card.png` (the real macOS window with the user's actual offline device, collapsed, with no address or key shown), `check-device-identity.mjs` with `device-identity-results.json`, `device-identity-detect.json` and `device-identity-review.md`. The frontend build and 54 tests pass; the standalone macOS debug build was rebuilt and relaunched. The first finish review returned fix with two material findings: the key placeholder was truncated at 390/340px, and the removal button's accessible name did not match its visible text. Both were scored resolved and the disposition is ship. That verdict covers those fixes; the first full review found the other contract items matched and the ceiling reached for the inherited shell. Browser checks used synthetic demo devices. Online readings on the physical device (it was offline), Bluetooth and microphone glyph states beyond code review, and Windows/Intel builds are not verified. The detector's advisories (the 20px card radius, the 16/14/12px tile radii, `#a5221e`, `#8a5a12` and the 18px narrow title) are intentional local values.

Editor documentation was reconciled with `src/pages/CodeWorkspace.tsx` / `.css`, `src/pages/FileBreadcrumbs.tsx` / `.css`, `src/Editor.tsx`, `src/editor-language.ts`, `src/editor-format.ts`, `src/editor-lua-browser.ts`, `src-tauri/src/formatter.rs`, the relevant workspace state/JSX in `src/pages/Workspace.tsx`, the pure tree/path helpers in `src/project-files.ts`, and shared popup/keyboard helpers in `src/select-options.ts`; the surface contract is `.impeccable/surfaces/src-pages-workspace-tsx.md`. Earlier editor captures predate removal of local comparison and addition of formatting/language selection; they do not verify the current editor or establish a completed native visual check. The titlebar-feedback, breadcrumb-navigation and documentation-width refinements are recorded from the approved behavior and current source; this documentation update adds no rendered or native verification.

The project detail page was reconciled with `src/pages/Workspace.tsx`, `PaneSplitter.tsx`, `ProjectInfo.tsx` / `.css`, `AgentModelPicker.tsx` / `.css`, `AgentPanel.tsx`, `WorkspaceNavigation.css` and the workspace and agent rules in `src/styles.css`; its direction is `.impeccable/review/project-page-direction.md`. Browser review captures with demo data are `.impeccable/review/project-after-{chat,effort,info}-{1200,1000,390}.png`, `project-after-{code,picker,tools-open}-1200.png`, `project-after-info-code-{1200,1440}.png` and `pane-splitter-{1200,dragging}.png`; the clear, brief and context changes are in `project-after-{context,brief,brief-edit,clear-confirm,cleared}-1200.png`. Background sessions are in `src/agent-session.ts`; permission and web enforcement in `src-tauri/src/agent_access.rs`; the search settings card in `src/pages/WebSearchSettings.tsx`; captures `project-after-permissions-1200.png` and `settings-web-search-1200.png`. Conversation storage and the brief prompt are in `src/agent.ts` and the usage breakdown in `src/agent-thinking.ts`. They are review captures, not shipping rasters, and this record does not claim a native visual check of the redesign.

The approved typography and General settings merge is grounded in `src/typography.css`, `src/styles.css`, `src/ui-controls.css`, `src/pages/Settings.tsx` / `.css`, `SettingsRow.tsx`, `WebSearchSettings.tsx` and `SettingsNavigation.tsx`; the direction contract is `.impeccable/surfaces/src-pages-settings-tsx.md`. The user subsequently rejected the static Local data section, which is omitted from General. Final removal evidence is `.impeccable/review/settings-polish-final-light-zh-1024.png`, `settings-polish-final-mobile-light-zh.png` and `settings-polish-final-native-dark-en.png`. Earlier `settings-polish-*` captures with the Local data card predate that correction and are historical. The finish reviewer reported no material visual repairs. Reported checks passed: frontend build, 92 frontend tests, `cargo check --locked`, and two rebuilt native settings/appearance tests. Browser review covered 1200/1000/390/340px, 52px rows, loaded regular fonts, no horizontal overflow, keyboard theme selection and Escape tooltip dismissal. Native switch persistence was exercised through the actual UI. The native capture is an isolated macOS WKWebView fixture with search commands deliberately disabled and an explicit unavailable message; browser-demo evidence covers synthetic key save/deletion. Real search-provider/keychain integration was not exercised. This documentation pass adds no browser or native verification and no shipping raster imagery. Earlier verification paragraphs remain historical, with their original scope; the older sidecar is reconciled only for typography and General settings, not comprehensively refreshed.

## Colors

The user requires two complete themes and an automatic mode. Settings › General offers System, Light and Dark; System is the default. `src/theme.css` owns semantic palette roles: the light canvas/sidebar/card are `#f7f7f8` / `#f2f2f4` / `#ffffff`, while dark uses `#19191c` / `#202024` / `#242429`. Dark primary/secondary text are `#f2f2f5` / `#b4b4bd`, with borders and hover surfaces raised enough to separate controls without bright outlines. Menu elevation, status fills, selection, editor gutters and syntax themes adapt together. Fixed device artwork, colorful app icons and the black hardware framebuffer retain their physical meaning. Themes change without page remounts or destructive editor/session resets.

Settings also provides System, 简体中文 and English for the App language. Documentation follows this resolved preference rather than maintaining its own picker. Both preferences default to System, persist in native `desktop.json` and synchronize across windows through focused read commands and events; only the main window can change them. Browser demos use localStorage and storage events. Chinese source keys and named placeholders in `src/locales/` localize app-authored UI only; user names, conversations, source code and external diagnostics are preserved. The settings selectors use existing small in-app Select controls, 192px wide (128px at viewport widths up to 600px), in the first list card.

### Primary

The `text` color also fills primary actions; `card` supplies their contrasting text. `primary-hover` is the primary action's hover fill. There is no separate chromatic brand accent in the shell.

### Neutral

`bg` is the application canvas; `side` is the sidebar and tag background; `card` is the white editor and container surface. `line` outlines controls and cards; `line-soft` separates rows and panes. `text`, `text-2`, and `text-3` distinguish primary, secondary, and subdued information. `hover`, `control-hover`, and `row-hover` serve different interaction targets. `focus` marks keyboard focus on buttons and navigation and supplies the field focus ring; `field-focus` changes focused field borders.

Documentation and model-menu text selections use shared `selection-bg` (`#dce7fa`) and `selection-fg` (`text`) tokens. The pale-blue background and dark foreground retain 13.51:1 contrast and keep selected code legible; selection styling never depends on an undefined focus-color token.

### Functional color

`ok`, `warn`, and `bad` represent positive, warning, and error/destructive states. Device-local colors communicate hardware type: overview light glyphs use `#946511`, and audio uses `#365779`, on white square controls. The expanded panel uses a dark `#1a1b23` to `#090a0f` surface with pale text; screen effects use pale RGB 195/222/255, faceted lamp effects use blue RGB 126/209/255, and upward sound waves use RGB 148/203/255. Effect opacity and extent follow the selected level. The device canvas remains `#f2f2f7`; these local values do not change the shell palette. Application launchers use local per-app colors with a neutral fallback and white glyphs, with a slight lighter top edge. These local treatments follow the user's iPhone/HomeKit reference without adding a new global brand accent. Existing updater and preview-input colors remain unchanged. Text and control contrast were measured from the rendered page.

`simulator-screen` is the black framebuffer surface; `simulator-screen-text` and `simulator-screen-muted` distinguish its off-state message and explanation. Held hardware keys use `simulator-key-active` and `simulator-key-active-text`, with the existing `focus` color as their border. Simulator error text reuses `bad-hover` on white feedback surfaces. Text selection uses `simulator-selection` and `simulator-selection-text`. These are local functional roles rather than new shell accents.

The application action dialog retains its local success color (`application-success`) and uses `bad-hover` for error text and deletion. `application-warning` identifies a local-source version mismatch; `application-delete-hover` supplies the pale destructive hover fill. These scoped feedback roles do not replace global status or launcher colors.

Sidecar tonal ramps are synthesized preview metadata only. They are not product tokens and are not evidence of an implemented tonal scale.

## Typography

The existing website fonts are served locally, unchanged, as regular WOFF2 files in `public/fonts/`, with their SIL Open Font License 1.1 texts alongside them. `src/typography.css` defines `--font-heading` as Montserrat, `--font-body` as Monda, and `--font-small` as Lato, each followed by PingFang SC, Microsoft YaHei, system-ui and sans-serif for Chinese and unavailable glyphs. HTML headings and the titlebar brand use Montserrat; ordinary UI, list titles, labels, editor tabs and tree names inherit Monda. Lato supplies the explicitly assigned metadata, status, tags, navigation labels/counts, settings group headings, tooltip copy and About notes; the About app name uses Montserrat. A semantic title is not automatically an HTML heading: the `title`, `row-title` and `label` roles retain Monda. Device identity metadata inherits Monda, recorded separately as `device-metadata`; its semantic meaning does not assign the shared Lato face. Code and model chips preserve their separate monospace stacks.

**The Regular-Only Type Rule.** All explicit interface weights are 400, including selected navigation, component headings and controls. HTML headings, b, strong and th are reset to 400, and `font-synthesis: none` remains enabled. Use the existing sizes, spacing, color and font roles for hierarchy; do not add bold or synthetic emphasis.

The hierarchy retains device identity (`headline`), section titles (`title`), list titles (`row-title`), body text (`body`), form labels (`label`), and secondary metadata (`metadata`). Device identity retains its recorded line height and tracking; the application-library heading remains 20px with -0.3px tracking. The controls heading is 15px. Application names are centered below icons at 12.5px and may wrap to two lines. The application action dialog uses `application-title`, 12px version metadata and an optional 11px identifier, omitted when it repeats the name. Long identity and metadata text wrap. Destination headings use 13px; descriptions and feedback use 12px. Square-control names use 12px and readings use 11.5px tabular figures, reducing to 10.5px in the narrowest console. Adjustment-panel values use 44px tabular figures with an 18px percent sign. Device identity reduces to 18px at console widths up to 600px. Body line height is not globally specified; conversational text and dialog descriptions use 1.6, while the editor retains its recorded 1.85. Path metadata remains 11.5px and the path bar language selector uses the shared small control role.

Simulator chrome inherits Monda: window title 14px with -0.02em tracking, popover headings 13px, body and hardware labels 12px, and runtime/field metadata 11.5px. Shortcut keycaps use inherited proportional text at 11px. Popover and feedback copy uses 1.5 line height. Settings group headings explicitly override HTML-heading Montserrat with the `settings-group` Lato role.

## Layout

The viewport is a full-height grid shell with document scrolling disabled. A shared 52px titlebar spans the sidebar and content; page breadcrumbs and actions portal into its flexible center. The expanded sidebar and titlebar brand area share a 208px width, including on macOS. Collapsing uses a 56px rail; macOS reserves a 120px transparent titlebar area for traffic-light clearance. The main macOS window uses an overlay titlebar with hidden native title and traffic lights at `(16, 28)`. General pages keep their internal scroll area full width and horizontally center their child content within the right pane, with 20px 24px 28px padding and a 14px gap. The shared content column caps at 1040px; Open Source content caps at 712px, preserving the former 760px outer width minus its 48px horizontal padding. General settings uses the shared 1040px cap, with 24px between groups. Page content remains top-aligned. Individual components retain their narrower limits, including the centered 520px simulator editor. Header actions use 8px gaps. Main native windows default to 1200 × 780 with a 1000 × 680 minimum.

Device detail follows the user's local iPhone/Apple Home references. Content is centered and capped at 840px with 24px 24px 40px page padding and 32px between content groups; at viewport widths up to 600px, padding becomes 16px 16px 28px and the gap becomes 28px. The device identity card leads the page (described below). Accessory controls are 112px square buttons grouped with 12px gaps; they retain their dimensions on wide screens. Below 420px console width, three controls fit across using a 64–100px size range, with compact typography below 280px. Clicking a square expands a 360px-wide dark panel from that square's position. Its visual interaction area is 180–260px high depending on available viewport height, with a native vertical range input over screen, faceted-lamp, or directional sound feedback. There is no permanent large fader on the overview and no Done button in the panel; outside click, Escape, or the secondary close icon dismiss it. Applications have independent icons and names in six columns, four at library widths up to 680px, and three at up to 280px. Icons are 76px, becoming 60px at library widths up to 430px; this breakpoint also removes inline launcher padding to give captions more room. Application actions and version metadata live in the application dialog rather than an icon management mode.

The device identity card is padded 16px on a grid of a 112px model column and a flexible text column (`minmax(0, 1fr)`) with a 20px column gap. Its areas are model/head, model/strip and empty/details: the tile spans the head and strip rows, and details open in the text column. The head uses a two-column grid: the single-line name/editor and removal occupy the same vertically centered first row, with model and connection metadata on a separate second row. Input, save, cancel and removal share one centerline in edit mode. The status strip and each information row have a fixed 44px height. Detail labels use a 128px column with a 16px gap; values truncate with tooltips and controls stay inline. Breakpoints are container queries on the device console. At ≤600px the tile is 64px, the column gap 14px and the name 18px; the head centers vertically beside the tile, and the strip and details span the card width with a 14px margin above the strip. Removal and the disclosure become icon-only. At ≤360px labels use an 88px column and an 8px gap; reveal and save retain their shared 28px icon controls and 2px gap. Rows remain single-line at narrow widths. At ≤300px the tile is 52px, padding 14px and column gap 12px. The native window's 1000px minimum always yields the wide layout (a 744px card); narrower layouts apply to browser previews and narrow containers.

The application action dialog uses the `application-dialog` width and inset, a maximum height of viewport height minus 32px (`100dvh`) and internal scrolling. At viewport widths up to 380px, side insets reduce to 16px. Its header pairs a 48px icon with identity text across a 12px gap; 20px separates the header from its content. Device and simulator sections each use 16px vertical padding and a `line-soft` top separator. Trailing Run buttons align right with a 76px minimum width. Source/profile fields use a 28px label column, a 12px gap and a flexible full-width in-app Select; the destructive footer has a separate top rule and 12px top padding.

The workspace keeps project-category navigation in the primary sidebar; an optional file tree belongs inside the code pane. Project navigation expands below Projects inside the 208px primary rail and contains three categories: Lua applications, Giz firmware, and native firmware. The authoring area starts directly beside that rail with AI chat visible and code hidden by default. Show code reveals a two-pane layout in wide windows, with chat on the left and code on the right. Narrow windows show the editor instead of chat until the user returns to chat. Both panes remain mounted while hidden, preserving editor and conversation state. A single Run action opens a chooser for hardware or independent simulator targets before execution. Code visibility, file selection, package checks, export and file/project actions remain available. The workspace toolbar has no persistent target dropdown, simulator-management icon, separate simulator launcher or code/pi-agent mode selector.

On the project page, chat, code and the optional information column share one row. When code is visible and the authoring container is at least 900px wide, a 1px `line-soft` divider separates the panes. Chat width defaults to 420px, never drops below 300px and always leaves the code pane at least 380px; the chosen width persists across visits. The conversation and composer share one centered reading column: messages cap at 760px with at least 20px side padding, and the composer caps at 768px with at least 16px side margins and 12px below. The 264px information column sits in flow at the far right of the row, on the `bg` canvas behind a `line` hairline, when the row is at least 720px wide with chat only or at least 1164px with code open (container widths). Below those widths it floats 8px inside the right edge as a white card over the chat or code pane. The column is not persisted; every visit starts with it closed.

Simulators has its own primary navigation entry for named independent devices. Each profile opens a 560×860px window with a 340×600px native minimum; reopening the profile focuses the same window. On macOS its draggable 52px header merges with the native overlay titlebar, hides the native title, reserves 92px on the left and positions traffic lights at `(16, 28)`. The header names the simulator. A 40px white toolbar holds a 132px default bordered model select, model/screen segmented control and firmware-popover trigger. The stage flexes above a 49px hardware strip, including its top separator, and a 34px runtime footer. Five 32px controls form equal columns with 4px gaps, capped at 600px and centered beside keyboard help on wide windows. At widths up to 440px, view and hardware labels hide while Phosphor icons and accessible names remain. Saved browser metrics record no normal-state overflow at 560×860, 340×600 and 1000×780; these metrics do not establish native execution or a final review verdict. There is no project picker or permanent current-project panel. Browser fallback uses a resizable popup. Workspace and device-app actions send temporary application requests to a selected profile; source comes from the open workspace's unsaved reader or saved files, without a lasting project association. Generic dialogs cap at 440px or 600px and remain within viewport width minus 40px; pairing and application dialogs use their documented local bounds. The frontmatter spacing keys label observed pixel values, not existing CSS variable names.

Browser fallbacks use max-width queries: at 1100px titlebar spacing tightens; at 760px primary navigation becomes a 52px icon rail, titlebar utilities simplify, page padding becomes 16px, and project search fills its row. At narrow browser sizes, project and Settings sub-lists remain in the primary sidebar; the titlebar toggle opens that sidebar as a temporary overlay and selecting a child page closes it. Workspace actions remain in the chat pane header; at widths up to 600px the category breadcrumb hides while the chat actions retain their compact icon layout. These fallbacks do not change the native minimum window size.

## Elevation & Depth

The shell and ordinary list surfaces use tones and thin borders. Device accessory tiles have a restrained downward soft shadow (`0 2px 6px #1d1d1f06`) without an additional border. Application icons use a small downward shadow and inner top highlight to read as independent home-screen objects. Native modal dialogs retain their larger soft shadow and translucent backdrop; existing sidebar and switch treatments remain unchanged.

The device model sits in the identity card's flat `#f2f2f7` tile; neither the card nor the tile has a shadow or outer border. Its interactive 3D rendering and material lighting come from the external models-player package and CDN geometry. The player's blurred `.nebula` glow is hidden on the tile because WKWebView rendered it as a pale box. The tile introduces no gradient or shadow treatment.

Simulator firmware and keyboard-help popovers use a soft downward shadow (`0 12px 36px #18202e26`); temporary application/error feedback uses `0 8px 24px #18202e1a`. Popovers enter the native top layer without a modal veil. Feedback overlays the stage above the hardware controls.

Workspace action menus use a soft downward shadow (`0 8px 24px #1d1d1f26`) without a modal veil. The assistant model picker pairs a faint 1px ring with a soft popover shadow (`0 0 0 1px #1d1d1f0f, 0 8px 32px #1d1d1f26`) without a modal veil; its effort submenu uses the same ring with a slightly tighter shadow (`0 0 0 1px #1d1d1f0f, 0 8px 24px #1d1d1f24`), and the context popover reuses the picker's ring and shadow. The floating project information panel uses the ring with `0 8px 32px #1d1d1f1f`; in flow it is flat, with only its left hairline. The composer input carries a barely visible lift (`0 2px 8px #1d1d1f08`). Pairing retains the existing modal backdrop and dialog shadow.

## Shapes

Shell controls and navigation retain their shared radii. Square device controls use 20px corners, becoming 18px or 14px in narrow containers. Their expanded control panels use 32px corners. Every panel keeps its widening cone in the upper region, connected to a small emitter at the bottom. The screen is a horizontal plane in a tilted 2.5D view. `GemEmitter` parametrically projects a rounded hemisphere from 25° above its horizontal circular base, with fine triangular facets, depth ordering and a visible lower rim. The lamp keeps its physical base at the bottom; brightness changes shading and spill without moving its geometry. The acoustic field uses concentric planes, a visible side wall, and front/back shading. These are authored geometric control effects, with a transparent native range input over them. Application icons are 76px rounded squares with 22% corners, reducing to 60px at library widths up to 430px. An application launcher contains its icon and name. Its action dialog contains an icon/metadata summary, separated device and simulator sections, conditional source/profile fields, destination-local feedback, and a red delete footer. The launcher grid has no application card, row separator, or repeated Open capsule. Small circular result badges distinguish acknowledged requests from failures.

Simulator screens and segmented-view containers use the existing 8px control radius; popovers and feedback use the existing 12px container radius. Keyboard-help keycaps use the local `keycap` radius, a thin neutral border and 2px 6px padding.

The device identity card uses `identity-card` corners, matching the square controls beneath it. Its model tile uses `identity-tile` corners, reducing to 14px at console widths up to 600px and 12px up to 300px, and clips the model. `line-soft` hairlines separate the strip and detail rows. These are intentional local radii, not new shell steps.

## Components

### Buttons

`Button`, `IconButton`, `Input`, `Select`, and `SearchInput` are shared typed primitives exported through `src/ui.tsx`, implemented in `src/ui-controls.tsx` and `src/ui-controls.css`. A `controlSize` selects one coherent role: `sm` is 28px high with 10px horizontal padding and 6px corners; `md` is 32px with 12px padding and 8px corners; `lg` is 40px with 14px padding and 10px corners. Dense workspace controls use `sm`; normal fields and actions, including the application action dialog, use `md`. The application dialog uses `sm` for its local-source read retry. Pages may constrain width and layout, but must not override a primitive's height, padding, arrow offset, or focus geometry.

Default buttons are white with a neutral border; `variant="primary"` is dark, `danger` is destructive, and `ghost` is transparent. `Button` and `IconButton` default to `type="button"`; form submission is explicit. Icon buttons require an accessible name or title. Shared disabled opacity is 0.5, except status-bearing updater feedback that deliberately stays readable. Keyboard focus uses a two-pixel blue outline with a two-pixel offset.

Danger buttons have an explicit dark-red `bad-hover` background and border while retaining white text. Ghost buttons retain the generic pale hover fill. The danger preview includes this specific override.

### Inputs / Fields

`Select` is the shared, controlled in-app combobox in `src/Select.tsx`, re-exported from `ui-controls` and `ui`. The user explicitly rejected system-native dropdown menus. A button with `role="combobox"` displays the selected label and a 14px Phosphor caret; HTML listbox options are rendered in the WebView with a check on the selected row. There is no native or hidden select. Callers pass `options` and `onValueChange(value)` rather than option children or synthetic native events. Shared 28/32/40px geometry and `variant="quiet"` remain available. Empty/unavailable lists disable the trigger; disabled items are skipped; `aria-invalid` uses the error border. Popup menus use shared `menu-radius` (12px), `menu-padding` (6px), `menu-item-radius` (6px), `menu-item-height` (32px) and `menu-shadow` tokens, also consumed by action menus and model picker surfaces. Labels wrap within the viewport-bounded menu while the trigger truncates within its page width.

Opening explicitly focuses the trigger for macOS WebKit. Arrows/Home/End navigate, Enter/Space confirm, Escape cancels and restores focus, and Tab commits then leaves. A 700ms typeahead buffer refines the current match for longer prefixes and cycles for repeated single characters; composition keys are guarded. The popup flips above when needed and scrolls internally. A portal stays inside the nearest dialog, parent popover or `data-popover-scope` owner, outside wrapping labels. The project information overlay is a scope so selecting a permission does not dismiss the panel. Popover supplies top-layer placement/dismissal only; plain HTML is the fallback when unsupported. Usage and migration examples are in `docs/ui-components.md`.

`Input` handles ordinary text/password fields. `SearchInput` owns its leading search icon and clear target; clearing returns focus to the input, and Escape clears a nonempty query without submitting a form. Search fields on devices, projects and applications use the same 32px role. Range controls and switches retain their specialized native input implementations. The assistant composer separates its white input surface from a right-aligned footer of changeable controls below it, following the user's supplied reference. The input has a 14px radius, 8px padding, the 添加图片 (Paperclip) icon button at the left and, at the right, a bare 28px Send icon (Phosphor ArrowElbowDownLeft, no filled circle; `text-3` while disabled) that becomes a non-interactive spinner while the assistant runs. There is no Stop button: Esc stops the run unless a dialog or popover is open, and the run status line in the conversation shows an "Esc 停止" key hint. Each project has one conversation, so the composer has no new-conversation action; clearing lives in the chat toolbar. The textarea starts as one line at 28px and grows to 180px; the input is normally 46px high. Focus within changes the input border. The model trigger and the optional context ring sit outside the input (see the assistant model picker below); narrow layouts wrap them instead of squeezing their labels.

Image attachments enter through the Paperclip button, paste, or a drop onto the input, which shows a dashed `field-focus` border while dragging. PNG, JPEG, GIF and WebP are accepted, up to four per message. Images longer than 1568px on either side or larger than about 3.75 MB are scaled down and re-encoded as JPEG at quality 0.85, flattened onto white. Pending images form a wrapping row of 56px cover-cropped thumbnails above the text, with `control` corners and a `line` ring. Each has an 18px dark circular remove button at its upper-right corner, with an enlarged hit area and the accessible name 移除图片 followed by the file name. A message may contain images alone. Capability comes from the selected model's declared input: the provider list's modalities when reported, otherwise the SDK catalog entry for a known model id. Without image input, the Paperclip button is disabled and its title explains why, and sending is blocked while images are attached. Sent user messages show their images right-aligned at up to 160px on the `bg` tone with `control` corners. Saved history stays under a 7 MiB budget by replacing the oldest image data with （图片较大，未保存在对话记录中）.

```tsx
import { Button, Select, Input, SearchInput } from "./ui";

<Select
  controlSize="sm"
  aria-label="Target device"
  value={deviceId}
  onValueChange={onDeviceChange}
  options={devices.map((device) => ({ value: device.id, label: device.name }))}
/>
<Button controlSize="sm" variant="primary" onClick={run}>Run</Button>
<Input aria-label="Project name" value={name} onChange={onNameChange} />
<SearchInput aria-label="Search projects" value={query} onChange={onQueryChange} onClear={() => setQuery("")} />
```

Use the shared API for ordinary selects. The assistant model picker is the scoped searchable-popover exception described below; it keeps its specialized search/effort behavior while sharing the menu surface tokens.

### Navigation

Sidebar items use a compact icon-and-label row, including the dedicated Simulators entry. Hover adds the neutral hover fill; selection adds a white background, border, subtle shadow, and weight 400. Phosphor (`@phosphor-icons/react`) is the sole regular interface icon family; the GizMoth mark and authored hardware-effect geometry remain product artwork. Collapsed navigation hides labels and counts and centers icons in 36px-high rows. The shared titlebar holds the mark, page context and conditional update control at the far right. Version information belongs in Settings > About. Project, provider and simulator creation use a quiet plus/text action at the upper left of their lists; the item count sits at the upper right, and no creation action is portaled into the titlebar. Device pairing follows the same layout above the device list, with no persistent Connect device row in the primary sidebar. Empty states retain the same list-toolbar entry without duplicate creation actions. Project search remains between creation and count on desktop and moves to a full-width second row in narrow browsers. Device detail keeps its own Refresh button and omits the duplicate global refresh icon. Other pages retain the global refresh action before Update. The update control is absent until a newer release is found, including at narrow widths. Its compact muted-violet Update badge changes in place through download percentage, signature verification, preparation, installation and restart after one click; tabular numerals keep progress stable. Unknown totals display downloaded bytes, and retry remains in the same control with an error tint. The native single-instance handler restores and focuses the main window on a second launch.

### Menu bar / system tray

The tray follows the user's TailSplit reference with a compact native menu rather than a wide dashboard. It groups the app version and Open action, the paired-device count, a simulator submenu, Settings, and Quit with native separators. Simulator entries show a single-line name shortened to 18 characters plus a model name; the submenu also retains Manage simulators, including when no profile exists. Counts describe saved profiles and paired devices, not live connectivity. Native menu geometry, typography, highlighting and submenu placement follow the host OS. Device/profile mutations refresh the menu after releasing their storage locks. Simulator selection uses the main window's existing open/focus transport with no project source, so a hidden main window can dispatch it without appearing. Actions remain queued until the main webview is ready; only that window can consume them. Settings and management actions restore the main window and navigate to the requested page.

### Project discovery and workspace

Project categories appear as indented child rows below Projects in the primary sidebar: Lua applications, Giz firmware, and native firmware. A thin vertical guide expresses nesting, following the user's magpie Settings reference. Selection uses a neutral fill and regular text weight. The parent shows all projects, while category children filter discovery and remain selectable even when no matching project exists. The titlebar breadcrumb identifies the category; the content toolbar places creation at the left, search in the middle and the project/result count at the right. Narrow browsers keep creation and count on the first row and move search below. New-project kind choices retain explicit unavailable messaging for Giz and native firmware; those paths cannot be created and their category navigation does not imply implemented toolchains.

Project discovery is capped at 1040px and uses one white card containing single-line rows separated by thin internal rules, with no table header. Each 48px row uses 10px 16px padding and contains a glyph, name, kind, app/version/file metadata, relative update time and trailing caret. Long text truncates; below 600px the kind is hidden and horizontal padding reduces to 8px. The complete row remains a keyboard-focusable open action.

Inside an open project, AI chat is the initial authoring surface. The titlebar carries project context. The chat pane has a 40px header with a quiet conversation label and five 28px icon controls at its right edge, in order: clear conversation (Phosphor Broom, 清空对话), project information, Run, code (Phosphor BracketsCurly) and More for project actions. The assistant panel renders the clear action into the toolbar's slot; it is disabled while the assistant runs or the conversation is empty. Clearing first asks in the app's confirm dialog with 取消 and a red 清空 button, then empties the messages and keeps the project brief. It aligns with the neighboring 40px code-tab strip and ends at the chat/code divider. The information and code toggles expose their expanded state and use the neutral hover fill while open; controls remain inside chat rather than in the application titlebar. Closing the code pane returns to chat when the layout shows only one pane. The project menu contains package check, export and confirmed project deletion. When code is visible, its own 40px tab strip contains horizontally scrolling, closable file tabs and compact content-search, format, tree, file-menu, expand and close controls. Each 29px-high tab has a Phosphor file icon, a truncated basename, full-path title and accessible name. Tabs cap at 210px with 6px corners and a neutral active fill; a separate close button remains available. A path row with a 30px minimum separates this strip from the white document, with clickable FileBreadcrumbs on the left and a shared small language select that is 136px wide and 28px high with 3px vertical margins. File creation, rename, delete and close-all live in the file menu. Rename and delete are disabled for `app.json`. Run opens a target chooser grouping hardware devices and independent simulators, then dispatches to the chosen target. Target selection belongs in that chooser rather than persistent toolbar dropdowns or a separate simulator control. Wide layouts keep chat and code side by side when code is shown; narrow layouts show one pane at a time with a return-to-chat action. Neither files nor project contents appear under the primary Projects entry. Visibility changes preserve both mounted panes, unsaved edits and conversation drafts. Switching or closing tabs flushes active edits; reopening a file within the mounted workspace restores its cursor selection and scroll position. This is not a claim of preserved undo history. Existing save-on-leave behavior remains.

FileBreadcrumbs replaces the static code path with buttons for each project-relative segment. Each button lists siblings under that segment's parent in an HTML tree popup; folders expand within the popup and files use the existing async `onOpen` callback so pending edits save before switching. Navigation stays within the supplied project file list, with rename left in the file-actions menu. The breadcrumb row scrolls horizontally and individual labels truncate with full-path titles. The popup reuses the shared menu surface tokens and `selectPopupPosition` with `minimumWidth=240`, bounded by the viewport. `projectFileTree`, `projectPathSegments`, `projectTreeChildren` and `visibleProjectTree` in `src/project-files.ts` provide pure tree/path operations without filesystem access. Arrow keys and Home/End navigate visible rows; Right/Left expand, descend, collapse or move to a visible parent. A 700ms typeahead buffer matches visible names, Enter/Space expands a folder or opens a file, and Escape dismisses and restores trigger focus. File selection preserves breadcrumb focus through the async open flow. Composition guards, outside-click/scroll/resize dismissal and a plain HTML fallback keep the popup consistent with shared Select behavior; it uses no system-native menu. The reusable API is documented in `docs/ui-components.md`.

The optional right-hand directory tree is 244px wide, with a shared 28px filter, expandable folders and 30px file rows. At code-pane widths up to 520px, the tree overlays the right edge with a dismissible light backdrop and width limited to the smaller of 260px and 88% of the pane; it does not squeeze the document. At up to 460px, the expand control and tab file icons hide, tab width caps at 150px, and toolbar spacing tightens. These are container-width rules, independent of viewport breakpoints.

**The Explicit Formatting Rule.** The code toolbar's Format icon and Shift+Alt+F explicitly format and save the active source file. The path bar offers automatic language detection from the extension and a per-file override for Lua, JSON, JavaScript, TypeScript, HTML, CSS, Markdown, YAML or plain text. Selection changes highlighting and formatter choice without renaming files; overrides last within the open workspace and are not persisted preferences. Plain text disables formatting. Formatting uses two-space indentation and a 100-column target. Lua uses StyLua 2.6 with Lua 5.4 syntax and output AST verification through native Rust or browser-demo WASM; other source languages use Prettier standalone and bundled plugins. Formatting is text-only with a 2 MiB limit and no project-owned plugin execution. Syntax failures preserve source and surface an error. The existing 500 ms autosave does not format. This record does not claim verified undo behavior.

The project and file menus use native auto popovers: 208px wide, constrained to the viewport, with 12px corners, 6px padding and 32px minimum action height. Destructive text uses `bad-hover`; hover and focus use the neutral hover fill. Opening focuses the first enabled action; arrows and Home/End move through enabled actions, Escape closes and restores trigger focus, Tab leaves normally, and resize dismisses. Composition keystrokes do not trigger menu navigation.

The chat/code divider is a focusable vertical separator that reports its current, minimum and maximum chat width. Its 9px hit area surrounds a quiet `line-soft` hairline with no native border or glow. Hover uses `control-border-hover`; keyboard focus and drag use the neutral `field-focus` tone. Pointer drag resizes live; arrow keys move 16px (64px with Shift), Home and End jump to the limits, and Enter or a double-click restores the 420px default. It is hidden while code is expanded or the panes show one at a time.

The project information panel opens from the toolbar's information toggle and takes focus. Its 40px head holds a 13px/400 title and a small close icon; the body scrolls with 16px side padding. The first section is the project brief (项目说明). Then sections for the project (type and app ID), files, package check, run target and assistant permissions (助手权限) are divided by `line-soft` hairlines with 12px vertical padding. Section headings are 11.5px/400 in `text-2`. Facts use a two-column definition list at 12.5px/20px with `text-2` terms, and identifiers use the monospace stack at 12px. Action lines are at least 28px high with a trailing small button.

The project brief is an assistant-maintained project prompt stored with the conversation and kept when it is cleared. Its heading row has a small ghost 编辑 button, or 填写 when the brief is empty, with a Phosphor pencil. The brief shows as 12.5px pre-wrapped text with 1.6 line height. When it is empty, a `text-2` note with a sparkle explains that the assistant summarizes the project automatically and that the brief survives clearing. Editing replaces the text with an inline textarea: 120–320px high, 8px 10px padding, `control` corners, a `line` border that turns `field-focus` on focus, and a 2000-character limit. Beneath it, an 11.5px `text-3` tabular character count sits at the left, with small 取消 and primary 保存 buttons at the right. Cmd/Ctrl+Enter saves and Escape cancels without closing the panel.

The assistant permissions section is the panel's only settings group. Its three rows have a 12.5px label at the left and a small shared Select at a fixed 112px at the right, 6px apart: 项目外文件 (不可访问 / 只读 / 读写, default 只读), 推送到设备 (每次询问 / 直接运行 / 禁止, default 每次询问) and 联网 (允许 / 禁止, default 允许). Beneath them, a 12px `text-2` note with a Phosphor lock explains the scope. Depending on the file setting, it says that only the project is reachable, that outside means the home folder, or that hidden configuration and Library stay read-only. In every case it adds that credential folders and GizMoth's own data are never reachable. Changes apply to the project immediately and persist. The panel shows no version, and push status messages omit it too; `app.json` and the package manifest still carry a SemVer version (0.1.0 for new projects) because the firmware package format includes it. Escape and the close button dismiss it and return focus to the toggle; while floating, an outside click also dismisses it.

The workspace has no bottom status bar, including while code is visible, and idle shows no saved label. Saving, busy operations, results and errors render through `PageHeader.actions` in the shared titlebar. The non-interactive indicator is 28px high with 11.5px `text-2` text and a 13px spinner for progress, check for a settled result or warning for an error. Its text truncates within a maximum of the smaller of 32vw and 400px; the full message remains in its title. Progress disappears when the operation or save settles. Non-error results clear after six seconds; errors use the error text color and remain until the next operation or result replaces them. The authoring panes retain the full available height without a reserved footer line.

### General settings

General is a top-aligned column at the shared 1040px maximum. One theme-aware list card holds three single-line rows: theme, language and continuing to run after closing the window. Rows have a 52px minimum height (52px in the reviewed layouts), aligned leading labels and trailing controls, 10px 16px insets and soft internal separators. Groups are 24px apart; the search heading sits quietly outside its card with an 8px gap and 16px horizontal inset. Shared small controls retain their 28px geometry; preference selects, search-provider select and key input share a 192px width, reducing to 128px at viewport widths up to 600px. Narrow rows use 12px side padding and a 10px label/control gap. Labels truncate visually while control accessible names and label titles retain their full text.

**The On-Demand Settings Help Rule.** Explanations belong in the focusable Phosphor Info help control beside each label. Hover or focus shows its associated tooltip; Escape hides it while retaining focus. A later focus or pointer entry makes it available again. Do not repeat explanations beneath every setting.

Theme, language and the native background-window switch share one saving guard, disabling those controls while a preference write is pending. The switch loads actual native preferences, stays disabled until available, and updates its checked state only after a successful save; failures show an alert. Existing preference persistence and cross-window theme/language synchronization remain in place.

The search group contains a provider row (None / Brave Search / Tavily) and a conditional API Key password row. Provider credentials remain in the OS credential store; a stored key may be retained by leaving its field blank, or replaced by entering a key. The compact footer exists only for pending changes, a saved provider, busy work or feedback. Save appears only for changes or an in-flight save and is disabled until configuration is valid; a saved provider exposes Turn off and delete key. Status and error feedback stay beside these actions. The default unused state has no empty Save strip. At narrow widths, footer actions wrap and a nonempty status takes its own line.

General omits the static Local data section: it has no setting or data-management action, and the user explicitly requested its removal. General has no version or about footer; Settings > About owns that information.

### About and shared updates

Settings > About owns the app identity, MIT License, version imported from `package.json`, and manual Check for updates. A compact identity precedes a neutral list with Current version and Software update rows. The update row reports checking, current, available or failed status; errors remain retryable and are never described as up to date. At narrow widths it wraps so the status and action remain readable. A quiet note explains the existing startup, hourly and network-recovery checks.

`UpdatesProvider` wraps the main app only, so About and the titlebar share one controller and request guard across navigation. Checks are read-only; a successful check without a release keeps the titlebar button hidden, while a newer release shows Update. Download, signature verification, save-before-install, installation/restart and retry retain the existing controller behavior. Work in progress shows percentage or stage feedback; successful installation/restart hides the Update button rather than leaving an installed badge. The completed resource is released, and About returns to current status with its normal Check for updates action. Browser demo fixtures include current, check failure and a later update found on the next check; they never install or restart the machine.

### Settings navigation and Open Source

The bottom-left Settings row is an expandable navigation group, matching the existing indented project-category navigation. Its children are General, Open Source and About. Only the current leaf carries `aria-current`; collapsing the group retains the selected page. Selecting Settings from a collapsed rail reveals the group. Narrow windows reuse the nested-sidebar overlay, closing it after a page choice.

Open Source lists exactly the user-selected GizOS, GizClaw and GizMoth projects in that order. A single theme-aware narrow list holds project names (13px/400), repository slugs (11.5px), concise localized descriptions (12.5px/20px) and separate trailing Star links. The main repository link and its Star action are sibling anchors; each action opens that row's repository. Rows use 16px padding, reduced to 14px/12px at narrow widths. The page scroller fills the work area while its content remains centered at the 712px narrow cap. No live repository API calls or fabricated star counts are needed. These per-project Star links remain; the separate Star GizMoth sidebar entry is removed. The main-window opener is scoped to these three repository URLs; simulator permissions are unchanged. Browser previews retain standard new-tab links.

### GizOS documentation reader

The sidebar's 文档 entry uses the Phosphor BookOpen icon and opens an offline Read surface. The source is the committed Markdown in `docs/gizos/`, shared byte-for-byte with the built-in documentation MCP and installed as local files. The documentation root fills the main workspace with a 224px topic navigation column and a flexible reader extending to the far-right edge. The reader owns the scrollbar at that edge; only its reading column is centered and capped at 72ch. The grid has no outer width cap and does not inherit the ordinary 1040px/712px page-content limits. Navigation starts with a compact GizOS identity, the shared 32px SearchInput and a full-width development-area select. The reader follows the App language preference, with no documentation-specific language picker. When the App language is System, Chinese locales use Chinese and other locales use English. Nine areas cover the platform, with Lua App development one section. The default is the GizOS overview. Topic buttons use 12.5px/20px text, 32px minimum height, 6px corners and the neutral hover/selected fill. The topic list contains curated bilingual guides, API explanations and application examples only; there is no source/reference disclosure or archive list. Topics are searchable across the platform using Chinese concepts or API names. The area and selected topic survive navigation. App language changes update the translated guide content and controls in place; obsolete documentation-specific language overrides are removed. Source citations sit at document footers.

The reading area uses 24px/32px top/side padding, a small 本机 Markdown toolbar with a copy icon, and 14px/24.5px body text. Main headings are 24px/400, second-level headings 18px/400 and third-level headings 15px/400. Source paths and immutable revisions remain visible. Code blocks use the white card tone, a `line` border, 8px corners, a 32px language/copy header and 12.5px monospace text at 1.7 line height. Code and tables scroll inside their own bounds, have keyboard focus targets, and show the shared focus outline. Markdown rendering skips raw HTML; code is text, never executed. At widths up to 760px, search remains above the article, matching topics appear only during search, and a shared document Select replaces the persistent topic list. Body text becomes 13px and the main heading 21px. No new palette, shadows or global tokens are introduced.

Sources: `src/pages/Documentation.tsx` / `.css`, `src/gizos-docs/` and `.impeccable/surfaces/src-pages-documentation-tsx.md`. Browser evidence is `.impeccable/review/gizos-docs-example-{1200,1000,390}.png` and `gizos-docs-overview-1200.png`; native integration reads the same docs with web/home permissions disabled before writing and validating Lua. The finish review identified missing keyboard access to overflowing code; code and table regions now accept focus and arrow-key scrolling. This does not claim successful execution of a game on physical hardware.

### Assistant model picker

Message actions sit in a 24px row under each prompt, right-aligned, and under the last message of each reply turn, left-aligned and flush with the text. A muted 11.5px relative time (full date as its title) leads, followed by 24px icon buttons with 6px corners: Copy and ArrowCounterClockwise (rewind: confirmed inline under the prompt with 12px `text-2` text, a 24px 取消 and a red 24px 回退 that takes focus; Esc cancels; the text warns that changed files stay changed) for prompts; Copy and, on the latest reply only, ArrowClockwise (regenerate) for replies. Copy briefly swaps to a check. The row fades in over 120ms on hover or focus-within; the latest reply and touch screens keep it visible.

**The Changeable-Only Footer Rule.** Only controls the user can change sit under the input. The footer holds the model trigger and, only once the provider has reported usage, a context-usage ring. Fixed facts such as the provider name, and per-project settings such as assistant permissions, belong in the project information panel, never in the footer.

The footer aligns right below the input and wraps at narrow widths. The Claude Code-style trigger uses the shared small ghost button: the model name in primary text, a muted 思考 {level} suffix in `text-2` when the model supports adjustable thinking, and a `text-2` Phosphor caret. The name truncates first within a 420px maximum, and the trigger carries the requested level in a `data-thinking` attribute. The context ring is a 16px SVG ring (2px stroke, `line` track, `text-2` value) inside a 28px button with 6px corners and the neutral hover fill while hovered or open. It turns `warn` at 75% and `bad` at 90% of the context window. It opens the `agent-context-popover`, a native auto popover above the ring, right-aligned to it and dismissed on resize. The popover has a 12.5px 上下文窗口 heading (400) with used / total (percent) in tabular `text-2`, then a 4px `line` meter bar with 2px corners whose fill follows the ring's colors. Rows for input, cache read, cache write and output from the latest reported usage follow; rows with no tokens are omitted. An 11.5px `text-2` note closes it. From 75%, the note suggests clearing, and a small default 清空对话… button opens the same confirmation. The trigger's native auto popover is anchored above or below it, constrained to viewport width minus 24px and available height, with a 420px height ceiling. The `agent-model-popover` tokens own its surface, width, padding and radius. A shared search field precedes one flat scrolling list without provider groups. Each option is a two-line row: name · provider (provider in `text-2`) at 12.5px/18px, then 11.5px/16px `text-2` metadata naming the context window and whether thinking is supported; rows use 6px 8px padding and 6px corners. The current model carries a check and weight 400; keyboard/pointer activity uses the neutral hover fill. Below a `line` hairline, a 32px 思考程度 row shows the current level in `text-2` with a caret, or 不支持 and a disabled state when the model declares no adjustable levels. It opens the `agent-model-effort-menu`: a radio submenu of 30px items whose checked level has a check and weight 400. The submenu flies out 10px beside the popover on whichever side has room, or opens above the effort row, over the list, when neither side does.

Opening focuses search. Arrow keys, Home/End and Enter navigate and select models. ArrowRight or Enter on the effort row opens the submenu focused on the checked level; arrows and Home/End move between levels, Enter selects, and ArrowLeft or Escape returns to the effort row. Elsewhere Escape dismisses and restores trigger focus, while Tab leaves normally. Outside click and resize dismiss the popover. Composition keystrokes are guarded against selection, navigation, dismissal and query clearing. Synthetic-event evidence does not establish OS IME compatibility. Thinking levels follow the SDK's supported levels for the selected model; exact known model mappings are retained, unavailable levels are omitted and a non-reasoning model exposes off only. The requested level is saved per project and applied to the actual agent request. Agent activity distinguishes waiting, thinking, text, tool execution, history saving and stopping. The final UI update occurs after the SDK prompt fully settles, not merely on its earlier agent_end event. Consecutive tool calls collapse into one muted 12px `text-2` disclosure whose single truncating line summarizes them (for example 列出文件，写入 breathing_light.lua，检查程序包), followed by a caret that rotates when open; the line darkens to primary text on hover. While a call runs, the line names it beside a 13px spinner. Failures add a warning glyph and a red N 项失败 count, and calls that never ran add N 项未执行. Expanded, the individual rows sit behind a `line` left rule and keep their running, queued, success, error or skipped state. Only currently executing tools spin; queued calls use a clock and historical calls without results are marked not executed. A group's open state is keyed by its first tool-call id, so it survives streaming updates. Completed reasoning is collapsible. Each project's conversation lives in a background session: leaving the project page detaches the view while the run continues, every finished message is saved, and returning reattaches to the live state. The unsent draft, text and images, is kept per project. Automatic scrolling follows new output only while the user remains near the end.

### Chips / Tags

Provider tags use small proportional text on the sidebar tone. Model chips use the monospace role, a pale control background, soft border, and truncated single-line content. Editable chips add an inline removal control; the static preview represents the non-editable variant.

### Cards / Containers

Cards provide background, border, and radius but no universal padding. The provider directory groups all entries in one list card, following the user's magpie reference. Provider rows add 14px 16px padding and soft internal separators, with shared outer corners and no gap between rows. Clicking the identity opens provider editing; key-status labels and the separate pencil action are omitted. The default row retains its endpoint, refresh action and quiet model-count button. Clicking the count opens editable model management with wrapping, removable model-ID tags inside a textarea-like bordered field, following the user's preference for the original tag editor. Tooltips preserve full names and IDs; long tag collections scroll internally. API retrieval, manual addition, cancel and save remain available. Cancel preserves the saved list, and model-only saves preserve provider connection settings. Narrow layouts move row actions below the identity. Generic list rows have a 56px minimum height, 10px 16px padding, and soft internal separators; project and simulator-manager rows use the denser 48px role within a shared white card. Clickable rows receive the row-hover fill. Generic dialogs use 18px 20px 20px padding and native modal behavior; the application action dialog uses its scoped inset. Optional shared heading and initial-focus props let it focus the identity without changing the default generic heading or focus behavior.

### Device directory

The paired-device list follows the user-supplied magpie reference's compact desktop density. One short description and a compact filter/search row lead directly into the theme-aware list surface; the titlebar already supplies the page title, so there is no repeated large heading or table header. Each 36px tile uses the model-specific glyph exposed by @gizclaw/models-player, rendered at 28px with a theme-colored mask and a connection dot. Unknown devices retain a CPU fallback. Icons are bundled with the SDK rather than requiring a 3D viewer or a remote model download. Identity is always device name above model · address, including narrow screens.

Device rows have a 64px minimum height and explicit keyboard focus. A 36px glyph tile carries a 10px connection dot; the name is 14px/400 above a 12px `text-2` model · address line. Online rows show only available network and battery readings at 12.5px tabular figures, right-aligned before a `text-3` chevron; offline and unchecked rows show a single state line, with last-online time when known. Below a 560px list container the same two identity lines stay visible; trailing readings and extra state text hide instead of replacing the subtitle. Connection state remains visible through the icon dot and the full state, address and readings remain in the button's accessible label and device detail. Names and long addresses truncate to keep the row at 64px. The normal model/address example fits at 390px without truncation. The list surface does not shrink to clip devices; the page scrolls naturally. First-use and unmatched-filter states retain pairing or reset actions. A quiet Connect device text action at the upper left of the device list opens pairing; the visible-device count sits at the upper right, below the filter/search row. This toolbar remains available in the first-use state, and the sidebar, titlebar and empty state have no duplicate pairing action. In browser demos, the mode marker occupies the titlebar instead of a full content banner.

### Pairing dialog

The nearby-first dialog inherits the compact neutral shell and shared controls. `pair-dialog` records its width ceiling; actual width is constrained to viewport width minus 32px and height to viewport height minus 32px with scrolling. A short binding-mode hint leads to the nearby-device section with inline scan/stop. The white bordered results surface has a 144px minimum and 240px maximum height with internal scrolling. Single-line device rows have a 48px minimum height, truncated names, Phosphor icons and trailing action/status. The selected connecting row retains full opacity and uses the canvas fill while other busy rows dim. Keyboard focus uses the shared blue with an inset outline.

A divided secondary disclosure reveals a shared password field for credential links and an Add action. Scanning starts only on request. Stop, close and unmount invalidate late UI results; native BLE/backend behavior is unchanged. Loading, no results, browser limitations and failures remain explicit. The nearby heading identifies an actionable list; it is not a decorative eyebrow or a new heading convention.

### Device controls and status

The identity card's meta line pairs a connection dot with explicit status text; its strip shows battery and connectivity readings, and Device information holds the firmware version. Compact square controls show a glyph, name, and confirmed percentage. Activation opens a dark control panel with light or audio effects driven by the current setting. These effects are neither live audio analysis nor sensed light measurements. Audio steps by 1 and light controls by 10; pointer release commits the changed value, keyboard key-up and non-drag blur also commit, and pointer cancellation or interrupted capture restores the confirmed reading. Closing resets the draft without canceling a request already sent. A transparent native input preserves vertical slider semantics and visible keyboard focus. Offline and unread controls disable adjustment. The shared dialog dismisses only when both pointer press and release occur on its backdrop, followed by the click; internal gestures cannot accidentally dismiss it or click through. Focus returns to the opener. Busy destructive operations retain their dismissal guard, while control panels may close during an in-flight adjustment. Device removal is the identity card's red upper-right ghost action and requires confirmation.

Device information's key row includes a shared password `Input` followed only by an eye reveal/hide icon and a checkmark save icon, with a 2px gap. It has no extra information, warning or success glyphs and no Save text label. Errors open a text notice anchored above the field, with explicit retry or legacy-migration actions and a text Dismiss action. Migration hints open only when the key field is focused or clicked, rather than covering detail values on initial expansion. Dismiss, Escape, clicks outside the editor or focus leaving it (including a null next target or window focus loss) closes the notice without saving or migrating; focusing/clicking the field can reopen it. Mouse activation of notice actions retains field focus so migration and retry still run. Failed status reads keep the field focusable and read-only so Retry remains reachable after dismissal. The key starts hidden; a stored key displays eight password dots without loading the secret until Reveal is requested. Loading, missing-key, error and save feedback remain distinct. When no local key exists, the user can enter one or explicitly migrate an old OS-store key. Normal key reads, reveal, replacement and device removal use private app-data files without accessing Keychain; migration alone accesses the old device credential store. Provider-key storage remains separate and continues to use the OS credential store.

### Device identity card

The card at the top of device detail applies the user's Apple Home accessory direction inside the pinned magpie shell. The header rules it replaced were removed from `src/pages/DeviceDetail.css`; the device-key editor rules remain there.

- **Model tile:** `device-identity-tile`, 112×112px with overflow hidden, holding the models-player view; `DeviceModelView`'s stage is a plain div. Offline status applies `grayscale(1)` and 0.5 opacity to the model with a 220ms exponential ease-out (`cubic-bezier(0.16, 1, 0.3, 1)`) that reduced motion disables. Unknown status keeps full color. The tile holds the loading/failed/unrecognized note as icon plus text; at narrow widths only the icon remains visible.
- **Head:** the name uses `headline` on one line with ellipsis and a full-name title, beside an inline pencil. The `device-identity-meta` line (12px/18px) holds the model name, a 6px status dot with 在线/离线/未检查 and, for offline devices with a last-seen time, "N 分钟前在线" after a "·" separator. The separator is clipped when that item wraps to a new line.
- **Removal:** `device-remove`, a 28px ghost button at the upper right with hover `device-remove-hover`. Its accessible name equals the visible "移除设备"; the title "移除这台设备" is a tooltip. It opens the existing confirmation.
- **Strip:** a `line-soft` top hairline with 12px top padding. Readings run battery, Wi-Fi, cellular, Bluetooth, microphone. Glyphs are 16px Phosphor in `text-2`; values are 13px/400 with tabular figures. The battery glyph follows the level (≥85 full, ≥60 high, ≥35 medium, >10 low, otherwise empty), with a charging glyph while charging. Low battery (≤20% and not charging) uses the `device-vital-low` amber, matching the device list. Connectivity glyphs show on/off state only, never signal strength: WifiHigh/WifiSlash, CellTower/CellSignalSlash, BluetoothConnected/Bluetooth/BluetoothSlash, Microphone/MicrophoneSlash. Unread values are muted "未读取".
- **Strip states:** offline shows one 12px sentence naming the declared readings, for example "上线后显示电池、Wi-Fi 和蜂窝网络状态" (Latin words unbroken, balanced wrapping). Online with no readings shows "设备未提供电量和网络状态". Unknown shows "刷新后显示设备状态", or "正在读取设备状态…" with a spinner whenever a refresh is running while the status is still unknown.
- **Disclosure:** only a 16px caret at the strip's right end, without a text label, Info glyph, padding, border, background or shadow. Its button bounds match the arrow, and hover and expanded states use primary text. The accessible name and keyboard focus ring remain. It sets `aria-expanded`, and `aria-controls` only while open. The caret rotates 180° when expanded (220ms, none under reduced motion).
- **Details:** a definition list directly below the strip, with `line-soft` separators. The status strip and each information row are 44px high and stay on one line. Detail labels use a 128px column with a 16px gap, 12px `text-2` labels and 12.5px values. Address and firmware values truncate with full-value tooltips. Below 360px console width, labels use an 88px column and 8px gap, truncating with full-label titles. The key editor uses only two shared 28px icon controls, reveal and save, with a 2px gap at every width. Storage explanations use the input title; error, retry and migration notices remain outside the icon row and do not increase its height.
- **Name editing:** a persistent Phosphor pencil beside the single-line device name opens an inline 28px field with save and cancel icons. Enter saves, Escape cancels and restores pencil focus; IME composition does not submit. Names are trimmed and limited to 40 Unicode characters. Saves update the local device metadata, device directory, titlebar and tray without requiring the device to be online or accessing its API key. Failed writes preserve the editable draft and show an error.

Reference authority is the user's iPhone screenshot, [Apple Home](https://www.apple.com/home-app/), and [the iPhone accessory-control guide](https://support.apple.com/en-ie/guide/iphone/iph0a717a8fd/ios). Apple imagery is reference-only. These references constrain this device surface rather than establishing a new global identity.

Applications follow the user's iPhone home-screen reference: independent icons and names below. Clicking opens a focused action dialog before any command runs. It offers device execution, a temporary install/run request from matching local source to a selected simulator profile, and confirmed removal. Device and simulator actions occupy separate sections with their availability and feedback beside the affected destination. Missing local source or profile selection disables the simulator action. Source loading, read failure with retry, and absence remain distinct; no target picker appears without matching local source. One matching project is plain source text; multiple matches use the shared in-app Select, initially preferring an exact installed-version match and then the latest update. A version mismatch is an inline warning. Profile selection uses the shared in-app picker without its redundant management shortcut. With no profiles, creation stays available even without matching local source: an inline name/model form creates an independent profile and selects it without leaving the dialog. The name starts as “我的模拟器” and the model defaults to the current device model when known. Cancel keeps the list unchanged; blank names disable submission, save failures retain the form for retry, and saving blocks duplicate submission and dialog dismissal. With missing source, successful creation shows a confirmation and the application Run action remains disabled. Profile loading and read failures remain separate from an empty list. Deletion stays in a separated footer and opens the existing confirmation. Initial focus lands on the app identity without a visible heading outline; buttons retain visible keyboard focus. Escape, guarded backdrop dismissal, busy protection and opener-focus restoration use the shared Dialog behavior. Full supplied metadata remains in tooltips. Local Phosphor glyphs are fallback artwork; no ratings, prices, popularity, or invented descriptions are added. Device results remain beside the affected icon and also appear inside the device section, with errors explicitly inviting retry. Simulator request success and launch failures stay in the simulator section. Success copy confirms a request or demo simulation rather than asserting live execution. Search, loading, failed reads, empty libraries, and offline state remain distinct. The device-detail styles live in `src/pages/DeviceDetail.css`, with the superseded shared stylesheet block removed. The action dialog is implemented in `src/pages/ApplicationActions.tsx` / `.css` and integrated through `DevicePrograms.tsx`. Recorded synthetic browser checks in `.impeccable/review/application-dialog-results.json` and `check-application-dialog.mjs` cover 1200/390/340px widths, source/profile states, version preference, offline/read failure/retry, long metadata, short-height scrolling, focus and dismissal guards, device dispatch, deletion confirmation, and independent popup failure/retry. The supplied review disposition is ship with no material fixes after seven synthetic screenshots. Reported validation includes the frontend build, 53 existing frontend tests and standalone macOS build/relaunch; a generic native Dialog was opened and closed without pairing. These checks do not verify execution of this redesigned modal on physical hardware.

### Simulator manager and independent windows

The manager matches project discovery: a 1040px maximum width, one white card with thin separators, no table header, and single-line 48px rows. Its content toolbar places the quiet plus/text creation action at the left and the simulator count at the right, using the shared list-toolbar styles also used by Projects, Providers and Devices. The titlebar and empty state have no duplicate creation action. Each row places a Phosphor monitor, name, fixed model, firmware status and open/rename/delete actions on one line. At narrow widths, the status and Open label hide while icon actions remain available. Model selection appears only in the creation form; editing changes the name. The native update command also rejects model changes. Each UUID owns independent native website storage for installed apps, firmware files and device preferences. Normal close saves the device; confirmed deletion removes its entire store and preserves local projects. Native `simulators.json` stores only ID, name, model, firmware choice and view; browser previews use localStorage. Profiles have no permanent project association or source payload.

The independent window follows the pinned magpie/Apple Simulator direction: a flexible device stage with compact chrome and on-demand configuration. Its actual device model remains the pre-existing models-player asset; this redesign adds no shipping raster artwork.

**The On-Demand Configuration Rule.** Simulator firmware settings and keyboard help use native auto popovers, dismissed by outside click, Escape or their close action, without a modal veil. Keep the device stage visible around them.

Both popovers are at most 312px wide, constrained to viewport width minus 24px, with 16px padding and internal scrolling when needed. Firmware settings open below the toolbar; help opens above the controls/footer. Firmware settings retain a compatible-firmware select, distinct installed label, reload and explicit install/reinstall actions. The device model is fixed at creation and displayed as plain metadata. Successful firmware installation persists the choice; reopening restores it without replacing the device store. Power uses the physical device key. The provider remains the compatibility authority; raw ROM images are not compatible Web firmware.

The pixelated square framebuffer is sized to the smaller of 80% of stage width, 80% of stage height and 420px. Its black surface carries an explicit screen-off message when firmware is not running. Loading and unavailable states occupy the stage. Eight shared hardware keys use a compact four-column grid: Back, Left, Right, OK, Power, Light core and Volume down/up. English labels become icon-only at widths up to 600px to keep the longer volume names inside their columns; Chinese labels retain the existing 440px breakpoint. Accessible names and shortcut tooltips remain available. Pointer holds, scoped keyboard semantics and pressed states remain available while an application waits for setup or an idle device. Model/screen switching changes only presentation and preserves the runtime.

**The Visible Runtime State Rule.** Keep runtime state in the footer. Place Delete simulator at the right of the titlebar as red text with no icon, filled background or visible border. The window has no model picker or separate Stop action. Availability follows actual Web Crypto, cross-origin isolation and SharedArrayBuffer capabilities; help and firmware settings retain explicit explanations. Ordinary native Close and orderly application Quit wait for firmware shutdown before destroying the window. Deletion clearly states that installed apps, firmware data and preferences will be removed, while local projects remain.

Incoming temporary application requests and runtime errors use bounded feedback. While installing or waiting for an idle device, feedback occupies normal layout space so it does not cover first-boot setup; all hardware keys remain usable. The host waits for the actual command interface, validates Lua tool availability, and retries only an explicitly rejected BUSY/resource-exhausted launch. Timeouts remain ambiguous and are not replayed automatically. Cancel retains an acknowledged installation; Retry launch reuses it unless a definitive missing-app response invalidates the cache, in which case the next explicit retry reinstalls. Completed feedback uses a bounded overlay above the two-row controls. Dismiss hides presentation without canceling or rebuilding the runtime. There is no project picker, permanent project association or ongoing source synchronization. Profile errors remain alerts beneath the titlebar.

### Page companion

The user explicitly requested a jumping character from `idy/peetty`. `src/PageCompanion.tsx` loads the pinned local SDK after the main shell and mounts its original Miru figure at 92px. The character can walk, perch, jump and be dragged; its shadow-root interaction menu inherits the existing light/dark tokens and regular Monda typography through `PageCompanion.css`. Menu items wrap within narrow viewports. Keyboard users focus the figure and press Enter/Space to open actions; Escape closes the menu and returns focus. The titlebar is excluded from perching. The main window owns one instance across page navigation; simulators omit it. Hidden documents stop the frame loop, while the SDK's reduced-motion behavior removes autonomous spatial movement. Settings adds a regular 52px Page companion switch using the existing row and help-control pattern. Its origin-local preference also records the SDK's Hide action. The package and original MIT license are bundled locally; source provenance is in `vendor/README.md`, and the distributed license is in `public/licenses/peetty-MIT.txt`.

### Motion

Loading indicators spin with a one-second linear infinite animation; switch thumbs translate over 0.15 seconds. The chat/code divider's highlight fades in over 120ms ease-out after an 80ms delay, and tool-group carets rotate over 120ms ease-out; reduced motion removes both transitions. Application icons lift 3px on hover and scale to 0.94 while pressed, using a 160ms exponential ease-out transition. A device-control panel expands from its trigger over 340ms with exponential ease-out; its backdrop dims and blurs over 220ms. Screen emission, lamp spill and directional sound-wave extent respond over 100–120ms when the user changes the level, with no idle loop. Reduced-motion preferences remove the expansion, spatial transitions, backdrop blur, and hover movement while preserving immediate level feedback. Exit is immediate so dismissal never waits for animation. No page entrance animation or management-mode shaking is added.

Simulator reduced-motion preferences disable all animations and transitions in the window and its popovers, including loading animation, while preserving immediate state feedback.
