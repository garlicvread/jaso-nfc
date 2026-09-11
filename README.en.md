# Jaso NFC

[한국어](README.md) · English

Tidy decomposed Korean filenames and automatically handle new files. Jaso NFC brings file and folder names into Unicode NFC form within your chosen folders. Its menu bar controls show current activity and items that need attention.

[Product website](https://garlicvread.github.io/jaso-nfc/en/) · [User guide](docs/usage.en.md)

## Download

[Download](https://github.com/garlicvread/jaso-nfc/releases/download/v0.1.0/Jaso-NFC-0.1.0-arm64-local.dmg)

0.1.0 · Apple Silicon Mac (M1 or later) · macOS 13 or later · Free / [MIT licensed](LICENSE)

Install the app and start with the Korean filenames in Downloads.

## 1. Open the installer

1. Open the downloaded DMG and double-click `Install Jaso NFC.app`.
2. Choose `Install` and keep the window open until it finishes.
3. Choose `Keep Installer` or `Move Installer to Trash`. The Trash action applies to the original DMG.
4. Close the installer and eject its mounted disk in Finder.

The app is installed in Applications. If macOS blocks it, follow the [app opening instructions](docs/usage.en.md#if-macos-blocks-the-app).

## 2. Review the files to rename

Choose Jaso NFC in the menu bar → `Manage folders…`. Select Downloads and choose `Preview filenames` to compare current and proposed names. Add other folders you need or remove them from the list.

If you already use Jaso NFC, review your saved folder list first. See [folder selection and exclusions](docs/usage.en.md#change-folders) for the details.

## 3. Start automatic cleanup

After reviewing the preview, choose `Start automatic cleanup`. Jaso NFC cleans up your selected folders and keeps checking new files. Choose `Open Status…` from the menu to see current activity and waiting items.

## Everyday controls

| What you want to do | What to choose |
| --- | --- |
| See activity and affected files | `Open Status…` |
| Put cleanup on hold / continue | `Pause` / `Resume` |
| Change the folders to clean up | `Manage folders…` |
| Recheck a folder in your saved selection | `Open Status…` → `Recheck all watched roots` |
| Choose login startup, language, and theme | `Settings…` |
| Start stopped cleanup | `Start worker` |

Close the window to leave cleanup running in the background. Choose `Quit Jaso NFC` to stop the work too. Next time, open the app and choose `Start worker`. Use `Settings…` → `Run at login` to choose whether it starts automatically when you log in.

## Solve a problem

- For a folder that needs access permission, choose `Settings…` → `Open Full Disk Access…`, add and enable the installed Jaso NFC app, then choose `Restart worker` in Status.
- For a cloud folder waiting to retry, open the path from Status in Finder and check the login and connection in your sync app.
- For a name conflict or locked item, follow the reason and next step shown for that file in `File status`.

[Status messages and next steps](docs/usage.en.md#permissions) · [Processing scope and exFAT precautions](docs/usage.en.md#scope-and-safety) · [Undo names and remove the app](docs/usage.en.md#recovery)

## Help and support

[Complete user guide](docs/usage.en.md) · [Installation and upgrades](docs/installation.md) · [Release notes](https://github.com/garlicvread/jaso-nfc/releases/tag/v0.1.0)

Contact us through [GitHub issues](https://github.com/garlicvread/jaso-nfc/issues) or [aidall_manager@aidall.tech](mailto:aidall_manager@aidall.tech). Include the app version and status message to help us investigate. Remove private filenames and paths before sharing diagnostics.

Published by AidALL Inc.

## For developers

Build from source with macOS 13 or later, stable Rust, the Xcode Command Line Tools, and Python 3.11 or later. Find build commands and validation steps in the [development guide](docs/development.md).

[Advanced installation and CLI setup](docs/installation.md) · [Architecture](docs/architecture.md) · [Implementation choices](docs/comparison.md)
