# Release signing and notarization

This guide is for maintainers who produce a Developer ID signed and notarized DMG of Jaso NFC on a company-controlled Mac from a reviewed commit. It complements [development.md](development.md) (build and validation), [installation.md](installation.md) (installed layout, permissions, upgrades), and [architecture.md](architecture.md).

Local development builds are unchanged: they remain ad hoc signed and need no Apple account.

Real company signing and notarization are NOT-VERIFIED in this change. The public download links still point to the 0.2.1 `-local` package, and the Gatekeeper guidance in [macOS blocks the installer](installation.md#macos-blocks-the-installer) describes that package correctly.

## Local and release modes

Local mode is the default; release mode must be requested explicitly with `--release`. Release mode fails when an input is missing or wrong. It never falls back to an ad hoc signature. The release file name has no `-local` suffix, so it never collides with a local package of the same version.

| | Local mode (default) | Release mode |
| --- | --- | --- |
| Commands | `sh scripts/build-native.sh`, `sh scripts/build-installer.sh [--app PATH]` | `sh scripts/build-native.sh --release`, `sh scripts/build-installer.sh --release [--app PATH]` |
| Code signature | ad hoc | Developer ID Application with hardened runtime and secure timestamp |
| Notarization | none | payload app and DMG, each notarized and stapled |
| Required inputs | none | `JASO_SIGNING_IDENTITY`, `JASO_TEAM_ID`; the installer build also needs `JASO_NOTARY_PROFILE` |
| Output | `dist/Jaso-NFC-<version>-<architecture>-local.dmg` and its `.sha256` | `dist/Jaso-NFC-<version>-<architecture>.dmg` and its `.sha256` |
| Where it runs | any development Mac, CI | the release Mac, from a reviewed commit |

## Account and certificate

Release signing requires membership in the company's Apple Developer Program team, the team's Team ID, a Developer ID Application certificate with its private key in the login Keychain of the release Mac, and a notary profile. Developer ID Application signs all three release items: the payload app, the installer app, and the DMG. A Developer ID Installer certificate is for `.pkg` installer packages and is not used by this project.

Creating a Developer ID certificate in the developer account is an Account Holder action; see Apple's [Create Developer ID certificates](https://developer.apple.com/help/account/certificates/create-developer-id-certificates). Apple's cloud-managed Developer ID certificate option is a separate workflow with its own explicitly granted permission. A team member's Admin role by itself does not imply the ability to create this certificate; coordinate with the Account Holder.

The certificate's private key is provisioned to the release Mac through the company's normal secure process and stays in the Keychain. It is never exported into the repository, a pull request, CI configuration, logs, or chat.

The exact identity name is listed by the command below. Use the full `Developer ID Application: <Company> (<Team ID>)` string; the Team ID is the value in parentheses and also appears on the developer account's membership page.

```sh
security find-identity -v -p codesigning
```

## Notary profile

`notarytool` authenticates with a Keychain profile created once with the command below. The profile name (here `jaso-nfc-notary`) is the only value that later goes into the environment.

Leave `--password` out: when the Apple ID and Team ID are given and `--password` is omitted, `notarytool` prompts securely on the command line. The password is an app-specific password for the Apple ID (see Apple's [Using app-specific passwords](https://support.apple.com/en-us/HT204397)). Never type it on the command line, store it in a script, the repository, or an environment variable, or paste it into chat.

By default, `store-credentials` validates the credential with Apple before saving it.

An App Store Connect API key (`--key`, `--key-id`, `--issuer`) can be stored in the profile instead of an Apple ID; the key file is handled with the same care as the private key. Replace the Apple ID and Team ID placeholders with the company values.

```sh
xcrun notarytool store-credentials "jaso-nfc-notary" --apple-id "<apple-id@example.com>" --team-id ABCDE12345
```

## Release inputs

The table below lists each environment variable, its value, and which command requires it. Only names and identifiers go into the environment: an identity name, a Team ID, and a profile name. No password, private key, or API key.

| Variable | Value | Required by |
| --- | --- | --- |
| `JASO_SIGNING_IDENTITY` | the Developer ID Application identity name as listed by `security find-identity -v -p codesigning` | `build-native.sh --release`, `build-installer.sh --release` |
| `JASO_TEAM_ID` | the expected Team ID; the signer's team must match exactly | `build-native.sh --release`, `build-installer.sh --release` |
| `JASO_NOTARY_PROFILE` | the notary profile name created with `store-credentials` | `build-installer.sh --release` |

Both scripts check their inputs before building anything. A missing or empty variable, an identity that is not a Developer ID Application identity, or a signer whose team differs from `JASO_TEAM_ID` stops the build with an error. There is no silent fallback to local mode.

`ABCDE12345` and `Example Company, Inc.` are placeholders; use the values shown by `security find-identity`.

```sh
export JASO_SIGNING_IDENTITY='Developer ID Application: Example Company, Inc. (ABCDE12345)'
export JASO_TEAM_ID=ABCDE12345
export JASO_NOTARY_PROFILE=jaso-nfc-notary
```

## Build the release

Preconditions on the release Mac: macOS 13 or later, stable Rust, Python 3.11 or later (as in [development.md](development.md)), an Xcode installation or Command Line Tools that provide `xcrun notarytool` and `xcrun stapler` (Apple's notary service stopped accepting `altool` and Xcode 13 or earlier on November 1, 2023), and network access for the secure timestamp, the notary service, and stapling.

Check out the reviewed commit in a clean working tree. The version and metadata in `Cargo.toml`, `native/macos/Info.plist`, and `pyproject.toml` must agree; the installer build runs `native/tests/test_release_metadata.py` against the payload app, and the release keeps the existing bundle identifier `io.github.garlicvread.jaso-nfc`, the MIT license, the copyright notice, and the publisher and contributor metadata.

Run the two commands in this order:

```sh
sh scripts/build-native.sh --release
sh scripts/build-installer.sh --release --app 'dist/Jaso NFC.app'
```

`sh scripts/build-native.sh --release` performs the local build and its checks, then signs the GUI executable `Contents/MacOS/Jaso NFC` first and the payload app second so that the app signature covers the Rust main executable `Contents/MacOS/jaso-nfc`. It uses the Developer ID Application identity with the hardened runtime and a secure timestamp on every executable, and verifies the signer, the exact Team ID, the runtime flag, and the timestamp. `--deep` is used only for verification, never for signing. Output: `dist/Jaso NFC.app`.

`sh scripts/build-installer.sh --release --app 'dist/Jaso NFC.app'` requires a trusted payload built from the reviewed source; the input need not already carry the chosen Developer ID signature. The script copies the app to its own staging directory, adds `Contents/Resources/LICENSE.txt`, then signs and verifies that copy with the configured signer, Team ID, runtime, and timestamp before the first notarization. The original `--app` input is preserved. When `--app` is omitted, the installer build runs the native build in release mode first, as the local command does in local mode.

The ordered signing and notarization sequence proceeds as follows:

1. Round one (payload app): the script compresses its staging copy of the payload app into a ZIP for transport, submits it with `xcrun notarytool submit --keychain-profile "$JASO_NOTARY_PROFILE" --wait`, requires the status `Accepted` (a zero exit status with any other result is a failure), staples the ticket to the staged payload app, and validates the staple.

2. Embedding: the stapled payload app is copied to `Install Jaso NFC.app/Contents/Resources/Jaso NFC.app`. Nothing in the payload changes after this point, because the installer signature seals the contents of its Resources directory.

3. The installer app (`io.github.garlicvread.jaso-nfc.installer`) is signed with the same identity, hardened runtime, and timestamp, then verified.

4. The DMG is created and signed with the Developer ID Application identity and a timestamp.

5. Round two (DMG): the DMG is submitted, must be `Accepted`, is stapled, and is validated.

6. The SHA-256 checksum is computed last, after stapling, because stapling changes the DMG bytes. The final files are `dist/Jaso-NFC-<version>-<architecture>.dmg` and `dist/Jaso-NFC-<version>-<architecture>.dmg.sha256`; the script prints the DMG path as its last line. Both files pass packaging checks before replacement. Existing outputs must be a complete regular-file pair or both absent; symlink and directory destinations are refused without replacing them.

Publication keeps hard-link backups of the prior pair in a private `dist/.installer-publish.XXXXXX` directory until both final moves finish. An I/O error or handled HUP, INT, or TERM during replacement removes the new files and restores the prior pair (or leaves neither final file when there was no prior pair). If rollback itself fails, the script reports and retains the recovery directory with both backups; staging cleanup does not delete it. Resolve the I/O problem and recover the pair before publishing. Run only one publisher per output pair, and read or upload the files only after a successful exit: the two final names do not change atomically for concurrent readers. Uncatchable termination (including SIGKILL), machine failure, and concurrent modification of `dist` are outside this rollback guarantee.

Why two rounds: Apple requires a custom third-party installer to notarize and staple the installer's payload first and then notarize the packaged installer ([Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)). For Jaso NFC this matters concretely: the installer app runs the payload's `jaso-nfc` executable from the mounted DMG, and the installed `/Applications/Jaso NFC.app` is a byte-identical copy of the payload. Stapling the DMG does not staple the app nested inside it, so the payload must carry its own ticket for launches after the DMG is ejected, including offline.

The helper prints each receipt directory, not the raw submission ID. It automatically saves records under `build/notary.XXXXXX/payload` and `build/notary.XXXXXX/dmg`: `submission.json`, `submission.stderr.txt`, the retrieved `log.json` and `log-output.txt`, and `staple.txt` and `validate.txt` as those steps run. Failed runs retain the records reached before failure. Read `log.json` even for an `Accepted` submission, because it can list warnings; no duplicate manual log retrieval is needed. Logs are not a credential store; inspect them before sharing. No hardened-runtime entitlement exceptions are expected for this app; if the log or a test launch reports one, find the cause instead of adding an entitlement.

## Verify on the release Mac

The release scripts already verify signatures and staples; the commands below let the operator inspect the finished DMG independently. Replace `0.2.1` and `arm64` with the version and architecture in the printed DMG path.

```sh
dmg=dist/Jaso-NFC-0.2.1-arm64.dmg
(cd dist && shasum -a 256 -c Jaso-NFC-0.2.1-arm64.dmg.sha256)
xcrun stapler validate "$dmg"
spctl --assess --type open --context context:primary-signature --verbose "$dmg"
mkdir -p build/release-check
hdiutil attach -readonly -nobrowse -mountpoint build/release-check "$dmg"
installer='build/release-check/Install Jaso NFC.app'
codesign --verify --deep --strict --verbose=2 "$installer"
codesign --display --verbose=2 "$installer/Contents/Resources/Jaso NFC.app"
xcrun stapler validate "$installer/Contents/Resources/Jaso NFC.app"
spctl --assess --type execute --verbose "$installer"
hdiutil detach build/release-check
```

Expected results: the checksum line reports `OK`; `stapler validate` reports that the validate action worked for both the DMG and the nested payload app; `spctl` reports `accepted` with `source=Notarized Developer ID` for the DMG and for the installer app; `codesign --display --verbose=2` shows an `Authority=Developer ID Application:` line, `TeamIdentifier=` followed by the Team ID, `flags=0x10000(runtime)`, and a `Timestamp=` line.

The nested payload app is checked inside the mounted DMG because that copy, not `dist/Jaso NFC.app`, is the stapled one.

These checks are static. They prove the artifact's signature and ticket on this Mac; they do not prove that a downloaded copy installs and runs on a user's Mac. That is the next section.

## Test the download on a test Mac

Use a test Mac with default Gatekeeper settings and no developer certificates: one with an existing 0.2.1 installation for the upgrade check, and one without prior Jaso NFC state for the clean-install check; notary acceptance is not proof of any item below.

1. Before public release, make the candidate DMG and its `.sha256` available at a controlled release-candidate download URL. Download them through a browser so the DMG carries the quarantine attribute. Confirm the attribute and the checksum with the commands below:

   ```sh
   cd ~/Downloads
   xattr -p com.apple.quarantine Jaso-NFC-0.2.1-arm64.dmg
   shasum -a 256 -c Jaso-NFC-0.2.1-arm64.dmg.sha256
   ```

2. Open the DMG and double-click `Install Jaso NFC.app`. Expected: it opens with at most the standard confirmation for a downloaded app; it must not require `Open Anyway` in System Settings and must not show a damaged-file message. The installer runs the payload's `jaso-nfc` executable from the mounted DMG, so this step also exercises the payload signature.

3. Complete the installation, choose the Trash option or keep the DMG, close the installer, and eject the disk. Validate the ticket on the installed copy:

   ```sh
   xcrun stapler validate '/Applications/Jaso NFC.app'
   ```

   Expected: validation succeeds. Then disconnect from the network and open `/Applications/Jaso NFC.app`; record whether it launches offline. An online first installation or launch can cache Gatekeeper decisions, so this offline launch only records the observed behavior and alone does not prove that the ticket was copied.

4. Upgrade check on the Mac that had 0.2.1: after installation, the saved folders, exclusions, rename history in `History`, and login preference are still present, `Status` shows the prior state, and the worker is running. `startup status` in [worker and login controls](installation.md#worker-and-login-controls) reports the saved preference.

5. Full Disk Access: the signing identity changes from ad hoc to Developer ID, so macOS can treat the release build as a different program. Check whether the existing Full Disk Access grant still applies; if the app reports access errors, follow [permissions](installation.md#permissions) to re-add `/Applications/Jaso NFC.app` and record that this step was needed so the release notes can say so.

6. Login: record the saved `startup status`, then log out and back in. If startup is enabled, expect the worker's LaunchAgent to start and the menu app to appear, with Jaso NFC allowed in the background under Login Items. If startup is disabled, it must remain disabled and the worker and menu app must not start automatically. Check both saved preferences across upgrade tests.

7. Clean install on the Mac without prior state: expected a fresh installation that suggests Downloads, as described in [installation.md](installation.md).

8. Record the macOS versions, the exact dialog texts, and each result in the release notes or the release pull request.

## Publish

This change does not publish a release. After the release-Mac and controlled-download checks pass and the operator explicitly decides to release, create the GitHub release for the tag `v<version>` manually and upload the verified DMG and its `.sha256` from the release Mac.

Once the notarized artifact is available at its public URL, replace the download links, which currently point at `Jaso-NFC-0.2.1-arm64-local.dmg`, in `README.md`, `README.en.md`, `docs/usage.md`, `docs/usage.en.md`, `docs/installation.md`, `site/index.html`, and `site/en/index.html`, then run `python3 scripts/check-public-docs.py`. The checker accepts both local and signed artifact names; no filename rule edit is needed. Revise [macOS blocks the installer](installation.md#macos-blocks-the-installer) and the matching user-guide section at the same time, since a notarized package no longer needs that workaround.

Until then, keep the `-local` links and the existing Gatekeeper guidance; they are correct for the package users can download today.

## Boundaries

No secrets in the repository, pull requests, CI jobs, or logs. `.github/workflows/tests.yml` builds in local mode only. Pull request builds from any branch are never signed with company credentials.

A CI release workflow is optional future work: if adopted, it would run only in a protected environment with the credentials as protected secrets and only from reviewed source. It is not part of this change.

Keep the bundle identifiers `io.github.garlicvread.jaso-nfc`, `io.github.garlicvread.jaso-nfc.menu`, and `io.github.garlicvread.jaso-nfc.installer`; the runtime, the installer, and the tests check them. Keep the MIT license, the copyright notice, and the publisher and contributor metadata; `native/tests/test_release_metadata.py` and the installer package test check them.

`python3 native/tests/test_release_signing.py` exercises the release preflight and sequencing with stubbed signing and notary tools. It needs no certificate or notary profile, and passing it is not evidence that a real artifact was signed or accepted.

NOT-VERIFIED in this change: company team membership and roles, the certificate and notary profile, real signing and notary acceptance, installed-app staple validation, Gatekeeper and offline-launch behavior, Full Disk Access continuity after the identity change, upgrade from 0.2.1, and login behavior for enabled and disabled startup preferences. These require operator evidence from the release setup and a real artifact tested with the checklist above. This change makes no claim about private artifacts outside its evidence. The Apple notary service and Gatekeeper requirements are summarized in Apple's [Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).
