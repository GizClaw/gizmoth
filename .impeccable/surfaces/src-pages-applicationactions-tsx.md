---
version: 1
slug: "src-pages-applicationactions-tsx"
primary_target: "src/pages/ApplicationActions.tsx"
related_targets: ["src/pages/ApplicationActions.css","src/pages/DevicePrograms.tsx"]
---

# Installed application action dialog

Mode: Operate. Scope: the dialog opened from a device's app icon. Preserve the established light magpie/native shell, Phosphor icons, real app metadata, explicit run actions and confirmed device deletion.

## Direction contract

THESIS: Present one application identity and two clear execution destinations. Replace the detached simulator picker, full-width button stack and distant explanation.

OWN-WORLD: Inherit compact system typography, shared 28/32px controls, white surfaces, quiet separators and existing application-icon artwork. A 420px modal uses consistent 20px insets and 12–16px internal grouping.

STORY: Identify the app, run it on the current device, or choose a simulator when matching local source exists. Feedback and availability explanations remain beside their destination. Removal is a separate footer action with the existing confirmation.

FIRST VIEWPORT: A 48px app icon beside the app name and version, with close at the top right. A compact device row has a trailing Run button. A separate simulator section contains its own Run button, target/source controls only when usable, or the exact reason it cannot run. If no simulator exists, an inline creation entry remains available regardless of local source availability; it asks for a name and model and selects the saved independent profile. A bottom separator protects the destructive action. Avoid repeated app IDs when they equal the name.

FORM: User-directed local dialog redesign from the supplied screenshot and request to improve typography, spacing and layout. The product's pinned magpie/native convention remains; no new visual world, seed or generated comp is claimed. Initial focus lands on the app identity, while keyboard focus remains visible on actions. Outside click/Escape, busy protection and focus return retain shared Dialog behavior.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Acceptance

Inspect desktop and 390/340px viewports with and without local source, multiple matching projects, no/multiple simulator profiles, offline/busy/error/retry states and long metadata. Verify explicit device/simulator action dispatch and deletion confirmation using synthetic data only. No real device operation or credential access is needed.
