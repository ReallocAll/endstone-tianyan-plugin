# Changelog

## [1.2.5] - 2026-10-22

- Own and join asynchronous database, query, cleanup, migration, logging and offline-inventory work before unload; reject excess work without leaving running requests behind.
- Rebind the HTTP query service during backend replacement and retain the previous backend if replacement preparation fails.
- Bind rollback to the originating player session and dimension, limit per-tick work, and count only commands that succeed without error output. Keep duplicate guards until status writes are confirmed, including recovery and in-flight retries.
- Read offline inventories once into an owned native snapshot; use stable completion IDs and typed player command events.
- Return SQLite connections on all error paths, roll back unfinished transactions and preserve failed final writes for graceful-restart recovery.
- Join Rust cancellation watchers before releasing borrowed state or database connections; use owned FFI errors and single-connection atomic MySQL batch transactions with checked commit results.
- Refuse empty/default WebUI secrets and default binding to loopback. Remote users must explicitly configure the bind address and a protected reverse proxy.
- Add C++/SQLite/Rust/MySQL regression tests and a formal versioned release flow with native Linux/Windows builds, dependency manifests, provenance and SHA-256 checksums.

### Upgrade notes

Stop the server and back up `plugins/tianyan_data` before upgrading. Put `endstone_tianyan.so` directly in `plugins` on Linux (`endstone_tianyan.dll` on Windows), and merge the platform ZIP's language files into `plugins/tianyan_data/language`. Keep the existing configuration and database. The frontend and inventory UI are embedded. Linux needs the standard SQLite3 and zlib runtime libraries.

Recovery protects failed writes during graceful shutdown, not power loss or forced process termination. Network/native calls can delay joined shutdown; a connection failure during commit may leave its outcome uncertain. Live BDS lifecycle, migration, permissions and offline-world integration still require a nonproduction server check. See `docs/async-ownership.md`.
