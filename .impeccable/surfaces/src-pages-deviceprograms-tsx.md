---
version: 1
slug: "src-pages-deviceprograms-tsx"
primary_target: "src/pages/DevicePrograms.tsx"
related_targets: ["src/pages/Devices.tsx", "src/pages/DeviceDetail.css", "src/pages/DeviceModelView.tsx"]
---

# Device overview and application home screen

Mode: Operate. The user pinned iPhone home-screen applications and Apple Home accessory controls, rejected repeated table rows and stretched controller cards, and supplied a home-screen screenshot. Keep GizMoth's magpie-style desktop shell and project-category navigation.

## Latest visual refinement requested

The latest user request is for the lamp emitter to have a rounded, approximately hemispherical raised crown with subtle diamond-like facets and a flat horizontal base. The current explicit polygon crown is still too angular; do not treat it as approved. Keep the upper cone and bottom-emitter layout. This refinement was relayed to the user's LiteLink app session; the rounded hemisphere has not yet been implemented in this worktree.

## Direction contract

THESIS: Use a compact Apple Home-style accessory overview above an iOS-like application grid. Full adjustment belongs in a focused accessory control, not a permanent giant fader beside every reading.

OWN-WORLD: A light grouped background, white accessory surfaces, compact system typography, warm light and cool audio icons. Independent rounded-square application icons sit above centered names. Device removal is red and belongs in the top identity region, per the user's explicit correction.

STORY: Read device identity/status, open an accessory to adjust its level, then click an application icon to choose device execution, simulator execution, or deletion. Search and project installation remain visible; app and device removal require confirmation. No invented on/off state, application artwork, ratings, or metadata.

FIRST VIEWPORT: The content region caps at 840px, centered in wide windows. A compact white identity surface groups model, name, connection, and icon-led battery/network readings. Address and firmware expand through Device information. Red removal remains at the upper right; narrow headers retain the action in that corner as a labeled icon button. Accessory controls are independent 112px square tiles, grouped with 12px gaps above the unchanged application grid. Below 420px console width, squares adapt to fit three across without stretching on wide screens. Clicking a tile opens a dark 360px control panel. The upper area is always a widening cone, anchored to a small emitter at the bottom: a tilted 2.5D screen, a horizontally tilted faceted gem, or an acoustic field with visible depth. Do not center the hardware object in place of the cone. The cone responds to the selected level. A transparent native vertical range input covers the visual control area; regular UI glyphs use Phosphor, while the emitter geometry is authored specifically for this control. A short expansion originates from the selected tile, and the background dims and blurs. Outside clicks and Escape dismiss the panel; internal interaction and drags that leave the panel must not dismiss it. Already-submitted changes may finish after the panel closes. Other dialogs retain their busy guard. Reduced motion removes the expansion and spatial transitions while preserving immediate level feedback.

FORM: Directly specified by the user; no concept lottery or generated comp is applicable. Reference authority: the user's iOS home-screen screenshot plus https://www.apple.com/home-app/ and https://support.apple.com/en-ie/guide/iphone/iph0a717a8fd/ios. Apple documents tapping an accessory name to reveal its controls. Downloaded Apple imagery is reference-only and is not shipped in the application.

FINISH: Verify desktop/minimum/ultrawide/narrow layouts, actual gesture and keyboard control, popup dismissal, red header removal and confirmation, application action dialogs, launch/error/retry, matching local project selection, independent simulator windows, search and installation entry. Preserve offline/loading/unread states. The local demo opens directly through `?demo=1&view=device`; real-device behavior is not proven by browser fixtures.
