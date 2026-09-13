"""Run the keyboard fixture as an app when desktop activation needs a click."""
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys

binary = Path(sys.argv[1]).resolve()
app = binary.parent / "MenuInterfaceTest.app"
macos = app / "Contents/MacOS"
macos.mkdir(parents=True, exist_ok=True)
shutil.copy2(binary, macos / "MenuInterfaceTest")
with (app / "Contents/Info.plist").open("wb") as stream:
    plistlib.dump(dict(CFBundleExecutable="MenuInterfaceTest",
        CFBundleIdentifier="tech.aidall.jaso-nfc.menu-interface-test",
        CFBundleName="Jaso Interface Test", CFBundlePackageType="APPL"), stream)
output = binary.parent / "menu-interface-desktop.out"
errors = binary.parent / "menu-interface-desktop.err"
output.write_text("")
errors.write_text("")
print(f"Activate the Jaso Interface Test window to verify keyboard shortcuts: {app}", flush=True)
subprocess.run(["open", "-n", "-W", "--stdout", str(output), "--stderr", str(errors),
    str(app), "--args", "--wait-for-activation"], check=True, timeout=180)
text = output.read_text()
print(text, end="")
print(errors.read_text(), end="", file=sys.stderr)
result = re.search(r"(\d+) menu interface cases, (\d+) failures", text)
if not result or int(result[1]) == 0 or int(result[2]) != 0:
    raise SystemExit(1)
