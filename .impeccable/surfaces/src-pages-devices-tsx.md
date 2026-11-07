---
version: 1
slug: "src-pages-devices-tsx"
primary_target: "src/pages/Devices.tsx"
related_targets: []
---

# Device list refinement

Mode: Operate. This is a refinement of the existing device list within GizMoth's light desktop system, requested directly by the user. Preserve pairing, device-detail navigation, credentials, and device controls.

## Direction contract

THESIS: Make each paired device identifiable and its availability readable in one pass. Replace the undersized tiles with an aligned list that gives names and model information enough room.

OWN-WORLD: Existing system fonts, neutral application canvas, white list surface, hairline dividers, compact controls, and restrained semantic status color. No new visual identity or decorative imagery.

STORY: Find a device by name, model, or address; optionally filter by online, offline, or unchecked status; open it to inspect and control it. Pairing starts from the persistent Connect device action in the sidebar or the first-use empty-state action. The titlebar has no duplicate pairing button.

FIRST VIEWPORT: Follow the user-supplied magpie reference's compact density. Start directly below the titlebar with one descriptive line, then small state filters and search. Omit a repeated page heading and table header. Use 64px device rows: a 36px device glyph carrying the connection dot, the name at 14px/600 with model · address at 12px beneath it, then a right-aligned readings group (network, battery at 12.5px tabular) and a bare chevron. Offline and unchecked devices replace readings with one state line (offline adds last-online time); never repeat the state or show "not read" placeholders. At every width, identity keeps the same two lines: device name above model · address. Below 560px of list width, hide the trailing readings and status text rather than replacing the subtitle; the connection dot remains and the complete state/readings stay in the accessible label and device detail. Names and long addresses truncate without adding a third line. Use model-specific bundled icons from @gizclaw/models-player, with a generic CPU fallback only for unknown devices. Use plain status text and a dot rather than filled badges. Narrow browsers wrap supplementary values while preserving every item. Keep the demonstration marker in the titlebar, not a content banner. Empty, unmatched, offline, and unknown states remain explicit.

FORM: Precisely scoped refinement directed by the user; no concept roll or generated seed is claimed. Keyboard focus and a restrained row hover are the interaction cues.

FINISH: Verify the shipped desktop size, minimum window, narrow browser fallback, filtering, pairing entry, device navigation, and unknown/offline data. Record only material additions to the existing design documentation.
