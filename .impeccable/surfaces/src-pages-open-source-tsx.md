# Settings navigation and Open Source

Mode: Operate / Read. Preserve GizMoth's compact neutral desktop shell, system typography, shared themes and App language preferences.

The user supplied a nested Settings sidebar reference. The bottom-left Settings row expands into General, Open Source and an external Star GizMoth link, using an indented hairline and existing selected/hover fills. General retains the current appearance/language, window, search and data settings. Only the active leaf has aria-current. Collapsed rails expand before showing children; narrow windows use the existing sidebar overlay and close it after selecting a page.

Open Source shows exactly the three user-selected public projects, in order: GizOS, GizClaw and GizMoth. Do not expand the list to other organization projects. Use a centered narrow list with a short localized description for each project. Each row has a project name, repository slug, short localized description and a separate trailing Star link with a star glyph. Repository and Star links are sibling anchors pointing to that row's own repository. The sidebar retains Star GizMoth. All links are actual repository URLs and never star on the user's behalf. No live star counts or remote API calls are needed to read this page. External links use the system browser in the desktop app and ordinary new tabs in browser previews, retaining failure feedback in the app.

Use a single theme-aware list surface, 16px row padding (14px/12px on narrow screens), 13px/600 names, 11.5px slugs and 12.5px/20px descriptions. Content may wrap, but the scroller fills the main workspace and only the reading column is capped. Preserve keyboard navigation, focus visibility and save-before-leave behavior.

Verify submenu expansion/collapse and selection, navigation from projects and the tray, collapsed rail and mobile overlay, long descriptions, Chinese/English, light/dark, external target URLs and opener failures. Native opener capability is main-only and scoped to the curated GitHub repositories; simulator permissions are unchanged.
