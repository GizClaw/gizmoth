---
version: 1
slug: "src-app-tsx"
primary_target: "src/App.tsx"
related_targets: ["src/pages/Workspace.tsx","src/pages/Projects.tsx"]
---

# Desktop shell and development workspace

Mode: Operate. Preserve device management and the Lua editing/agent workflow. The user delegated implementation and supplied magpie's desktop UI as the structural reference.

## Direction contract

THESIS: One desktop workspace connects devices, editable projects, and a separately openable simulator window. The frame stays compact enough to leave room for the task.

OWN-WORLD: Retain GizMoth's system typography, neutral light and dark surfaces, thin separators, and compact rounded controls. Settings owns System/Light/Dark themes and System/简体中文/English App languages; both default to System and synchronize across windows without resetting projects or simulators. Documentation follows the App language. Follow the supplied magpie reference for titlebar identity, context, and utility placement rather than introducing a new decorative system.

STORY: Connect a device, choose a project, edit or ask pi-agent, inspect the device preview, then deploy intentionally. Distinguish Lua, Giz firmware, and native firmware without implying an unavailable build path works.

FIRST VIEWPORT: A 52px titlebar carries the mark, app name, breadcrumb, version, and update entry. A collapsible primary sidebar holds Devices and Projects. Projects expands into three indented categories: Lua applications, Giz firmware, and native firmware (the existing internal `rom` kind). The parent shows all projects; each child filters discovery. The selected project's kind remains highlighted in its workspace. This is category navigation, not an expansion of project contents. Within the workspace, a compact toolbar carries file selection, code/pi-agent selection, a simulator-window launcher, and file/project actions. There is no extra sidebar or main-area mode tab strip; both authoring modes remain mounted to retain state. The simulator opens as a separate reusable project window, leaving the full workspace available for code or pi-agent. It receives current unsaved source through a project-specific channel and falls back to saved project reads after navigation. Preview controls preserve actual Left/Right/OK/Back button semantics and release on focus loss. Loading and unsupported-runtime states are explicit.

FORM: User-pinned desktop tool composition from the supplied magpie screenshot, applied as a refinement of the existing workspace. Provenance is the user's explicit request to place identity, information, and updates in the titlebar, alongside Devices/Projects navigation, and to implement directly without Live selection. A generated concept seed is not applicable to this precisely specified refinement; no roll or generated reference is claimed. Signature interaction: move from editing to a focus-scoped independent simulator without losing project state. Motion is limited to feedback and respects reduced motion.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
