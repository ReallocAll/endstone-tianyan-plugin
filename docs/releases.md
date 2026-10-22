# Releases

The persistent **Build and Release** workflow builds both native platforms and
runs C++ ownership/SQLite and Rust/MySQL regressions on every master push.
Ordinary commits never publish. To prepare a version, update CMake `BASE_VERSION`
and add a nonempty `## [X.Y.Z]` entry to `CHANGELOG.md`; commit exactly
`chore: release vX.Y.Z`. A matching `vX.Y.Z` tag push is also supported.

Only after every build and regression succeeds does the publish job create the
tag at that run's exact SHA and publish that run's artifacts. Existing releases
are never overwritten, and existing tags cannot move. The manual entrypoint
calls the same workflow; dry-run is the default, and publishing requires a
prepared matching version on master. No external token or repository setting
change is needed. The old independent development-release path is superseded.

Release files include Linux `endstone_tianyan.so`, Windows `endstone_tianyan.dll`,
platform ZIPs with language data, `SHA256SUMS` and source/run provenance.
With the server stopped, back up `plugins/tianyan_data`, put the native library
directly under `plugins`, and merge the ZIP's `tianyan_data/language` files into
`plugins/tianyan_data/language`. Keep the existing configuration and database.
The WebUI frontend and inventory UI are embedded. Endstone 0.11.x is required;
the build targets the declared 0.11.9 API. See `async-ownership.md` for WebUI
binding changes and graceful-shutdown recovery limits.
