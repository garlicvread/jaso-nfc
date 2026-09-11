# 설치 및 고급 설정 / Installation and advanced setup

[한국어 사용 설명서](usage.md) · [English user guide](usage.en.md)

[다운로드 / Download](https://github.com/garlicvread/jaso-nfc/releases/download/v0.1.0/Jaso-NFC-0.1.0-arm64-local.dmg) · [릴리스 정보 / Release notes](https://github.com/garlicvread/jaso-nfc/releases/tag/v0.1.0)

0.1.0 · Apple Silicon Mac · macOS 13 이상 / macOS 13 or later

## Graphical installation package

### 한국어

1. 다운로드한 DMG를 열고 `Install Jaso NFC.app`을 더블클릭하세요.
2. `설치`를 누른 뒤 완료될 때까지 창을 열어 두세요.
3. `설치 파일 유지` 또는 `설치 파일 휴지통 이동`을 선택하세요. 휴지통 이동 대상은 원본 DMG입니다.
4. 설치 창을 닫고 Finder에서 설치 디스크를 추출하세요.
5. Jaso NFC의 `정리할 폴더…`에서 폴더를 선택하고 `파일명 미리보기`를 실행하세요. 결과를 확인한 뒤 `자동 정리 시작`을 누르세요.

앱은 `/Applications/Jaso NFC.app`에 설치됩니다. 새 설치는 다운로드 폴더를 제안하며, 업데이트는 저장된 폴더와 제외 설정을 이어서 사용합니다. 설치 권한 오류가 표시되면 응용 프로그램 폴더에 쓸 권한이 있는 계정을 사용하거나 Mac 관리자에게 문의하세요.

### English

1. Open the downloaded DMG and double-click `Install Jaso NFC.app`.
2. Choose `Install` and keep the window open until it finishes.
3. Choose `Keep Installer` or `Move Installer to Trash`. The Trash action applies to the original DMG.
4. Close the installer and eject its mounted disk in Finder.
5. Open `Manage folders…` in Jaso NFC, select your folders, and choose `Preview filenames`. Review the result, then choose `Start automatic cleanup`.

The app is installed at `/Applications/Jaso NFC.app`. A fresh installation suggests Downloads; an update continues with saved folders and exclusions. For an installation permission error, use an account that can write to Applications or contact the Mac's administrator.

If the Trash button is disabled, keep the installer file and manage it in Finder. The installer identifies the original DMG before offering this action. A cleanup error can be handled after the app is installed.

## macOS blocks the installer

다운로드한 파일의 출처를 확인한 뒤 [Apple의 앱 실행 허용 안내](https://support.apple.com/ko-kr/102445)를 따라 주세요. 앱을 한 번 열어 본 다음 `시스템 설정 → 개인정보 보호 및 보안`에서 해당 앱의 `확인 없이 열기`를 선택하고 `열기`로 확인하세요. 선택지가 없거나 손상 메시지가 나오면 DMG를 다시 다운로드하거나 지원팀에 문의해 주세요.

After checking that you trust the download, follow [Apple's app opening instructions](https://support.apple.com/en-us/102445). Try opening the app, then choose `Open Anyway` for that app in `System Settings → Privacy & Security` and confirm with `Open`. If the option is unavailable or you see a damage message, download the DMG again or contact support.

[지원 / Support](mailto:aidall_manager@aidall.tech) · [소스 / Source](https://github.com/garlicvread/jaso-nfc) · AidALL Inc.

<a id="enable-filename-changes"></a>

## Folder setup

일상적인 설정은 앱의 `정리할 폴더…`에서 진행하세요. 폴더와 제외 항목을 선택한 뒤 `파일명 미리보기`로 변경할 이름을 확인하고 `자동 정리 시작`을 누르면 됩니다. 자세한 순서는 [사용 설명서](usage.md#파일명-변경-켜기)를 참고하세요.

For everyday setup, use `Manage folders…` in the app. Choose folders and exclusions, review `Preview filenames`, then choose `Start automatic cleanup`. Follow the [user guide](usage.en.md#enable-filename-changes) for the full walkthrough.

### Advanced CLI alternative

The following reference is for scripts, source builds, and installations using custom paths. Preview and installation must use the same folder selection.

```sh
jaso_cli="/Applications/Jaso NFC.app/Contents/MacOS/jaso-nfc"
"$jaso_cli" scan --root "$HOME/Downloads"
```

Review `candidates` for proposed names and `errors` for incomplete checks. The preview's `renamed` count is zero. Then enable the same folder:

```sh
"$jaso_cli" install --app "/Applications/Jaso NFC.app" --root "$HOME/Downloads" --apply
```

The supplied `--root` list replaces the saved root list. Repeat it for every folder you want to keep watching. `--exclude` adds to saved exclusions. A custom installation should pass its `--config "/absolute/path/to/config.json"` to both commands. Omitted options retain existing values; in particular, omitting `--apply` keeps an existing rename setting. A new configuration defaults to preview, and `scan` requires its own explicit `--apply` to rename files.

For automatically discovered coverage, choose `--all-user-files` instead of `--root`. It can add account homes, cloud roots, and mounted volumes as they become available. Saved exclusions still apply. Review [coverage behavior](architecture.md#event-policy) before selecting this broad scope.

## Build the installer from source

Use the prerequisites in the native build section below. Build a complete DMG with:

```sh
sh scripts/build-installer.sh
```

To package an app that has already been built and verified:

```sh
sh scripts/build-installer.sh --app 'dist/Jaso NFC.app'
```

The output is `dist/Jaso-NFC-<version>-<architecture>-local.dmg` for the build machine's architecture. Packaging checks the payload, metadata, installation instructions, MIT license, and accompanying `.dmg.sha256` checksum. The release includes that checksum for download-integrity checks. See [build validation](development.md) for the test workflow.

## Native development build

Use macOS 13 or later, stable Rust, the Xcode Command Line Tools, and Python 3.11 or later. Python runs the bundle-validation suites in the build script.

```sh
sh scripts/build-native.sh
jaso_cli="$PWD/dist/Jaso NFC.app/Contents/MacOS/jaso-nfc"
"$jaso_cli" scan --root "$HOME/Downloads"
```

After reviewing the preview, install the build with the same scope:

```sh
"$jaso_cli" install --app "$PWD/dist/Jaso NFC.app" --root "$HOME/Downloads" --apply
```

The script compiles the Rust executable, native interface, and icon resources; runs the bundled checks; and creates `dist/Jaso NFC.app`. Helper executables and generated resources use `build/`. The resulting app targets the build machine's architecture. [Development and validation](development.md) describes the checks and opt-in filesystem fixtures.

## Installed layout

The working app is `/Applications/Jaso NFC.app`. The default support directory is `~/Library/Application Support/jaso-nfc`, containing:

| Location | Purpose |
| --- | --- |
| `config.json` | Processing scope, exclusions, and rename setting |
| `state/` | File index, queued work, retries, controls, and unfinished-operation record |
| `logs/` | Name-change history and diagnostic logs |
| `releases/` | Retained app copies for recovery |
| `backups/` | Earlier configuration and registration files from installation |

Language, appearance, and content size are stored through macOS preferences. For custom paths, `--state-dir` selects a support directory and `--log-dir` selects a log location. Processing excludes the chosen state and log paths.

One per-user LaunchAgent at `~/Library/LaunchAgents/io.github.garlicvread.jaso-nfc.plist` runs the installed executable with `run --config <path>`. That worker starts the native menu. The earlier separate menu registration is removed during migration. The installer retains a recovery copy at `releases/<version>-native-<digest>/Jaso NFC.app`.

Opening the app loads the configuration selected by the installed registration, including custom state paths. If registration files disagree or contain an invalid configuration path, resolve that error before installing again. After Quit, open the app and choose `Start worker` to continue processing.

## Permissions

권한 오류가 표시되면 아래 순서로 확인하세요.

1. `설정…` → `전체 디스크 접근 권한 열기…`에서 `+`로 `/Applications/Jaso NFC.app`을 추가하고 허용하세요. 항목이 추가되지 않으면 Finder에서 설치된 앱을 목록으로 끌어 넣으세요.
2. `로그인 항목 열기…`에서 Jaso NFC의 백그라운드 실행도 확인하세요.
3. 상태 창의 `작업 다시 시작`을 선택한 뒤 접근할 수 없는 위치를 다시 확인하세요.

For an access error, follow these steps:

1. Choose `Settings…` → `Open Full Disk Access…`, add `/Applications/Jaso NFC.app` with `+`, and enable it. If the entry is not added, drag the installed app from Finder into the list.
2. Check Jaso NFC's background activity through `Open Login Items…`.
3. Choose `Restart worker` in Status, then check the unavailable locations again.

File ownership, locks, and volume write permissions also affect access. For another account's files, check its sharing permissions. When running CLI tests, check the terminal's access permissions too. A rebuilt app may need its permission entry updated to the new installed copy. See Apple's [filesystem permission and executable identity guidance](https://developer.apple.com/forums/thread/678819) for development details.

## Upgrade from Python or an earlier native build

Run the new DMG installer using the same account. For a source build, run the new app's `install --app` command against the same support directory. The installer keeps processing settings, index and queued work, retries, recovery records, earlier releases, and the saved login preference.

Installation starts the new worker immediately. A saved choice to disable startup continues to govern the next login. Check Status after upgrading for active locations, pending work, and recovery messages.

The installer prepares and verifies the new app before stopping existing jobs. It retains the previous app and registration files until activation succeeds. If activation fails, it attempts to restore those files, jobs, startup preferences, and menu arguments, and reports any restoration error. Keep the backups until you have accepted the upgrade.

Installation and lifecycle commands in one account share an operation lock; a conflicting command reports that it should be retried. Coordinate separately when using several accounts. During recovery, keep the matching app, configuration, and registrations together, and run one worker against a state directory at a time. Retained Python releases require their original interpreter if restored.

## Worker and login controls

The app provides everyday controls through its menu and status window. For automation or troubleshooting, use the following CLI commands with the installed executable.

| Command | Effect |
| --- | --- |
| `pause` | Save a pause while a running worker continues recording filesystem changes. |
| `resume` | Clear the pause and notify the worker. |
| `reconcile --path <folder>` | Queue a recheck inside the saved scope, following exclusions and pause state. |
| `stop` | Stop the worker and wait for the registration to unload. |
| `start` | Start the installed worker with its saved login preference. |
| `restart` | Stop the previous worker registration, then start it again. |
| `startup off` | Disable startup at future logins. |
| `startup on` | Enable startup at future logins. |
| `startup status` | Report the worker's login startup setting. |

Pause and startup preferences are saved. Startup commands control future logins; use `stop` for the current worker. Reconciliation requires an initialized index and waits for the worker to be running and unpaused.

## Recovery

상태 창의 `복구 기록 폴더 열기`에서 이름 변경 기록의 위치를 확인하세요. 아래 고급 명령은 지정한 기록 전체를 대상으로 되돌리기를 시도하므로 사용할 이력을 먼저 확인해 주세요.

Use `Open recovery history` in Status to find the name-change records. The advanced commands below attempt to revert the selected journal and its archives; review that history before proceeding.

Disable future login startup and stop the current worker:

```sh
jaso_cli="/Applications/Jaso NFC.app/Contents/MacOS/jaso-nfc"
"$jaso_cli" startup off
"$jaso_cli" stop
```

Then specify the history to revert:

```sh
"$jaso_cli" revert --journal "$HOME/Library/Application Support/jaso-nfc/logs/renames.jsonl"
```

Use the actual log path and corresponding `--config` for a custom installation. Read the `reverted` and `failed` counts and any error output. Revert uses recorded file identity, so later moves, deletions, replacements, or identity changes can limit recovery. Keep the worker stopped while retaining the earlier names.

Preserve `logs/renames.jsonl`, `logs/renames.jsonl.history/`, and `state/pending.json`. Revert also keeps its own journal and unfinished-operation record. See [mutation and recovery](architecture.md#mutation-and-recovery) for the identity checks and exFAT concurrency limit.

## Uninstall

앱을 제거하기 전에 아래 명령으로 백그라운드 실행 등록을 정리하세요. 성공하면 Finder에서 응용 프로그램 폴더의 Jaso NFC를 휴지통으로 옮기세요. 이름을 되돌리고 싶다면 먼저 위의 복구 절차를 진행해 주세요.

Before deleting the app, remove its background startup registration with the following command. After it succeeds, move Jaso NFC from Applications to Trash in Finder. To restore earlier names, complete the recovery steps above first.

```sh
"/Applications/Jaso NFC.app/Contents/MacOS/jaso-nfc" uninstall
```

The command stops the worker and identified menu app and removes the worker and any legacy menu registration. It retains the app bundle, settings, index, and history. Saved data remains available for recovery or reinstallation after you remove the app from Applications.
