---
version: 1
slug: "src-pages-settings-tsx"
primary_target: "src/pages/Settings.tsx"
related_targets: ["src/pages/Settings.css","src/pages/SettingsRow.tsx","src/pages/WebSearchSettings.tsx","src/pages/SettingsNavigation.tsx","src/pages/About.tsx","src/pages/About.css","src/updates.tsx","src/UpdateButton.tsx"]
---

# General settings

## Permissions extension (0.3.9)

Mode: Operate. Settings › Permissions extends the incumbent list system. The first viewport has one compact introduction, a system-permission list (notifications, Bluetooth, local network), then serial notification preferences and a test action. Status and an explicit request/settings button share each row; explanations use existing focusable help controls. Narrow rows place their status and action beneath the label. Requests run only from explicit clicks; returning from system settings refreshes the displayed state. Native notification observation continues outside the active page. Local-network access remains honestly system-managed when authorization cannot be queried. Existing palette, typography, controls and reduced-motion behavior apply. No generated images or visual-system changes are needed.

Mode: Operate. The user supplied Magpie's single-line settings as the structural reference and requested direct implementation.

## Direction contract

THESIS: General is a quiet, aligned list of preferences. Every option occupies one row; explanations appear on demand rather than underneath every label.

OWN-WORLD: Preserve GizMoth's neutral light/dark palettes, thin separators, 12px list corners, Phosphor icons and shared 28px controls. Use the user's verified website families: Montserrat for headings/brand, Monda for ordinary UI, and Lato for small text, all at weight 400 without synthetic bold. Bundle the unchanged WOFF2 files with their SIL OFL 1.1 licenses; Chinese text uses the existing system fallbacks.

STORY: Choose an appearance and language, decide whether closing the window keeps the app running, configure web search, and open About for the version and manual update check. Settings navigation contains General, Open Source and About; remove the separate Star GizMoth sidebar link. Keep the individual project links in Open Source.

FIRST VIEWPORT: A centered column up to 1040px starts beneath the existing titlebar. A three-row preference list leads; a compact search group follows, revealing its key field and save action only when relevant. The user explicitly rejected the non-actionable Local data section; omit it. General has no version footer. About shows the app identity, current package version and a shared update-status/check row; the titlebar shows Update only after a newer version is found. Rows are 52px high, with labels left and controls right. Keep labels on one line at desktop sizes, preserve complete accessible names and supply descriptions through focusable help controls. Narrow widths keep controls usable without horizontal page scrolling.

FORM: seed: none (user-pinned precise refinement; no roll). The user's Magpie Settings screenshot supplied on 2026-11-08 is the composition authority and quality bar. New-work's precise-request exception applies: shape this surface directly without a concept-seed or options round. No generated seed or catalog card is claimed. Signature interaction: progressive disclosure keeps search credentials and pending changes beside the affected option. Motion is limited to existing control feedback and respects reduced motion.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

Both theme and language retain System defaults, persistence and cross-window synchronization. Documentation follows the app language. Preserve project/editor/chat/simulator state when preferences change. Native preference writes remain main-only. Store provider/search keys in the OS credential store and device keys in private app data; never claim all device keys use Keychain.

Earlier General-only completion (2026-11-08): scoped finish-review disposition was ship. The reviewer scored documentation, user-pinned provenance and the requested Local data removal resolved. Final desktop, mobile and native captures are `settings-polish-final-light-zh-1024.png`, `settings-polish-final-mobile-light-zh.png` and `settings-polish-final-native-dark-en.png` under `.impeccable/review/`. The final rebuild and two native settings tests passed. No shipping raster imagery was added.

About and chrome follow-up (2026-11-08): the scoped finish review returned ship with no material fixes. About owns version/manual checks; the shared updater checks automatically, shows progress only while active and removes Update after completion/restart. Creation and pairing use content-level text actions, with neither project creation nor version in the titlebar and no persistent sidebar pairing row. Final validation passed 94 frontend tests and three rebuilt native settings tests. Current captures include `about-final-dark-available.png`, `about-final-mobile-dark.png`, `about-final-complete-hidden.png`, `about-native.png` and `projects-content-create.png` in `.impeccable/review/`. Browser updates were simulated; native tests intentionally deny updater permission, and real signed installation was not performed.

Permissions completion/verification (0.3.9): supplied finish-review disposition is **ship**, with no fixes. Documentation checked against `reference/document.md`, PRODUCT.md Operating Context, current settings capabilities and Brand Commitments, DESIGN.md typography/layout/settings sections, and the sidecar's typography/settings records. Source evidence: `src/pages/Grants.tsx`, `Grants.css`, `SettingsNavigation.tsx`, inherited `Settings.css`/`SettingsRow.tsx`, and shared typography/theme aliases. Existing captures inspected: `.impeccable/review/grants-desktop-light-zh.jpg`, `.impeccable/review/grants-mobile-dark-en.jpg`, and `.impeccable/review/grants-native.png`. They evidence desktop inline status/actions, narrow wrapped permission rows, the notification switch/footer, and native unavailable/pending/system-managed states; browser captures explicitly identify simulated permissions. Source confirms focusable help, explicit request/test actions and focus/visibility refresh. This documentation pass ran no build, permission request or notification test; captures do not establish notification delivery or authorization success.

System preservation: ordinary Operate extension; DESIGN.md and `.impeccable/design.json` remain untouched, with no new tokens or global rules. The page reuses semantic colors, Magpie font aliases, 52px minimum settings rows, 24px group spacing and shared small controls; its 600px row wrapping is local to Permissions. Product behavior is already documented in README.md and docs/maintenance.md. Pre-existing drift is recorded, not canonized or repaired: the earlier OWN-WORLD font names, DESIGN.md's later legacy typography wording and sidecar typography descriptions still name Montserrat/Monda/Lato despite the current Magpie aliases; PRODUCT.md and DESIGN.md settings-child inventories also lag the implemented navigation. These are incumbent documentation discrepancies outside this narrow extension's write scope. No shipping raster assets were added.
