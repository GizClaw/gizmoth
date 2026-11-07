# Shared in-app Select

Mode: Operate. The user explicitly rejected system-native dropdown menus and asked to extract reusable page components with impeccable extract. Existing Button, Input, IconButton and SearchInput remain the design-system foundation.

Direction: replace the shared native select with one controlled select-only combobox. Options are rendered HTML in the WebView, with the existing neutral menu surface, 32px rows, Phosphor check/caret, shared 28/32/40px control sizes and default/quiet variants. Native Popover may supply top-layer positioning; it must not supply OS menu artwork. No hidden native select or fake native change events. Callers provide options and onValueChange.

Coverage: section/topic selectors in documentation, project permissions, editor language, provider API, web search service, simulator model/target/firmware and application source selection. Reuse menu radius/padding/shadow tokens in action menus and model picker surfaces without conflating their distinct interaction semantics.

Acceptance: pointer selection, arrow/Home/End navigation, Enter/Space, Escape cancellation and focus restoration, Tab exit, typeahead and IME guard. Disabled options/controls and asynchronous option changes remain safe. Popup stays inside the viewport, scrolls internally and works inside native modal dialogs and parent popovers. Review 1200/1000/390px and a short viewport. Native tests must drive actual HTML option clicks instead of modifying a native select.
