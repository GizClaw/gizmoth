# Vendored packages

## Peetty 0.2.0

`idy-peetty-0.2.0.tgz` is the unmodified npm package built from
[`idy/peetty` v0.2.0](https://github.com/idy/peetty/tree/de14590632c91dc914d56a8bc9074fead615aa5d)
(commit `de14590632c91dc914d56a8bc9074fead615aa5d`). Its MIT license is included
in the archive and installed package.

The upstream registry is GitHub Packages, which requires authentication even for
public packages. Keeping this release archive here lets normal `npm ci` and
release builds run without an additional registry credential. The dependency is
installed as `@idy/peetty`; application code imports its public SDK.

This tagged release provides the local figure, animation, perching and drag
behavior. It has no AI/chat implementation or runtime dependencies. Do not infer
its API from the upstream main branch, which added chat after this tag.

To update, review and download a specific upstream tag into a separate temporary
directory, install its development dependencies, run `npm run build` there, then
run `npm pack <source-directory> --pack-destination vendor` from this repository.
Install the resulting archive with `npm install ./vendor/<archive> --save-exact`.
Record the new version and full source commit here. Recheck companion movement,
cleanup, language changes, reduced motion, and the desktop WebView.
