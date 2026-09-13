#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
test "$(uname -s)" = Darwin || { echo 'The installer requires macOS.' >&2; exit 1; }
case "$#" in
    0) sh scripts/build-native.sh; app_dir="$project_dir/dist/Jaso NFC.app" ;;
    2) test "$1" = --app || { echo 'Usage: build-installer.sh [--app BUILT_APP]' >&2; exit 2; }
       app_dir=$(CDPATH= cd -- "$2" && pwd) ;;
    *) echo 'Usage: build-installer.sh [--app BUILT_APP]' >&2; exit 2 ;;
esac
codesign --verify --deep --strict "$app_dir"
JASO_RELEASE_APP="$app_dir" python3 native/tests/test_release_metadata.py
mkdir -p "$project_dir/build" "$project_dir/dist"
stage_dir=$(mktemp -d "$project_dir/build/installer.XXXXXX")
trap 'rm -rf "$stage_dir"' EXIT HUP INT TERM
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
codesign --force --sign - --identifier io.github.garlicvread.jaso-nfc.installer "$installer_app"
codesign --verify --deep --strict "$installer_app"
version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app_dir/Contents/Info.plist")
architecture=$(lipo -archs "$app_dir/Contents/MacOS/jaso-nfc" | tr ' ' '-')
case "$version-$architecture" in *[!0-9A-Za-z.-]*) echo 'Invalid package version or architecture.' >&2; exit 1 ;; esac
dmg_path="$project_dir/dist/Jaso-NFC-$version-$architecture-local.dmg"
hdiutil create -quiet -ov -format UDZO -fs HFS+ -volname "Jaso NFC $version Installer" -srcfolder "$volume_dir" "$stage_dir/installer.dmg"
mv "$stage_dir/installer.dmg" "$dmg_path"
(
    cd "$project_dir/dist"
    shasum -a 256 "${dmg_path##*/}" > "${dmg_path##*/}.sha256"
)
JASO_INSTALLER_DMG="$dmg_path" JASO_INSTALLER_PAYLOAD="$app_dir" python3 native/tests/test_installer_package.py
printf '%s\n' "$dmg_path"
