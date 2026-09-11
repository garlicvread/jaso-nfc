#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
test "$(uname -s)" = Darwin || { echo 'The native application requires macOS.' >&2; exit 1; }
cargo build --release --features runtime --locked
app_dir="$project_dir/dist/Jaso NFC.app"
mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources" "$project_dir/build"
cp target/release/jaso-nfc "$app_dir/Contents/MacOS/jaso-nfc"
cp native/macos/Info.plist "$app_dir/Contents/Info.plist"
clang -Os -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/macos/Menu.m native/macos/AnimatedMark.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m native/macos/SetupWindow.m -o "$app_dir/Contents/MacOS/Jaso NFC"
clang -Os -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/macos/RenderIcon.m native/macos/AnimatedMark.m -o "$project_dir/build/RenderIcon"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/tests/menu_startup.m native/macos/AnimatedMark.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m native/macos/SetupWindow.m -o "$project_dir/build/menu-startup-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/setup_window.m native/macos/SetupWindow.m native/macos/ContentZoom.m native/macos/Localization.m -o "$project_dir/build/setup-window-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework Foundation native/tests/status_presentation.m native/macos/StatusPresentation.m -o "$project_dir/build/status-presentation-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/status_window.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m -o "$project_dir/build/status-window-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/settings_window.m native/macos/ContentZoom.m native/macos/SettingsWindow.m native/macos/Localization.m -o "$project_dir/build/settings-window-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework QuartzCore -framework CoreText -framework ImageIO native/tests/menu_interface.m native/macos/AnimatedMark.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/Localization.m native/macos/SettingsWindow.m native/macos/SetupWindow.m -o "$project_dir/build/menu-interface-test"
clang -O0 -g -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit native/tests/content_zoom.m native/macos/ContentZoom.m native/macos/StatusWindow.m native/macos/StatusPresentation.m native/macos/SettingsWindow.m native/macos/Localization.m -o "$project_dir/build/content-zoom-test"
"$project_dir/build/menu-startup-test"
"$project_dir/build/menu-interface-test"
"$project_dir/build/content-zoom-test" "$project_dir/build/content-zoom-previews"
"$project_dir/build/settings-window-test" "$project_dir/build/settings-previews"
"$project_dir/build/setup-window-test" "$project_dir/build/setup-previews"
"$project_dir/build/status-presentation-test"
"$project_dir/build/status-window-test" "$project_dir/build/status-previews"
"$project_dir/build/RenderIcon" --self-test
"$project_dir/build/RenderIcon" --iconset "$project_dir/build/JasoNFC.iconset"
iconutil -c icns "$project_dir/build/JasoNFC.iconset" -o "$app_dir/Contents/Resources/JasoNFC.icns"
if [ -n "${JASO_ICON_PREVIEW_DIR:-}" ]; then
    "$project_dir/build/RenderIcon" --preview "$JASO_ICON_PREVIEW_DIR"
fi
# Sign nested code first, then let the app signature cover its Rust main.
codesign --force --sign - --identifier io.github.garlicvread.jaso-nfc.menu "$app_dir/Contents/MacOS/Jaso NFC"
codesign --force --sign - --identifier io.github.garlicvread.jaso-nfc "$app_dir"
codesign --verify --deep --strict "$app_dir"
JASO_NATIVE_BINARY="$app_dir/Contents/MacOS/jaso-nfc" python3 native/tests/test_app_trampoline.py
JASO_NATIVE_BINARY="$app_dir/Contents/MacOS/jaso-nfc" python3 native/tests/test_installed_runtime.py
printf '%s\n' "$app_dir"
