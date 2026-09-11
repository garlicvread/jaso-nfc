# Jaso NFC user guide

[한국어](usage.md) · English · [Download](../README.en.md)

Start with the Korean filenames in Downloads, then add the folders you need. This guide covers installation, folder selection, preview, automatic cleanup, and troubleshooting in version 0.1.0.

## Install and open

1. On an Apple Silicon Mac (M1 or later) running macOS 13 or later, [download the installer](https://github.com/garlicvread/jaso-nfc/releases/download/v0.1.0/Jaso-NFC-0.1.0-arm64-local.dmg).
2. Open the DMG, double-click `Install Jaso NFC.app`, and choose `Install`.
3. When installation finishes, choose `Keep Installer` or `Move Installer to Trash`. The Trash action applies to the original DMG. If the button is disabled, manage that file in Finder.
4. Close the installer and eject its mounted disk in Finder.
5. Click Jaso NFC in the menu bar and choose `Manage folders…`. For later launches, open the app from Applications.

A fresh installation suggests Downloads as the folder to clean up. An update continues with your saved folders and exclusions, so review the list first.

### If macOS blocks the app

After checking that you trust the download, follow [Apple's app opening instructions](https://support.apple.com/en-us/102445).

1. Try opening the app named in the alert.
2. Open `System Settings → Privacy & Security` and choose `Open Anyway` for that app.
3. Choose `Open` in the confirmation window.

If `Open Anyway` is unavailable or you see a damage message, download the DMG again or contact support. Check whether the alert names the installer or the installed Jaso NFC app.

<a id="enable-filename-changes"></a>

## Set up your folders

### Choose your folders

Open `Manage folders…` from the menu or Settings. Review the `Selected folders` list and use `Add folders…` to add a location. To take a folder off the list, select it and choose `Remove selected`.

### Review the files to rename

Choose `Preview filenames` to compare current names with proposed names. Run it again after changing folders or adding exclusions. Choose `Cancel preview` to stop the check.

If a location is unavailable, check its permissions or connection. You can begin cleanup in accessible folders while using the preview's error details to identify locations that still need a check.

The preview shows names checked within a time limit. When that limit is reached, review the sample, then select a specific folder to inspect it in more detail.

### Start automatic cleanup

After reviewing the result, choose `Start automatic cleanup`. It saves your selected folders and settings and begins cleanup. It also resumes work if you previously paused it.

Follow activity in `Open Status…`. Jaso NFC keeps processing new files in your selected folders. Choose `Pause` from the menu whenever you want to put cleanup on hold.

<a id="change-folders"></a>

## Choose folders to clean up

Use `Manage folders…` to manage your folder list and exclusions together.

| What you want to do | How to do it |
| --- | --- |
| Add a folder | Choose `Add folders…` and select a folder to clean up. |
| Remove a folder | Select a folder in the list and choose `Remove selected`. |
| Exclude a folder | Expand `Excluded folders`, choose `Add exclusions…`, and select the folder to leave out. |
| Review names | Choose `Preview filenames` to compare current and proposed names. |
| Save settings | Choose `Save settings` to apply your folder settings while keeping the current running or paused state. |
| Save and begin cleanup | Choose `Start automatic cleanup` to save settings and start or resume processing. |

Start with a folder that is easy to review, such as Downloads. If you already use Jaso NFC, review the full saved list before adding or removing folders.

`All user folders` automatically discovers account homes, shared folders, cloud document folders, and connected external volumes. Coverage can expand to newly connected or discovered locations and hidden user files outside exclusions. Use `Selected folders` for a fixed set of locations. Both choices apply your saved exclusions.

## Everyday controls

| What you want to do | How to do it |
| --- | --- |
| Check processing | Open `Open Status…` for activity, watched locations, and affected files. |
| Put cleanup on hold | Choose `Pause`. Changes that arrive in the meantime are recorded. |
| Continue waiting work | Choose `Resume`. The pause remains saved across app launches. |
| Start stopped work | Choose `Start worker`. |
| Change watched folders | Open `Manage folders…` to edit the folder and exclusion lists. |
| Recheck a saved folder | Open Status and choose `Recheck all watched roots`. |
| Stop the app and its work | Choose `Quit Jaso NFC`. The app exits after confirming the worker stopped. |

A folder recheck uses your current preview or automatic cleanup setting and exclusions. Choose `Resume` first if work is paused, or `Start worker` if it is stopped. To select a new watched folder, use the [folder settings](#change-folders).

Close or hide the window to leave work running in the background. The app also appears in the Dock. After quitting, open Jaso NFC from Applications and choose `Start worker` to continue. If an error appears during shutdown, check the status and try stopping the worker again.

Enable `Settings…` → `Run at login` to start the app and cleanup at your next login. Use Pause, Stop worker, or Quit to control the current session. To keep work stopped at the next login too, turn off `Run at login`.

## Status and settings

Open `Open Status…` to see current activity and watched locations. The window refreshes every five seconds while visible; choose `Refresh` for a manual update.

| Status shown | What to do next |
| --- | --- |
| `Preview mode` | The app is inspecting folders and names. Open `Manage folders…` and choose `Start automatic cleanup` when ready. |
| `Initial indexing` | The app is building its file list. Allow time for the first check, especially when there are many folders. |
| `Watching for changes` | The first check is complete and the app is waiting for new files and changes. |
| `Paused` / `Stopped` | Choose `Resume` / `Start worker` to continue cleanup. |
| `Waiting to retry some items` | Check the reason and retry time for each item in `File status`. |
| `Check unavailable locations` | Open the listed path in Finder and check its connection or access permissions. |
| `Recovery pending` | An unfinished name change is recorded. If it persists, retain the records and contact support. |

`Indexed items` counts the files, folders, and links recorded so far. `Queued folders` counts folder work waiting to run. `Rename retries` counts items awaiting another attempt. Counts can grow as more folders are discovered, so read them alongside the current activity message.

`File status` shows filenames, paths, the recorded cause, and the next step. Use `Show in Finder` to open the location. The list shows up to eight rename retries and eight folder retries, together with the total waiting count. Work can retry from the displayed time as its turn comes up. Failed renames first wait 15 minutes; repeated failures increase the interval up to one day.

At the bottom of Status, use `Restart worker`, `Stop worker`, or `Recheck all watched roots`. `Open recovery history` opens the name-change records in Finder, and `Copy diagnostics` copies current status for a support request. Remove private filenames and paths before sharing.

Choose Korean, English, or the system language in `Settings…`; menus and windows update immediately. The default icon theme is `Bookish Myeongjo`. You can also choose `Quiet Gothic`, or `Brushstroke Gungseo` on a Mac with that font available.

| Quiet Gothic | Bookish Myeongjo | Brushstroke Gungseo |
| --- | --- | --- |
| ![Gothic theme preview](assets/gothic.gif) | ![Myeongjo theme preview](assets/myungjo.gif) | ![Gungseo theme preview](assets/gungseo.gif) |

These theme previews are enlarged. The icon shows Korean letters coming together over a 12-second cycle. For a still icon, turn off animation or enable macOS Reduce Motion. Use Status to follow processing activity.

Command-comma opens Settings. Use Command-plus or Command-equals, Command-minus, and Command-zero to adjust status, folder setup, and app settings content from 80–200%. Your chosen size is saved for the next launch.

<a id="permissions"></a>

## Permissions and troubleshooting

| Situation | What to check |
| --- | --- |
| Protected folder unavailable | Choose `Settings…` → `Open Full Disk Access…`, add `/Applications/Jaso NFC.app` with `+`, and enable it. If it is not added, drag the app from Finder into the list. Then choose `Restart worker` in Status. |
| Background activity blocked | Choose `Settings…` → `Open Login Items…` and allow Jaso NFC's background activity. |
| Permission error installing to `/Applications` | Use an account with permission to install apps there, or contact the Mac's administrator. |
| Cloud folder unavailable or timed out | Open the path in Finder. Check that the sync app is running and its account is signed in and connected. After restoring access, wait for the scheduled retry or recheck watched folders. |
| External drive unavailable | Connect the drive and check that it opens in Finder. Saved work can continue when access returns. |
| Name conflict | Check the filename and destination in `File status`, then identify and resolve the conflicting items in Finder. |
| Locked item or read-only location | Open Get Info in Finder and check the lock and Sharing & Permissions. For a read-only disk, work with a copy in a writable location. |
| An expected name was not cleaned up | Check preview, pause, and stop state, then your folder selection and exclusions. Separately typed compatibility letters such as `ㅎㅏㄴ` are outside this conversion. |

Jaso NFC works with the current account's access permissions. For files owned by another account, check that account's sharing permissions too. System Settings labels vary slightly by macOS version. See the [detailed permission guide](installation.md#permissions).

## Scope and safety

Jaso NFC brings file and folder names into NFC form when the same characters are stored as separate components, including decomposed Korean. It processes items inside your selected folders. The selected top-level folder names, volume names, and targets reached through symbolic links are outside that scope.

Automatic exclusions cover `.git`, AppleDouble companion files (`._*`), the app's state and log paths, and the contents of these packages: `.app`, `.photoslibrary`, `.musiclibrary`, `.tvlibrary`, `.aplibrary`, and `.framework`. Add other folders you want excluded through `Manage folders…` → `Add exclusions…`.

`All user folders` excludes home Library internals, Trash, and recognized system areas while discovering cloud document folders separately. Google Drive's provider-root `.tmp` is excluded as a managed area; same-named folders inside My Drive or Shared drives can be processed. Saved exclusions also apply.

When a destination conflict is detected, the item is deferred and reported in Status. On filesystems including exFAT, another app can create a destination between the last check and the rename, and that destination can be overwritten. Avoid simultaneous renaming by other apps in these locations and review the [filesystem processing limits](architecture.md#mutation-and-recovery).

Processing settings, file lists, and name-change records are stored in `~/Library/Application Support/jaso-nfc` by default. Language, theme, and content size are saved in macOS preferences. Changes inside a cloud folder may be synchronized according to your sync app's settings.

<a id="recovery"></a>

## Undo retained name changes

Choose `Open recovery history` in Status to find the name-change records. To restore earlier names, prepare as follows.

1. Turn off `Run at login` in Settings.
2. Choose `Stop worker` in Status.
3. Follow the [advanced recovery instructions](installation.md#recovery) to review the history you will use and perform the undo.

Recovery processes the selected journal and its archived history. The original files must still be identifiable; later moves, deletions, replacements, or identity changes on exFAT can cause a failure. Restore file contents through your usual backup system.

Keep the worker stopped while you want the earlier names. Starting it again can normalize eligible names again. Retain recovery history and unfinished-operation records. If `Recovery pending` persists, keep those records and contact support.

## Update or remove

To update, run the installer from the new DMG using the same account. It continues with your saved folders, rename setting, login preference, and recovery history, and starts the new worker immediately. Check watched locations and current activity in `Open Status…` afterward.

To remove the app, follow the [removal steps](installation.md#uninstall) to remove its background startup registration first, then move Jaso NFC from Applications to Trash. Settings and recovery records remain available for recovery or reinstallation.

If you want to restore earlier filenames, follow the undo steps above before removing the app.

## Ask for help

Contact us through [GitHub issues](https://github.com/garlicvread/jaso-nfc/issues) or [aidall_manager@aidall.tech](mailto:aidall_manager@aidall.tech). Include the app version, macOS version, status message, and the steps that lead to the problem. Remove private filenames and paths from diagnostics before sharing.

[Installation and upgrades](installation.md) · [Architecture](architecture.md) · [Build and validation](development.md) · [MIT license](../LICENSE)
