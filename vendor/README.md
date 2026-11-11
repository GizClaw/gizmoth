# Vendored packages

## Magpie Design System 0.4.1

`idy-magpie-design-system-0.4.1.tgz` is built without changes from the clean
[`idy/magpie-design-system` source](https://github.com/idy/magpie-design-system/tree/1dc84e8db0e9dd11c39a315e957616bd208aa495)
(commit `1dc84e8db0e9dd11c39a315e957616bd208aa495`, package version `0.4.1`).
The MIT licenses and font notices are included in the package. A local archive
keeps app builds independent of GitHub package access.

The Agents help group imports the package's actual `css/components.css` layer.
A page-local token adapter in `src/pages/Agents.css` maps the component roles to
GizMoth's existing themes, font families and regular-only type rule. The app does
not load the library's document reset, shell, fonts or JavaScript.

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
