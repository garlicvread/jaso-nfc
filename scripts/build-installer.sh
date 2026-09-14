#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
release=0
app_dir=
usage() { echo 'Usage: build-installer.sh [--release] [--app BUILT_APP]' >&2; exit 2; }
while [ "$#" -gt 0 ]; do
    case "$1" in
        --release) test "$release" = 0 || usage; release=1; shift ;;
        --app) test "$#" -ge 2 && test -z "$app_dir" && test -n "$2" || usage
               app_dir=$(CDPATH= cd -- "$2" && pwd); shift 2 ;;
        *) usage ;;
    esac
done
test "$(uname -s)" = Darwin || { echo 'The installer requires macOS.' >&2; exit 1; }
if [ "$release" = 1 ]; then
    python3 scripts/release_signing.py preflight --notary
fi
if [ -z "$app_dir" ]; then
    if [ "$release" = 1 ]; then
        sh scripts/build-native.sh --release
    else
        sh scripts/build-native.sh
    fi
    app_dir="$project_dir/dist/Jaso NFC.app"
fi
mkdir -p "$project_dir/build" "$project_dir/dist"
stage_dir=$(mktemp -d "$project_dir/build/installer.XXXXXX")
trap 'rm -rf "$stage_dir"' EXIT
trap 'exit 1' HUP INT TERM
if [ "$release" = 1 ]; then
    # Never sign or staple a caller's --app. Finish the payload before embedding it.
    ditto "$app_dir" "$stage_dir/Jaso NFC.app"
    app_dir="$stage_dir/Jaso NFC.app"
    JASO_RELEASE_APP="$app_dir" python3 native/tests/test_release_metadata.py
    python3 scripts/release_signing.py sign payload "$app_dir"
    receipts_dir=$(mktemp -d "$project_dir/build/notary.XXXXXX")
    python3 scripts/release_signing.py notarize payload "$app_dir" "$receipts_dir/payload"
else
    codesign --verify --deep --strict "$app_dir"
    JASO_RELEASE_APP="$app_dir" python3 native/tests/test_release_metadata.py
fi
volume_dir="$stage_dir/volume"
installer_app="$volume_dir/Install Jaso NFC.app"
mkdir -p "$installer_app/Contents/MacOS" "$installer_app/Contents/Resources"
clang -Os -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/macos/Installer.m -o "$installer_app/Contents/MacOS/Jaso NFC Installer"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/installer.m -o "$project_dir/build/installer-test"
"$project_dir/build/installer-test"
ditto "$app_dir" "$installer_app/Contents/Resources/Jaso NFC.app"
cp "$app_dir/Contents/Resources/JasoNFC.icns" "$installer_app/Contents/Resources/JasoNFC.icns"
python3 - "$app_dir" "$installer_app" <<'PY'
import pathlib, plistlib, sys
source, installer = map(pathlib.Path, sys.argv[1:])
info = plistlib.loads((source / 'Contents/Info.plist').read_bytes())
assert info['CFBundleIdentifier'] == 'io.github.garlicvread.jaso-nfc'
assert info['CFBundleExecutable'] == 'jaso-nfc'
metadata = {
    'CFBundleIdentifier': 'io.github.garlicvread.jaso-nfc.installer',
    'CFBundleName': 'Install Jaso NFC',
    'CFBundleDisplayName': 'Install Jaso NFC',
    'CFBundleExecutable': 'Jaso NFC Installer',
    'CFBundlePackageType': 'APPL',
    'CFBundleIconFile': 'JasoNFC.icns',
    'CFBundleShortVersionString': info['CFBundleShortVersionString'],
    'CFBundleVersion': info['CFBundleVersion'],
    'LSMinimumSystemVersion': '13.0',
    'NSHighResolutionCapable': True,
    'NSHumanReadableCopyright': info['NSHumanReadableCopyright'],
    'JasoPublisher': info['JasoPublisher'],
    'JasoContributor': info['JasoContributor'],
    'JasoContributorEmail': info['JasoContributorEmail'],
    'JasoSupportEmail': info['JasoSupportEmail'],
    'JasoSourceURL': info['JasoSourceURL'],
}
(installer / 'Contents/Info.plist').write_bytes(plistlib.dumps(metadata))
PY
cp LICENSE "$volume_dir/LICENSE.txt"
cat > "$volume_dir/READ ME FIRST.txt" <<'TXT'
Jaso NFC — 시작 안내 / Getting started

