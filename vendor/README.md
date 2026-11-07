# Staged SDK packages

`gizclaw-models-player-0.7.0.tgz` is the locally built Models Player package from the `feat/model-icons` branch of `GizClaw/devtools`. It adds the lightweight `getModelIcon()` API and bundled SVG assets for all six player models.

The file dependency makes this checkout reproducible before the new version is published to the GizClaw npm registry. Once 0.7.0 is published, replace the file dependency with the exact registry version and remove this staged archive. Runtime model geometry and firmware continue to use the SDK's existing provider contracts; none are included in this archive.
