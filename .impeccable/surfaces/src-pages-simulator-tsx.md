---
version: 1
slug: "src-pages-simulator-tsx"
primary_target: "src/pages/Simulator.tsx"
related_targets: ["src/pages/GamePreview.tsx","src/pages/Simulator.css"]
---

# Independent simulator window

Mode: Operate. Scope: the standalone Tauri simulator and its browser fallback. The manager keeps its existing 48px single-line list. Makers use this window beside their editor on a desktop; preserve the light native shell, Phosphor icons, shared controls, persistent preferences, and independent simulator identity.

## Direction contract

**THESIS:** The device is the workspace. Replace the stacked configuration form, oversized button tiles and persistent prose with a model-first native simulator window.

**OWN-WORLD:** Inherit GizMoth's pinned magpie/Apple Simulator conventions: system typography, cool light canvas, white chrome, 28/32px shared controls, subtle separators, restrained selected and runtime states.

**STORY:** Open a saved simulator, inspect its device, choose model or screen, and operate hardware buttons. Firmware selection and installation remain explicit inside an anchored settings popover. Requests from projects are temporary applications, never profile associations.

**FIRST VIEWPORT:** A 52px native overlay titlebar, a compact 40px toolbar, a flexible device stage, a single hardware-button strip and a slim status footer. The fixed device model is a plain label on the left; the model/screen switch and firmware action sit right. Delete simulator is a red text action at the right of the titlebar, without an icon, filled background or visible border; the footer contains runtime status only. Model selection belongs only to device creation. No normal-state vertical scrolling at 560×860 or 340×600.

**FORM:** User-pinned native simulator convention. Provenance: the user requested “应该类似 苹果的 simulator 一样 是一个单独的窗口”, a merged system titlebar, Phosphor icons, and an independent simulator unrelated to projects. The established shell follows their explicit magpie reference. No seed roll was used or claimed: this follows new-work.md's user-pinned-direction branch and mode-operate.md. Signature interaction: firmware controls appear on demand over the uninterrupted device stage and dismiss with outside click or Escape. View selection preserves hardware/runtime state. Motion is limited to brief control-state transitions and respects reduced motion.

**FINISH:** unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Acceptance

Verify 560×860, 340×600 and a wide desktop window, including long names, settings/help open, model loading/error, view switching, button holds and releases, native runtime limitations, and application request feedback. Preserve exact firmware choice, persisted preferences, close/reopen behavior, main-window-only native opening and the main application's restrictive CSP. The isolated native simulator may admit reviewed firmware under its own policy; execution claims require actual native verification. Do not introduce project selection. No shipping raster assets are added.