1. Install Jaso NFC.app을 더블클릭하고 ‘설치’를 누르세요.
2. 설치가 완료되면 원본 DMG를 유지하거나 ‘설치 파일 휴지통 이동’을 누르세요.
3. 설치 창을 닫고 Finder에서 이 디스크를 추출하세요.
4. 메뉴 막대에서 ‘Jaso NFC 열기…’를 선택하고 창 왼쪽의 ‘폴더’를 누르세요.
5. 정리할 폴더를 고르고 ‘파일명 미리보기’로 바뀔 이름을 확인하세요.
6. ‘자동 정리 시작’을 누르세요.

처음 설치하면 다운로드 폴더부터 시작할 수 있습니다.
업그레이드하면 기존 폴더 설정과 이름 변경 기록을 이어서 사용합니다.
설치된 앱은 응용 프로그램 폴더에 있습니다.

1. Double-click Install Jaso NFC.app and click Install.
2. After installation, choose Keep Installer or Move Installer to Trash for the original DMG.
3. Close the installer and eject this disk in Finder.
4. In the menu bar, choose Open Jaso NFC…, then choose Folders in the sidebar.
5. Choose folders and click Preview filenames to review the proposed names.
6. Click Start automatic cleanup.

Start with Downloads on a fresh installation. Upgrades keep your existing
folder settings and rename history. The installed app is in Applications.

설치·사용 안내 / Installation and help
https://garlicvread.github.io/jaso-nfc/

게시자 / Publisher: AidALL Inc.
기여자 / Contributor: garlicvread <ceo@aidall.tech>
문의 / Support: aidall_manager@aidall.tech
소스 / Source: https://github.com/garlicvread/jaso-nfc
Copyright 2026 jaso-nfc contributors. License: MIT (see LICENSE.txt).

TXT
if [ "$release" = 1 ]; then
    python3 scripts/release_signing.py sign installer "$installer_app"
else
    codesign --force --sign - --identifier io.github.garlicvread.jaso-nfc.installer "$installer_app"
    codesign --verify --deep --strict "$installer_app"
fi
version=$(python3 -c 'import plistlib, sys; print(plistlib.load(open(sys.argv[1], "rb"))["CFBundleShortVersionString"])' "$app_dir/Contents/Info.plist")
architecture=$(lipo -archs "$app_dir/Contents/MacOS/jaso-nfc" | tr ' ' '-')
case "$version-$architecture" in *[!0-9A-Za-z.-]*) echo 'Invalid package version or architecture.' >&2; exit 1 ;; esac
suffix=-local
if [ "$release" = 1 ]; then suffix=; fi
dmg_name="Jaso-NFC-$version-$architecture$suffix.dmg"
dmg_path="$stage_dir/$dmg_name"
hdiutil create -quiet -ov -format UDZO -fs HFS+ -volname "Jaso NFC $version Installer" -srcfolder "$volume_dir" "$dmg_path"
if [ "$release" = 1 ]; then
    python3 scripts/release_signing.py sign dmg "$dmg_path"
    python3 scripts/release_signing.py notarize dmg "$dmg_path" "$receipts_dir/dmg"
fi
(
    cd "$stage_dir"
    shasum -a 256 "$dmg_name" > "$dmg_name.sha256"
)
JASO_INSTALLER_DMG="$dmg_path" JASO_INSTALLER_PAYLOAD="$app_dir" JASO_RELEASE_MODE="$release" python3 native/tests/test_installer_package.py
# Publishable names are only populated after every packaging/release check passes.
mv "$dmg_path.sha256" "$project_dir/dist/$dmg_name.sha256"
mv "$dmg_path" "$project_dir/dist/$dmg_name"
printf '%s\n' "$project_dir/dist/$dmg_name"
