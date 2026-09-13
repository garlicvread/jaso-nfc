# Jaso NFC user guide

[한국어](usage.md) · English · [Download](../README.en.md)

Start with Korean filenames in Downloads, then add the folders you need. This guide covers Jaso NFC 0.2.0: folder selection, filename preview, automatic cleanup, and restoring an original name.

## Install and open

1. On an Apple Silicon Mac (M1 or later) running macOS 13 or later, [download the installer](https://github.com/garlicvread/jaso-nfc/releases/download/v0.2.0/Jaso-NFC-0.2.0-arm64-local.dmg).
2. Open the DMG, double-click `Install Jaso NFC.app`, and choose `Install`.
3. Choose `Keep Installer` or `Move Installer to Trash` when it finishes. The Trash action applies to the original DMG. If the button is disabled, manage that file in Finder.
4. Close the installer and eject its mounted disk in Finder.
5. Open Jaso NFC from Applications. Choose `Open Jaso NFC…` from the menu bar to return to the workspace.

A fresh installation suggests Downloads. After updating, review your saved folders and exclusions first.

### If macOS blocks the app

After checking that you trust the download, follow [Apple's app opening instructions](https://support.apple.com/en-us/102445).

1. Try opening the app named in the alert.
2. In `System Settings → Privacy & Security`, choose `Open Anyway` for that app.
3. Choose `Open` in the confirmation window.

If `Open Anyway` is unavailable or you see a damage message, download the DMG again or contact support. Check whether the alert names the installer or the installed Jaso NFC app.

<a id="filename-repair"></a>

## Repair decomposed or garbled Korean names

Jaso NFC brings decomposed Korean file and folder names into NFC form. For regular files, it also repairs eligible names that were read using the wrong character encoding. Select your folders and choose `Preview filenames` to review both kinds of change together.

| Current name | Proposed name |
| --- | --- |
| `ㅂㅗㄱㅗㅅㅓ.pdf` · letters stored as separate components | `보고서.pdf` |
| `µµÀüÀÇ IRÆ÷½ºÅÍ-ÃÖÁ¾º».pdf` | `도전의 IR포스터-최종본.pdf` |

Filename encoding repair covers reversible Korean filenames originally encoded as UTF-8 or CP949 and misread as Latin-1 or Windows-1252. CP949 candidates also need recognizable Korean filename terms and signs of character corruption. Names with weak evidence or multiple plausible interpretations keep their current spelling. If characters have already been replaced by `?` or `�`, confirm the original name and edit it in Finder.

Automatic cleanup uses the same rules for new names. Find completed changes in `History` to review or restore them. If text inside a document is garbled, check the encoding in the application that opens that document.

## Find your way around

Switch between `Status`, `Activity`, `Folders`, `History`, and `Settings` in the same window.

| Tab | What you can do |
| --- | --- |
| `Status` | See the current state, today's changes, recent changes, and items needing attention. |
| `Activity` | Follow the current path, measured progress, and recent work. |
| `Folders` | Choose folders, exclusions, and external drives, then preview names. |
| `History` | Search by filename or folder, filter by date and result, and restore one original name. |
| `Settings` | Choose language, appearance, and login startup; open `Advanced settings` for permissions and storage. |

<a id="enable-filename-changes"></a>
<a id="change-folders"></a>

## Choose folders to clean up

1. Review `Selected folders` in the `Folders` tab.
2. Choose `Add folders…` to add a location. Select a folder and choose `Remove selected` to take it off the list.
3. To leave out a subfolder, expand `Excluded folders` and choose `Add exclusions…`.
4. Choose `Preview filenames` to compare current and proposed names.
5. Choose `Start automatic cleanup` when ready.

Run the preview again after changing folders or exclusions. Choose `Cancel preview` to stop checking. If a time limit or access problem appears, review the listed locations and preview a specific folder when needed.

`Save settings` saves your selection and mode while keeping the current running or paused state. `Start automatic cleanup` enables automatic mode and saves your selection. If you paused the app earlier, choose `Resume` from the menu to continue processing.

`All user folders` discovers account homes, shared folders, and cloud document folders. Use `Selected folders` to choose particular locations. Your saved exclusions apply to both modes.

Hidden folders whose names begin with a dot, such as `.ghost-alice` and `.venv`, are excluded by default. Add a hidden folder directly under `Selected folders` to include it. Hidden subfolders inside that selection still follow the exclusion rule. You can save an empty folder list and add a folder when you are ready to begin cleanup.

### External drives and reconnecting

Use `Drives` to check selected drives and their connection state. Choose `Add a connected drive…`, then `Save settings`. Choose `Refresh drives` after connecting a drive.

In `All user folders`, choose `Include remembered drives` or `Choose drives manually` to manage your selection. A drive removed with `Remove selected drive` can be added again later.

Select a drive and choose `Manage selected drive…` to set `When this drive reconnects`.

| Choice | What happens on reconnect |
| --- | --- |
| `Default · Resume automatically` / `Resume automatically` | Cleanup continues when the app is running. The app's global pause remains in effect. |
| `Start manually` | Choose `Start this drive` in its details for each connection. If the app is paused, also choose `Resume`. |

Choose `Save` in the details. If your other folder edits need a preview, preview them and choose `Save settings` to apply the drive preference too. A manual start applies to that drive's current connection. Select a different drive separately even if it uses the same name.

Choose `Remove from list` in a drive's details when you finish using it, including while it is disconnected. With no other folder edits, removal is saved immediately. If your other folder edits need a preview, preview them and choose `Save settings` to apply the removal too. A saved exclusion remains in effect after reconnection. In `Selected folders`, the folders selected on that drive are also removed from the list.

## Check status and activity

Choose `View activity` in `Status` to see the folder or file being checked. `Renamed today` counts completed name changes using your Mac's calendar date. Restoring an original name later keeps the fact that it was changed that day.

`Activity` shows the current phase, path, completed checks and renamed items for this session, and the last progress time. A progress bar uses the total when it is known. While folder metadata is pending, check the waiting path and last progress time.

- Filter recent work with `All`, `Renames`, or `Issues`. `Issues` excludes entries with a recorded completion result.
- Enable `Follow new activity` to move to new entries. Turn it off while reading earlier entries.
- Choose `Open in separate window` to keep Activity beside another window.

Review task results and their recorded causes in `Recent activity`. Entries explain causes such as cloud storage, access permissions, or a delayed response. A later successful check updates the existing entry with its completion result. Open the entry to see the original cause and completion time. Use `Currently waiting` to see files and folders awaiting processing now.

Recent activity holds up to 200 entries, with older entries leaving the live list. Rechecking an already tidy file updates the current path and check count. Retries for the same issue update the count and time of its existing activity entry. Recent completed name changes are available in `History`. If live information is unavailable, choose `Refresh` to check the worker connection.

## Pause and quit

Choose `Pause` from the menu to put automatic cleanup on hold and `Resume` to continue. The pause remains saved across app launches. Changes detected while paused are processed after you resume.

Close or hide the window to leave background work running. Choose `Quit Jaso NFC` to stop the app and its work. Open it from Applications next time; if Status shows a start button, choose it to begin.

Use `Settings` → `Run at login` to choose whether the app starts at your next login.

<a id="recovery"></a>

## Search history and restore an original name

Recent history is retained within a storage budget, with older records retired automatically. Follow these steps to restore a retained item. Records needed by pending recovery are kept separately.

1. In `History`, search for a filename or folder.
2. Optionally enable `Date` and select a date, then narrow the result with `All results`, `Renamed`, or `Restored`.
3. Select a record to inspect `Original name`, `Changed name`, `Location`, and `Completed`. Use `Previous` and `Next` to move through pages of 50 records.
4. Choose `Restore original name` and read the availability check.
5. If the item can be restored, choose `Confirm restore` and wait for the completion message.

After the request is accepted, the existing background worker processes that item. A queued message means the result is still pending. Explicit restore requests can run while automatic cleanup is paused.

If the item moved or was replaced, or another item occupies the original name, the app explains why it cannot restore it. It also checks later related name changes and cloud items whose identity still needs verification. If recovery remains pending, retain the records and contact support.

A restored item keeps its original name while its path and file identity remain the same. Use your usual backup to restore file contents. For operations involving multiple retained records, see the [advanced recovery guide](installation.md#recovery).

## Settings and advanced settings

Choose Korean, English, or the system language in `Settings`; menus and windows update immediately. Choose an icon theme: `Quiet Gothic`, `Bookish Myeongjo`, or `Brushstroke Gungseo` on a Mac with that font available.

| Quiet Gothic | Bookish Myeongjo | Brushstroke Gungseo |
| --- | --- | --- |
| ![Gothic theme preview](assets/gothic.gif) | ![Myeongjo theme preview](assets/myungjo.gif) | ![Gungseo theme preview](assets/gungseo.gif) |

For a still icon, turn off `Animate menu bar icon` or enable macOS Reduce Motion. Command-comma opens Settings. Use Command-plus, Command-minus, and Command-zero to adjust content size.

`Advanced settings` includes `Check folders again`, `Restart cleanup`, `Copy diagnostic information`, access permissions, and `Storage usage…`. Checking folders again uses your saved selection and exclusions.

Choose `Open user guide…` at the bottom of `Settings` to return to these instructions in your selected app language.

### Check storage space

Open `Settings` → `Advanced settings` → `Storage usage…` to see Jaso data and free space on the disks storing it. Jaso data includes the file index, change history, and diagnostic logs.

A warning appears below 1 GiB available or below 5% free. Below 256 MiB, new name changes wait while existing pending recovery can finish. Choose `Open storage settings` to review files in macOS and free up space before continuing. Change history remains available for recovery. Unavailable measurements are shown as `Unavailable`.

<a id="permissions"></a>

## Permissions and troubleshooting

| Situation | What to check |
| --- | --- |
| Protected folder unavailable | Open `Settings` → `Advanced settings` → `Open Full Disk Access…`, add `/Applications/Jaso NFC.app` with `+`, and enable it. You can also drag the app from Finder into the list. Then choose `Restart cleanup`. |
| Background activity blocked | Open `Settings` → `Open Login Items…` and allow Jaso NFC's background activity. |
| Cloud item waiting | Check its path and reason in `Activity`. Open the location in Finder and check your sync app's login and connection. If a download is required, download the item in Finder. |
| External drive waiting | Connect it and check its state and reconnect preference in `Folders` → `Drives`. Choose `Start this drive` if configured for manual start. |
| Name conflict or locked item | Select the item under `Needs attention` in Status, then check its location, lock, and Sharing & Permissions in Finder. |
| An expected name was not cleaned up | Check automatic mode, pause state, selected folders, and exclusions. Review the [filename repair conditions](#filename-repair) for garbled names. Separately typed compatibility letters such as `ㅎㅏㄴ` are outside NFC normalization. |
| Low storage | Check the disk storing Jaso data under `Settings` → `Advanced settings` → `Storage usage…`. |

Jaso NFC uses the current account's access permissions. System Settings labels may differ slightly by macOS version. See the [detailed permission guide](installation.md#permissions).

## Scope and safety

Jaso NFC brings file and folder names into NFC form and applies [eligible encoding repairs](#filename-repair) to regular-file names. Folder and symbolic-link entry names receive NFC normalization. It processes items inside your selected folders. Selected top-level folder names, volume names, and symbolic-link targets are outside that scope.

Automatic exclusions include `.git`, AppleDouble companion files (`._*`), the app's state and log paths, and the contents of these packages: `.app`, `.photoslibrary`, `.musiclibrary`, `.tvlibrary`, `.aplibrary`, and `.framework`. Choose `Add exclusions…` in Folders to leave out other locations.

`All user folders` excludes home Library internals, Trash, and recognized system areas while discovering cloud document folders separately. Saved exclusions also apply.

Detected name conflicts are deferred and shown in Status. On some filesystems, including exFAT, another app creating the same destination concurrently can cause an overwrite. Avoid simultaneous renaming in these locations and review the [filesystem processing limits](architecture.md#mutation-and-recovery).

Processing settings, file lists, and change history are stored in `~/Library/Application Support/jaso-nfc` by default. Language, theme, and content size are saved in macOS preferences. Name changes inside a cloud folder may synchronize according to your sync app's settings.

## Update or remove

To update, run the installer from the new DMG using the same account. Your saved folders, settings, and change history remain available. Review them in `Status` and `Folders` afterward.

To remove the app, turn off `Run at login`, choose `Quit Jaso NFC`, and follow the [removal steps](installation.md#uninstall) to clear background startup registration. Then move Jaso NFC from Applications to Trash. Settings and change history remain available for recovery or reinstallation.

If you want to restore original filenames, use `History` before removing the app.

## Ask for help

Contact us through [GitHub issues](https://github.com/garlicvread/jaso-nfc/issues) or [aidall_manager@aidall.tech](mailto:aidall_manager@aidall.tech). Include the app version, macOS version, status message, and steps that lead to the problem. Use `Settings` → `Advanced settings` → `Copy diagnostic information`, then remove private filenames and paths before sharing.

[Installation and upgrades](installation.md) · [Architecture](architecture.md) · [Build and validation](development.md) · [MIT license](../LICENSE)
