# Vendored packages

## Magpie Design System 0.4.3

`idy-magpie-design-system-0.4.3.tgz` is built without changes from the clean
[`idy/magpie-design-system` v0.4.3 source](https://github.com/idy/magpie-design-system/tree/46ba098239979506a4ccc14721aac044483c66bc)
(commit `46ba098239979506a4ccc14721aac044483c66bc`, package version `0.4.3`).
The MIT licenses and font notices are included in the package. A local archive
keeps app builds independent of GitHub package access and provides an offline
fallback for runtime style updates.

`src/preferences-init.ts` installs the bundled token and component styles from
`src/design-system.css` before the interface mounts. They occupy one replaceable
style element before the app's styles, without requiring CSS cascade-layer
support on older macOS WebViews. `src/theme.css` maps shared colors and control
radii to those tokens; compact layout, bundled font roles and regular-only type
remain app overrides. The Agents help group uses the actual component classes, with a
local adapter in `src/pages/Agents.css`. The document reset, app shell and library
JavaScript are not loaded. The token layer registers bundled fonts; the host's
existing local font declarations take precedence.

`src/design-system.ts` checks jsDelivr's `@latest/package.json` at startup,
every six hours and after network recovery. It downloads tokens and components
from one exact release before replacing the entire library stylesheet, including
removal of declarations that no longer exist. An already active release is not
downloaded again. All light, dark and system-theme blocks must be present. Only stable 0.x releases
at least as new as the bundled version are accepted; a new major version needs
an app-side compatibility update. Imported font registration is omitted from
remote CSS and icon URLs are pinned to that release. The last successful style
bundle is cached in localStorage and restored immediately on the next launch.
Network, validation and storage failures leave the bundled or current cached
styles usable. Both native windows allow this CDN path for styles and icons,
without allowing remote JavaScript or fonts.

Compatible CSS updates need no GizMoth release. Changes to component markup,
JavaScript behavior or the supported major version still require an app update.
CDN caching can delay the visibility of a new upstream tag.

To reproduce the archive from a checkout at that commit:

```bash
npm pack /path/to/magpie-design-system --pack-destination vendor --ignore-scripts
```


## Peetty 0.2.0

`idy-peetty-0.2.0.tgz` is built from
[`idy/peetty` v0.2.0](https://github.com/idy/peetty/tree/de14590632c91dc914d56a8bc9074fead615aa5d)
(commit `de14590632c91dc914d56a8bc9074fead615aa5d`), with the local
[`peetty-0.2.0.patch`](peetty-0.2.0.patch) applied. The upstream MIT license is
included in the archive and installed package. The patch includes its upstream
regression tests and changes two SDK behaviors:

- Oversized text uses a conservative bounding box, allowing scans past offscreen
  text while keeping visible text and subsequent controls clear.
- With reduced motion, mount and show place the figure directly on a checked
  perch. If a perch becomes invalid or an active field needs room, the figure
  hides and retries rather than falling or jumping. Explicit dragging retains
  its usual release behavior.

The upstream registry is GitHub Packages, which requires authentication even for
public packages. Keeping this release archive here lets normal `npm ci` and
release builds run without an additional registry credential. The dependency is
installed as `@idy/peetty`; application code imports its public SDK.

This tagged release provides the local figure, animation, perching and drag
behavior. It has no AI/chat implementation or runtime dependencies. Do not infer
its API from the upstream main branch, which added chat after this tag.

To reproduce this patched release, run the following from the GizMoth root with
Node.js 22.19 or later and Git installed. The temporary directory is separate
from the app checkout; `npm pack` runs the upstream build output, not local app
sources.

```sh
repo="$PWD"
source_dir="$(mktemp -d)/peetty"
git clone https://github.com/idy/peetty.git "$source_dir"
git -C "$source_dir" checkout de14590632c91dc914d56a8bc9074fead615aa5d
(cd "$source_dir" && patch -p1 < "$repo/vendor/peetty-0.2.0.patch")
npm --prefix "$source_dir" install --ignore-scripts
npm --prefix "$source_dir" run check
npm --prefix "$source_dir" test
npm --prefix "$source_dir" run build
npm pack "$source_dir" --pack-destination "$repo/vendor"
```

After rebuilding, update the archive's SHA-512 integrity in `package-lock.json`
and reinstall the package. For an upstream update, review the new source and
patch, record its version and full commit here, and recheck companion movement,
cleanup, language changes, reduced motion, and the desktop WebView.
