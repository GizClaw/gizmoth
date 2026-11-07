---
version: 1
slug: "src-pages-deviceidentity-tsx"
primary_target: "src/pages/DeviceIdentity.tsx"
related_targets: ["src/pages/DeviceIdentity.css","src/pages/Devices.tsx"]
---

# Device identity card

Mode: Operate. Scope: the identity card at the top of device detail, implemented in `src/pages/DeviceIdentity.tsx`. Preserve the pinned light magpie/native shell, the user's iPhone/Apple Home device direction, Phosphor icons, actual device data, the red upper-right removal action with its confirmation, and the device-key editor's behavior.

## Direction contract

THESIS: One glance answers which device this is, whether it is reachable, and what it reports; device details expand where they are requested. Replace the three-column card whose two actions floated at opposite corners and whose offline state repeated “未读取”.

OWN-WORLD: System type, a white 20px-radius card on the #f2f2f7 device canvas, a #f2f2f7 model tile sized like the 112px control tiles, hairline separators, shared 28px ghost controls, plain status dot and text, amber for low battery and red for removal.

STORY: Recognize the device by its rendered model, name and model name; read its connection and last-seen time; scan battery and connectivity glyphs; open the address, firmware and API key in place; remove it only through the existing confirmation.

FIRST VIEWPORT: A 112px model tile spans the card height at left. The 22px/650 name and the model/status line top-align with the tile, with red “移除设备” at the upper right. A hairline strip aligned to the tile's bottom carries battery and connection glyphs, or one sentence naming the readings that return when an offline device reconnects; the “设备信息” disclosure sits at its right end. Expanded details form label/value rows directly below the strip. Offline models desaturate; unknown status keeps full color. Narrow containers use a 64px tile (52px at 300px or less), icon-only actions and a full-width strip.

FORM: User-directed local redesign from the supplied offline screenshot, inheriting the pinned magpie/native and Apple Home direction. No new visual world, seed or generated comp is claimed. Glyphs never imply data the device does not report, such as signal strength.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Acceptance

Inspect 1200, 390 and 340px widths in online, offline, low-battery, unknown and expanded states with synthetic demo data, plus the native macOS window with the user's real device collapsed. Verify disclosure semantics, keyboard order and focus, removal confirmation, reduced motion and absence of overflow. Native captures must not expose the device address or API key.
