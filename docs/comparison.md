# Implementation choices

The target is a small resident worker: sleep while idle, normalize changed names,
remember unfinished work across restarts, and cover accounts and mounted user
volumes. The application packages that Rust worker with a separate native menu-bar interface for status and explicit controls. The menu does not own normalization or the persistent event queue.

The following source revisions were inspected; the comparison concerns these
code paths rather than every feature of each project.

| Concern | Public implementation examined | Choice for jaso-nfc |
| --- | --- | --- |
| Event delivery | [nfd2nfc watcher](https://github.com/elgar328/nfd2nfc/blob/d7f35e021caebfd6c6eb887941f943faf1bc2451/nfd2nfc-watcher/src/watcher.rs) deduplicates pending paths before handling events. | Coalesce directory work in SQLite, allowing the pending set to survive restarts without retaining the whole index in RAM. |
| Actual filename | [nfd2nfc normalizer](https://github.com/elgar328/nfd2nfc/blob/d7f35e021caebfd6c6eb887941f943faf1bc2451/nfd2nfc-core/src/normalizer.rs) uses descriptor-based `F_GETPATH`. | Adopt descriptor-based name resolution. Real exFAT fixtures showed that directory listings can present NFD even after successful NFC renaming. |
| Rename compatibility | [NFCNameFixer converter](https://github.com/drzekil-dev/NFCNameFixer/blob/d665908991971228d834b0b54156a86713f2e108/Sources/Converter.swift) checks distinct source/destination identities and uses ordinary POSIX rename. | Prefer exclusive native rename; use a guarded ordinary rename when that primitive is unsupported. Keep durable recovery records and verify the resulting name. |
| Missing exclusive-rename support | [GNU gnulib renameatu](https://github.com/coreutils/gnulib/blob/master/lib/renameatu.c) also implements a non-atomic compatibility fallback. | Independently implement the compatibility approach and document its check/rename race; do not claim an unavailable atomic guarantee. No gnulib code is incorporated. |
| Startup and event loss | [NFCNameFixer watcher](https://github.com/drzekil-dev/NFCNameFixer/blob/d665908991971228d834b0b54156a86713f2e108/Sources/FolderWatcher.swift) starts from current events and scans at startup, with broader scans after dropped events. | Persist per-root cursors and queued work; start watching before initial enumeration. Resume saved work, reconciling affected roots when history cannot be trusted. |
| Resident memory | The nfd2nfc handler works on individual changed paths; whole-tree enumeration is unnecessary for ordinary events. | Reconcile one directory at a time. Store recursive traversal continuations on disk and use bounded SQLite caches. |
| Idle execution | Native filesystem events supply wakeups. | Block on a local wakeup descriptor with no periodic idle polling. Retry deadlines and explicit control requests also wake the worker. |

Ordinary rename compatibility preserves the rename operation rather than copying
file contents. A preexisting destination is rejected. On filesystems without an
exclusive primitive, another program can still create a destination between the
last check and the rename; the compatibility path cannot make that interval
atomic. Recovery state addresses interrupted operations, not arbitrary concurrent
rewrites by other applications.


The production implementation now resides in `native/src/`; the Python code is retained as a compatibility reference. The comparison above records design choices, not a current benchmark ranking. The menu's optional composition animation uses Core Animation, while worker wakeups remain driven by events and retry deadlines. Resource claims require full-application measurements; a minimal native-loop benchmark is insufficient.
