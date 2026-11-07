# Pair dialog

Scope: Local device pairing modal. Mode: Operate. The user opens binding mode on a nearby GizOS device, scans, and selects a device; a credential link is an alternate path. Preserve existing bridge behavior and the user-pinned compact magpie/macOS shell.

## Direction contract

THESIS: Give nearby devices the space and attention needed for a deliberate selection; keep credential import available on demand.

OWN-WORLD: Inherit the established neutral surfaces, system typography, compact shared controls and restrained state colors.

STORY: A short binding-mode hint prepares the user, the scan action fills a bounded list, and a selected row communicates connection. Disclosure reveals the masked link field.

FIRST VIEWPORT: A compact modal heading and hint lead to a nearby-device heading with an inline scan or stop action. A quiet list or instructional empty state sits beneath it; the alternate import path closes the composition.

FORM: Local extension of the existing user-pinned magpie/macOS operative shell; no concept seed was run. Signature interaction: selecting a device turns its row into a connecting status while stop remains available.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

Finish evidence: browser captures cover desktop and narrow idle/results/scanning/binding/empty/error/link states. Independent review found no material pairing-layout issue. Native WebKit opening and explicit scan/manual-link controls were inspected without performing real Bluetooth pairing. DESIGN.md records the implemented component.
