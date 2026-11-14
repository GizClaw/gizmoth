//! The GizClaw/gizmoth-devenv registry: SDKs and toolchains, read from a Git
//! checkout and validated. The compile service downloads every Linux release
//! and packs it for its VMs (see `compile::packages`); nothing is installed
//! for macOS itself.
pub mod registry;
