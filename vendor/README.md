# Vendored packages

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
  perch; if none is safe, it remains hidden and retries.

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
