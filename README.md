# GizMoth website

This branch contains the static website at <https://gizmoth.idy.dev/>. The desktop application source and its documentation live on the repository's `main` branch.

## Preview

Use Python 3 to serve this branch from its root:

```sh
python3 -m http.server 8000
```

Open <http://localhost:8000/>. No dependency installation or build step is required.

## Files

- `index.html` contains the landing page and verified v0.1.0 installer links that work without JavaScript.
- `styles.css` contains the page layout and responsive styles.
- `script.js` contains English/Chinese copy and refreshes release information and installer links from GitHub Releases.
- `language.js` accepts `?lang=en` and `?lang=zh` before shared preferences initialize. `zh-cn.html` preserves the previous Chinese entry point as a full page; both HTML pages include localized content and installer links that work without JavaScript. Keep their authored fallback content synchronized with the translations in `script.js`.
- `shared/sdk/` vendors the complete idy.dev Site SDK 0.2.0 distribution. It provides the Component Datasheet frame, theme/language controls, self-hosted Archivo, Phosphor icons and licenses. Keep the complete distribution together when updating it; both language pages load its JS/CSS through relative URLs, followed by website overrides in `styles.css`. The frame uses a blue masthead, white/deep-navy sheets, 2px corners and ruled specification/download rows without shadows.
- `assets/preview-mono-512.png` is the existing GizMoth brand mark.
- `CNAME` and `.nojekyll` configure the custom domain and static GitHub Pages publishing.

## Downloads

The page uses the shared `IdySite.loadRelease` and `IdySite.asset` helpers to select uploaded assets from the latest published release. Drafts, empty assets, signature files, and invalid download URLs are excluded. Missing installers are hidden with an explanation and a link to all releases.

If the API request fails, the verified v0.1.0 links remain available with a notice. Refresh these authored fallback links and their version labels when updating the site's baseline release. A future release normally needs no site change when it follows the existing asset names:

- `GizMoth_<version>_aarch64-apple-darwin.dmg`
- `GizMoth_<version>_x86_64-apple-darwin.dmg`
- `GizMoth_<version>_x86_64-pc-windows-msvc-setup.exe`

## Publishing

GitHub Pages publishes this branch's root. The `.nojekyll` marker bypasses the previous Jekyll theme; commit the HTML, scripts, styles, shared assets, and marker together.
