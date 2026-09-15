#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
release=0
case "$#" in
    0) ;;
    1) test "$1" = --release || { echo 'Usage: build-native.sh [--release]' >&2; exit 2; }
       release=1 ;;
    *) echo 'Usage: build-native.sh [--release]' >&2; exit 2 ;;
esac
test "$(uname -s)" = Darwin || { echo 'The native application requires macOS.' >&2; exit 1; }
if [ "$release" = 1 ]; then
    python3 scripts/release_signing.py preflight
fi
cargo build --release --features runtime --locked
app_dir="$project_dir/dist/Jaso NFC.app"
mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources" "$project_dir/build"
cp target/release/jaso-nfc "$app_dir/Contents/MacOS/jaso-nfc"
cp native/macos/Info.plist "$app_dir/Contents/Info.plist"
cp LICENSE "$app_dir/Contents/Resources/LICENSE.txt"
clang -Os -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/macos/Menu.m native/macos/WorkspaceWindow.m native/macos/NamePresentation.m native/macos/AnimatedMark.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m native/macos/SetupWindow.m -o "$app_dir/Contents/MacOS/Jaso NFC"
clang -Os -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/macos/RenderIcon.m native/macos/AnimatedMark.m -o "$project_dir/build/RenderIcon"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/tests/menu_startup.m native/macos/WorkspaceWindow.m native/macos/NamePresentation.m native/macos/AnimatedMark.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m native/macos/SetupWindow.m -o "$project_dir/build/menu-startup-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/setup_window.m native/macos/SetupWindow.m native/macos/ContentZoom.m native/macos/Localization.m -o "$project_dir/build/setup-window-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework Foundation native/tests/status_presentation.m native/macos/StatusPresentation.m -o "$project_dir/build/status-presentation-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework Foundation native/tests/name_presentation.m native/macos/NamePresentation.m -o "$project_dir/build/name-presentation-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/status_window.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m -o "$project_dir/build/status-window-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/settings_window.m native/macos/ContentZoom.m native/macos/SettingsWindow.m native/macos/Localization.m -o "$project_dir/build/settings-window-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/tests/menu_interface.m native/macos/WorkspaceWindow.m native/macos/NamePresentation.m native/macos/AnimatedMark.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m native/macos/SetupWindow.m -o "$project_dir/build/menu-interface-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/content_zoom.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/SettingsWindow.m native/macos/Localization.m -o "$project_dir/build/content-zoom-test"
clang -fobjc-arc -framework AppKit native/tests/workspace_window.m native/macos/WorkspaceWindow.m native/macos/NamePresentation.m native/macos/Localization.m native/macos/ContentZoom.m native/macos/StatusPresentation.m -o "$project_dir/build/workspace-window-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/workspace_layout.m native/macos/WorkspaceWindow.m native/macos/NamePresentation.m native/macos/SetupWindow.m native/macos/SettingsWindow.m native/macos/ContentZoom.m native/macos/Localization.m native/macos/StatusPresentation.m -o "$project_dir/build/workspace-layout-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/workspace_runtime.m native/macos/WorkspaceWindow.m native/macos/NamePresentation.m native/macos/ContentZoom.m native/macos/Localization.m native/macos/StatusPresentation.m -o "$project_dir/build/workspace-runtime-test"
clang -O0 -g -Wall -Wextra -Werror native/tests/query_worker.c -o "$project_dir/build/query-worker-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/tests/menu_queries.m native/macos/WorkspaceWindow.m native/macos/NamePresentation.m native/macos/AnimatedMark.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m native/macos/SetupWindow.m -o "$project_dir/build/menu-queries-test"
"$project_dir/build/workspace-runtime-test"
"$project_dir/build/menu-queries-test" "$project_dir/build/query-worker-test"
"$project_dir/build/workspace-window-test" "$project_dir/build/workspace-previews"
"$project_dir/build/workspace-layout-test" "$project_dir/build/layout-previews"
"$project_dir/build/menu-startup-test"
if [ "${JASO_INTERACTIVE_TESTS:-0}" = 1 ]; then
    python3 native/tests/run_menu_interface.py "$project_dir/build/menu-interface-test"
else
    "$project_dir/build/menu-interface-test"
fi
"$project_dir/build/content-zoom-test" "$project_dir/build/content-zoom-previews"
"$project_dir/build/settings-window-test" "$project_dir/build/settings-previews"
"$project_dir/build/setup-window-test" "$project_dir/build/setup-previews"
"$project_dir/build/status-presentation-test"
"$project_dir/build/name-presentation-test"
"$project_dir/build/status-window-test" "$project_dir/build/status-previews"
clang -fobjc-arc -framework AppKit native/tests/embedded_settings.m native/macos/SetupWindow.m native/macos/SettingsWindow.m native/macos/ContentZoom.m native/macos/Localization.m -o "$project_dir/build/embedded-settings-test"
"$project_dir/build/embedded-settings-test"
"$project_dir/build/RenderIcon" --self-test
"$project_dir/build/RenderIcon" --iconset "$project_dir/build/JasoNFC.iconset"
iconutil -c icns "$project_dir/build/JasoNFC.iconset" -o "$app_dir/Contents/Resources/JasoNFC.icns"
if [ -n "${JASO_ICON_PREVIEW_DIR:-}" ]; then
    "$project_dir/build/RenderIcon" --preview "$JASO_ICON_PREVIEW_DIR"
fi
# The next two suites copy executables and rewrite bundle metadata, so run them
# with the local ad hoc signature before sealing the release with Developer ID.
codesign --force --sign - --identifier io.github.garlicvread.jaso-nfc.menu "$app_dir/Contents/MacOS/Jaso NFC"
codesign --force --sign - --identifier io.github.garlicvread.jaso-nfc "$app_dir"
codesign --verify --deep --strict "$app_dir"
JASO_NATIVE_BINARY="$app_dir/Contents/MacOS/jaso-nfc" python3 native/tests/test_app_trampoline.py
JASO_NATIVE_BINARY="$app_dir/Contents/MacOS/jaso-nfc" python3 native/tests/test_installed_runtime.py
if [ "$release" = 1 ]; then
    # The helper signs nested code first, then the app, and verifies the final
    # signer, team, hardened runtime and secure timestamps. Smoke the intact app.
    python3 scripts/release_signing.py sign payload "$app_dir"
    "$app_dir/Contents/MacOS/jaso-nfc" --version
    "$app_dir/Contents/MacOS/jaso-nfc" --help
fi
# Workspace checks execute the intact binary in place, after final release signing.
JASO_NATIVE_BINARY="$app_dir/Contents/MacOS/jaso-nfc" python3 native/tests/test_workspace_cli.py
printf '%s\n' "$app_dir"
