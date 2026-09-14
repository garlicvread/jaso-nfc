# Development and validation

The production runtime is Rust plus the AppKit menu application. Python remains as a compatibility reference and provides standard-library black-box CLI tests.

## Local checks

Run on macOS with stable Rust, the Xcode Command Line Tools, and Python 3.11 or later. Python is also required by `scripts/build-native.sh`, which runs bundle validation suites; the installed app does not require it:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --locked
python3 native/tests/test_cli.py
python3 native/tests/test_app_trampoline.py
python3 native/tests/test_release_signing.py
sh scripts/build-native.sh
```

Cargo enables the complete `runtime` feature by default. The build script builds the release CLI, compiles the menu application and icon renderer, runs the icon and menu startup tests, and verifies the ad hoc app signature. The menu links `AnimatedMark.m` with AppKit, QuartzCore, and CoreText; the preview renderer also uses ImageIO. Rust artifacts stay under `target/`, helper executables and generated icon resources under `build/`, and the application under `dist/`. The script does not install login jobs.

The menu startup harness is `native/tests/menu_startup.m`, compiled as `build/menu-startup-test` and run automatically during `sh scripts/build-native.sh`. It checks registered custom config paths, malformed registrations, precise process identity matching, and preservation of the previous menu's arguments during handoff. Its subprocesses and files are temporary test fixtures; it does not stop the installed menu.

`native/tests/test_installed_runtime.py` exercises the real bundled `run` command with owned native menu fixtures. It verifies separate process sessions, menu survival across worker termination, child reaping, singleton behavior and that manual `watch` opens no GUI. The native build script runs it against the app's ad hoc signed Rust executable; CI also runs it against the debug CLI. `native/tests/installer.m` checks graphical installation planning and bounded Trash handling. `native/tests/test_installer_package.py` mounts a generated DMG read-only and verifies its payload and metadata without registering live jobs.

The trampoline suite builds temporary bundles and checks same-process GUI dispatch, explicit CLI isolation, metadata validation, executable validation, and malformed bundle rejection. The release build runs all bundle fixture suites before applying the final Developer ID signature, then verifies that signature and runs `--version` and `--help` against the intact signed app. These fixtures do not change Full Disk Access or verify a live worker permission grant.

The retained Python suite checks earlier behavior and compatibility formats:

```sh
PYTHONPATH=src python3 -m unittest discover -s tests -v
```

CI runs native checks and app-bundle construction, including the menu startup harness, on `macos-latest`. Separate Python compatibility jobs use Python 3.11 and 3.14. A successful CI bundle build establishes neither notarization nor permission to access protected user folders.

Both build scripts default to local mode: an ad hoc signature, a `-local` DMG name, and no Apple credentials. The explicit `--release` mode signs with a Developer ID Application identity and notarizes and staples the payload app and the DMG; its inputs `JASO_SIGNING_IDENTITY`, `JASO_TEAM_ID`, and `JASO_NOTARY_PROFILE`, the signing sequence, and the acceptance checklist are in [Release signing and notarization](releasing.md). `python3 native/tests/test_release_signing.py` exercises the release preflight and sequencing with stubbed signing and notary tools; it needs no certificate or notary profile, and passing it is not evidence that a real artifact was signed or accepted. Pull request and CI builds never receive signing credentials.

## Filesystem fixtures

Native unit tests exercise durable pending phases, both rename hops, rollback, journal persistence, archive rotation, identity changes, collision preservation, event ingestion, restart behavior, and coverage discovery. Callback tests include real native event delivery on macOS. All mutation fixtures are temporary and owned by the test.

The CLI suite uses `target/debug/jaso-nfc` by default. Select another binary explicitly when testing a release build:

```sh
JASO_NATIVE_BINARY="$PWD/target/release/jaso-nfc" python3 native/tests/test_cli.py
JASO_NATIVE_BINARY="$PWD/target/release/jaso-nfc" python3 native/tests/test_app_trampoline.py
```

External-volume coverage is opt-in. Set `JASO_TEST_VOLUMES` to colon-separated writable mount paths you intend to test; replace these illustrative names with your own test volumes:

```sh
JASO_TEST_VOLUMES="/Volumes/Test Disk:/Volumes/Other Test Disk" \
  python3 native/tests/test_cli.py
```

The suite creates uniquely named temporary package directories on those volumes and local temporary state, then removes its own fixtures. It does not normalize existing user files. CI leaves this variable unset. Never commit personal mount paths or private filenames to test defaults, reports, or workflows.

A forced unsupported-rename test on APFS verifies the compatibility code path but does not establish real exFAT behavior. A release validation should also cover actual target filesystems, including empty files, metadata, nested paths, supported symlinks, repeated reconciliation, and revert. Verify stored names through descriptors; directory listings alone can show a different normalization form.

## Resource measurements

Measure the release worker and menu app separately, then assess the complete installation. Record the OS, build, architecture, filesystem, watched roots, workload, icon theme, and whether animation is enabled. Distinguish initial indexing, settled idle, ordinary file events, retries, and explicit reconciliation. Include memory and CPU observations with a stated sampling interval.

The worker keeps the whole-tree index and recursive frontier in SQLite and sleeps when no work or retry is due. Worker directory observations spool wide listings to private temporary SQLite snapshots and yield between bounded batches. The synchronous `scan` command retains its whole-result behavior. Snapshot memory, temporary storage, and an individual filesystem call still contribute to resource usage. The menu fetches status when opened. Its glyph outlines interpolate between separated jamo and the composed word through Core Animation in a 12-second loop; there is no application frame timer. The three font styles are Bookish Myeongjo by default, Quiet Gothic, and Brushstroke Gungseo when its font is available. Reduce Motion or disabling animation holds the composed word still.

A native event-loop floor benchmark excludes work performed by the complete engine unless that work is explicitly included. It cannot establish application-wide memory or CPU usage. No fixed RSS or zero-CPU target is a published result here.

## Review boundaries

Durable event consequences must commit with their cursor. An incomplete directory observation must not erase prior evidence. A pending mutation must survive failures and block further changes when identity is uncertain. Retained success history must remain readable after rotation, upgrade, and recovery. Installation failures must preserve the prior files and login state. Discovery delays must not block healthy-root mutation work, stream startup must precede restored jobs, and dormant roots must retain prior observations. Chunked observations must preserve unseen entries until completion, yield to new work, and abort on a replaced directory or unresolved pending mutation.

Keep these properties covered by behavior tests when changing the implementation. Formatting and compiler diagnostics are additional checks, not substitutes for filesystem and restart verification.
