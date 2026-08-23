# Repository instructions

## Release versioning

Before building a distributable for any functional or user-visible change, bump
the semantic version. Use a patch bump for a compatible fix, a minor bump for a
new compatible feature, and a major bump for a breaking change.

Keep every version source synchronized before the build:

- `crates/usage_core/Cargo.toml`
- `src/BetterTrafficMonitorAiUsage/BetterTrafficMonitorAiUsage.rc` (`FILEVERSION`,
  `PRODUCTVERSION`, and their string values)
- `src/BetterTrafficMonitorAiUsage/BetterTrafficMonitorAiUsagePlugin.cpp`
  (`TMI_VERSION`)
- `scripts/build-release.ps1` (the default package version)

Run the release build with that version and verify that the generated ZIP name,
DLL metadata, and reported plug-in version all match it. Do not release a
feature build under a previous version number.

Pushing a version bump to `main` automatically creates the corresponding
`vX.Y.Z` GitHub Release after its release build succeeds. Do not manually create
a tag or release for the same version unless repairing a failed automation.
