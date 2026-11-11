---
version: 1
slug: "src-pages-agents-tsx"
primary_target: "src/pages/Agents.tsx"
related_targets: ["src/pages/Agents.css", "src/agents.ts"]
---

# External agent integration

Mode: Operate. The user chose Skill installation instead of global prompt injection. Extend the existing Agents list without changing the pinned compact neutral system, regular fonts, Phosphor icons, bilingual UI or themes.

## Direction contract

THESIS: Connect an existing coding agent to GizMoth by installing an inspectable, on-demand gizmoth Skill.

OWN-WORLD: Inherit existing canvas, card, hairline, text/status tokens and shared compact controls. One grouped list holds agent identities, SKILL.md paths, installation states and actions.

STORY: Preview or copy the generated SKILL.md, choose an agent, install or update the Skill, and uninstall only its managed file. Global instructions and other skills remain intact. Explain task-based loading and restart/reload discovery if needed. Keep device-identification limits clear.

FIRST VIEWPORT: Shared titlebar says Agents. A short description leads to the Skills list. Each row puts name and wrapping Skill path at left, state and actions at right. One status line beneath the list shows either a short operation result or the persistent installation hint, never duplicate restart guidance. Below, a Use and help group uses actual Magpie Design System section, group, row, hint and link-button components: SKILL.md disclosure followed by capability rows, with serial/service links aligned to their corresponding rows. The local token adapter preserves GizMoth's existing typography, regular-only weights and light/dark palettes; the shell and agent installation list keep their shared GizMoth components.

FORM: User-pinned conversion of the existing Nootify-inspired list, implemented directly as a narrow extension; no concept seed. Signature interaction: install a named agent's Skill and see its state change, then uninstall its managed file. Motion is immediate state feedback only.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
