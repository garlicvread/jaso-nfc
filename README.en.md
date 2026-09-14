# Jaso NFC

[한국어](README.md) · English

Repair decomposed or garbled Korean filenames. Jaso NFC automatically cleans up names in your chosen folders and checks new files as they arrive. Preview proposed names and manage activity, folders, history, and settings in one window.

[Product website](https://garlicvread.github.io/jaso-nfc/en/) · [User guide](docs/usage.en.md)

## Download

[Download](https://github.com/garlicvread/jaso-nfc/releases/download/v0.2.1/Jaso-NFC-0.2.1-arm64-local.dmg)

0.2.1 · Apple Silicon Mac (M1 or later) · macOS 13 or later · Free / [MIT licensed](LICENSE)

## Which names can change?

| Before | After |
| --- | --- |
| `ㅂㅗㄱㅗㅅㅓ.pdf` · letters stored as separate components | `보고서.pdf` · NFC form |
| `µµÀüÀÇ IRÆ÷½ºÅÍ-ÃÖÁ¾º».pdf` · a name read using the wrong character encoding | `도전의 IR포스터-최종본.pdf` |

Garbled filenames are repaired when there is sufficient evidence to recover the original characters. Check the result in `Preview filenames` first. Select a completed change in `History` to restore its original name.

## 1. Open the installer

1. Open the downloaded DMG and double-click `Install Jaso NFC.app`.
2. Choose `Install` and keep the window open until it finishes.
3. Choose `Keep Installer` or `Move Installer to Trash`. The Trash action applies to the original DMG.
4. Close the installer and eject its mounted disk in Finder.

The app is installed in Applications. If macOS blocks it, follow the [app opening instructions](docs/usage.en.md#if-macos-blocks-the-app).

## 2. Review folders and proposed names

Choose `Open Jaso NFC…` from the menu bar, then select `Folders`. Pick Downloads and choose `Preview filenames` to compare current and proposed names. Add folders you need and exclude those you want to leave out.

If you already use Jaso NFC, review your saved folder list first. Select external drives under `Drives`, and choose whether each drive resumes automatically or starts manually when reconnected.

Choose `Remove from list` in a drive's details when you finish using it. Hidden folders such as `.ghost-alice` are excluded by default; select a folder directly when you want to include it.

## 3. Start automatic cleanup

After reviewing the preview, choose `Start automatic cleanup`. Jaso NFC checks the selected folders and new files as they arrive. If you paused the app earlier, choose `Resume` when ready.

Use `Status` to see today's changes and items needing attention. Choose `View activity` to see the current path and progress.

## Everyday controls in one window

| What you want to do | Tab or control |
| --- | --- |
| See overall status and today's changes | `Status` |
| Follow the current path and progress | `Activity`; optionally `Open in separate window` |
| Manage folders, exclusions, and external drives | `Folders` |
| Search changed names and restore an original name | `History` |
| Choose login startup, language, and theme | `Settings` |
| Check access permissions and storage | `Settings` → `Advanced settings` |
| Put cleanup on hold / continue | Menu bar `Pause` / `Resume` |

Use `Activity` to review recent work and items currently waiting. When a task stops, its entry shows the recorded cause, such as cloud storage or access permissions. A later successful check updates the entry with its completion result. Completed name changes remain in `History`. Select a history item to compare its original and changed name, check its location, and restore that item.

Each completed rename creates one change record. Checking the same file again updates progress, and retries for the same issue update its existing activity entry. Recent history stays within a storage budget, with older records retired automatically. See the space used by the file index, history, logs, and saved app versions in `Settings` → `Advanced settings` → `Storage usage…`.

Open Activity in a separate window to see the same current progress in both windows. Turn off `Follow new activity` when reading an earlier entry. Progress updates preserve your selected entry and reading position.

Close the window to leave cleanup running in the background. Choose `Quit Jaso NFC` from the menu to stop the work too. Open the app from Applications when you want to use it again.

## When you need help

- For folder access permission, choose `Settings` → `Advanced settings` → `Open Full Disk Access…`, enable Jaso NFC, then choose `Restart cleanup`.
- For a waiting cloud item, inspect the path and reason in `Activity` or `Status`, then check its connection and download state in Finder and your sync app.
- Check available disk space in `Settings` → `Advanced settings` → `Storage usage…`.

[Folders and drives](docs/usage.en.md#change-folders) · [Restore a name](docs/usage.en.md#recovery) · [Troubleshooting](docs/usage.en.md#permissions) · [Scope and safety](docs/usage.en.md#scope-and-safety)

## Help and support

Open these instructions from `Settings` → `Open user guide…` in the app.

[Complete user guide](docs/usage.en.md) · [Installation and upgrades](docs/installation.md) · [Release notes](https://github.com/garlicvread/jaso-nfc/releases/tag/v0.2.1)

Contact us through [GitHub issues](https://github.com/garlicvread/jaso-nfc/issues) or [aidall_manager@aidall.tech](mailto:aidall_manager@aidall.tech). Include the app version and status message. Before sharing information copied from `Settings` → `Advanced settings`, remove private filenames and paths.

Published by AidALL Inc.

Contributor: [garlicvread](https://github.com/garlicvread) · [ceo@aidall.tech](mailto:ceo@aidall.tech)

## For developers

Build from source with macOS 13 or later, stable Rust, the Xcode Command Line Tools, and Python 3.11 or later. Find build commands and validation steps in the [development guide](docs/development.md).

[Advanced installation and CLI setup](docs/installation.md) · [Architecture](docs/architecture.md) · [Implementation choices](docs/comparison.md)
