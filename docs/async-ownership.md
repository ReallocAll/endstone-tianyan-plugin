# Asynchronous ownership and recovery

The plugin owns a bounded 256-entry FIFO worker queue. Query, logging, offline
inventory, cleanup, migration and API-future jobs are joined before unload.
Rejection removes the request and reports an error; it does not leave a running
request behind. Shutdown cancels queries, stops HTTP admission, joins accepted
work, then performs the final log/status drain. A stalled native/MySQL call can
still delay shutdown: this change deliberately retains ownership rather than
running detached code after unload. There is no hard shutdown-time guarantee.

Backend replacement validates the candidate first, drains plugin work, joins
HTTP requests, and rebinds both core and HTTP query service before restarting.
Logging and rollback-status writes collected during migration wait for the new
backend. Failure to create the candidate retains the old backend.

Offline inventory reads produce one owned encoded snapshot in the worker.
Stable IDs survive collection/erasure, and UI completion requires the submitting
player session. Native buffers are released even when that player has left.

Rollback requires the captured session and dimension, rechecks operator permission,
and inspects at most 100 rows across all rollback tasks per scheduler callback.
Commands with error output are failures even when Endstone's dispatch boolean is
true. Successful mutations remain guarded against duplicate execution until
status persistence succeeds and no older rollback query remains. Failed and
recovered status writes receive the normal periodic retry.

Failed final writes are saved to `plugins/tianyan_data/pending-writes.json` and
replayed on the next enable. Existing UUID primary/unique keys make log replay
idempotent. This is graceful-shutdown recovery, not a synchronous per-event WAL:
process kills/power loss can still lose unflushed memory, and flush/rename does
not claim filesystem fsync durability. An unreadable recovery file is preserved;
resolve it before restarting. Errors writing the recovery file are logged as
`FINAL DRAIN FAILED`, never silently discarded. A network failure during commit
can leave the database outcome uncertain; failed status writes remain guarded.

SQLite leases return on error and roll back unfinished transactions. Rust MySQL
batch inserts/status updates use one lease and one transaction, and all commit
results are checked. Cancellation watchers are joined before the borrowed flag
or query connection is released. FFI error buffers and result handles have
explicit owned release paths.

## WebUI configuration

WebUI remains disabled by default. When enabled, configure a private `web_secret`;
empty, whitespace-only and the shipped `your_secret` placeholder are refused.
`web_host` defaults to `127.0.0.1`. For remote access, explicitly select the bind
address and use a TLS reverse proxy plus appropriate firewall rules. Existing
local clients and the query API are unchanged.

## Validation boundaries

Standalone C++ tests exercise queue admission, blocked shutdown, FIFO drain,
exception/restart behavior, rollback-write guards, SQLite failed acquisition,
failed-batch rollback and UUID replay. Rust tests exercise callback join and
owned error strings; the CI MySQL service tests transaction rollback/commit.
The native CI builds both Linux and Windows against the declared Endstone API.
Live BDS timing, cross-plugin permission changes and real offline-world decoding
still need a nonproduction server integration check. No production server was
modified by these tests.
