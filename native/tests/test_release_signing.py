"""Credential-free release tests. No company signing, credentials, or notary service.

Task-owned PATH tools emulate external operations, including deliberately invalid
success responses. The shell integration fixture emulates compilation and DMGs;
test_installer_package.py separately checks a real built disk image on macOS.
The macOS requirement regression uses real codesign on an owned ad hoc fixture.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

PROJECT = Path(__file__).resolve().parents[2]
TEAM = "ABCDE12345"
IDENTITY = f"Developer ID Application: Fixture Company ({TEAM})"

# ReleaseSigning shadows every signing/notary command, even in missing-input tests.
# No fixture switch is implemented in production scripts.
TOOL = r'''
import hashlib, json, os, pathlib, plistlib, shutil, signal, subprocess, sys, zipfile
tools = pathlib.Path(__file__).resolve().parent
state = json.loads((tools / "state.json").read_text())
command, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with (tools / "events.jsonl").open("a") as stream:
    stream.write(json.dumps([command, *args]) + "\n")
identity = "Developer ID Application: Fixture Company (ABCDE12345)"
if command == "uname":
    print("Darwin")
elif command == "security":
    if state.get("identity_available", True):
        print('  1) ' + 'A' * 40 + ' "' + identity + '"')
elif command == "codesign":
    if "--sign" in args:
        path = pathlib.Path(args[-1])
        if path.is_dir():
            info = plistlib.loads((path / "Contents/Info.plist").read_bytes())
            path = path / "Contents/MacOS" / info["CFBundleExecutable"]
        with path.open("ab") as stream:
            stream.write(b"\nSIGNATURE FIXTURE\n")
    elif "--display" in args:
        bad = state.get("bad_metadata")
        arch = args[args.index("--architecture") + 1] if "--architecture" in args else None
        if state.get("bad_architecture") and arch != state["bad_architecture"]:
            bad = None
        if bad != "adhoc":
            print("Authority=" + ("Developer ID Installer: Fixture Company (ABCDE12345)"
                                  if bad == "signer" else identity))
            print("TeamIdentifier=" + ("WRONG12345" if bad == "team" else "ABCDE12345"))
        else:
            print("Signature=adhoc\nTeamIdentifier=not set")
        print("CodeDirectory v=20500 flags=" + ("0x0(none)" if bad == "runtime" else "0x10000(runtime)"))
        if bad != "timestamp":
            print("Timestamp=Sep 14, 2026 at 10:00:00 AM")
        else:
            print("Signed Time=Sep 14, 2026 at 10:00:00 AM")
    elif state.get("invalid_signature"):
        sys.exit(1)
elif command == "lipo":
    print(state.get("architectures", "arm64"))
elif command == "xcrun":
    if args[0] == "--find":
        print("/fixture/" + args[1])
    elif args[:2] == ["notarytool", "history"]:
        sys.exit(1 if state.get("invalid_profile") else 0)
    elif args[:2] == ["notarytool", "submit"]:
        phase = "dmg" if args[2].endswith(".dmg") else "payload"
        status = state.get(phase + "_status", "Accepted")
        receipt = {"id": "00000000-0000-0000-0000-00000000000" + ("2" if phase == "dmg" else "1"),
                   "status": status}
        print("invalid json" if state.get("invalid_receipt") else json.dumps(receipt))
        sys.exit(state.get(phase + "_exit", 0))
    elif args[:2] == ["notarytool", "log"]:
        if state.get("log_failure"):
            sys.exit(1)
        pathlib.Path(args[-1]).write_text(json.dumps({"id": args[2], "issues": []}))
    elif args[0] == "stapler":
        action, path = args[1], pathlib.Path(args[2])
        phase = "dmg" if path.suffix == ".dmg" else "payload"
        if state.get("fail_stapler") == phase + "-" + action:
            sys.exit(1)
        if action == "staple":
            if phase == "payload":
                (path / "Contents/CodeResources").write_text("STAPLED PAYLOAD FIXTURE")
            else:
                with path.open("ab") as stream:
                    stream.write(b"STAPLED DMG FIXTURE")
        elif phase == "payload":
            assert (path / "Contents/CodeResources").is_file()
        else:
            assert path.read_bytes().endswith(b"STAPLED DMG FIXTURE")
    else:
        raise AssertionError(args)
elif command == "spctl":
    print(state.get("assessment", "accepted\nsource=Notarized Developer ID"))
elif command == "ditto":
    source, target = map(pathlib.Path, args[-2:])
    if "-c" in args:
        with zipfile.ZipFile(target, "w") as archive:
            for item in source.rglob("*"):
                if item.is_file():
                    archive.write(item, source.name + "/" + str(item.relative_to(source)))
    else:
        shutil.copytree(source, target, dirs_exist_ok=True, symlinks=True)
elif command == "clang":
    target = pathlib.Path(args[args.index("-o") + 1])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("#!/bin/sh\nexit 0\n")
    target.chmod(0o755)
elif command == "cargo":
    target = pathlib.Path("target/release/jaso-nfc")
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("#!/bin/sh\necho 'jaso-nfc 0.2.1'\n")
    target.chmod(0o755)
elif command == "iconutil":
    pathlib.Path(args[args.index("-o") + 1]).write_text("ICON FIXTURE")
elif command == "hdiutil":
    assert args[0] == "create", "Fixture must never mount or install an application"
    (tools / "volume.txt").write_text(args[args.index("-srcfolder") + 1])
    pathlib.Path(args[-1]).write_bytes(b"DMG FIXTURE")
elif command == "shasum":
    path = pathlib.Path(args[-1])
    print(hashlib.sha256(path.read_bytes()).hexdigest() + "  " + path.name)
elif command in ("mv", "ln"):
    source, target = map(pathlib.Path, args[-2:])
    publishing = command == "mv" and target.parent.name == "dist"
    restoring = command == "ln" and target.parent.name == "dist"
    if publishing and target.suffix == ".dmg" and state.get("fail_final_move"):
        sys.exit(73)
    if command == "ln" and target.parent.name.startswith(".installer-publish."):
        if target.suffix == ".sha256" and state.get("fail_backup"):
            sys.exit(74)
    if restoring and target.suffix == ".sha256" and state.get("fail_restore"):
        sys.exit(75)
    # Exercise real filesystem renames/links, failing only the selected operation.
    status = subprocess.call(["/bin/" + command, *args])
    if status == 0 and publishing and target.suffix == ".sha256" and state.get("publish_signal"):
        os.kill(os.getppid(), getattr(signal, "SIG" + state["publish_signal"]))
    sys.exit(status)
else:
    raise AssertionError(command)
'''

PACKAGE_CHECK = r'''
import hashlib, json, os, pathlib
tools = pathlib.Path(__file__).resolve().parents[3] / "tools"
with (tools / "events.jsonl").open("a") as stream:
    stream.write(json.dumps(["package-check"]) + "\n")
if json.loads((tools / "state.json").read_text()).get("fail_package"):
    raise SystemExit(1)
image = pathlib.Path(os.environ["JASO_INSTALLER_DMG"])
source = pathlib.Path(os.environ["JASO_INSTALLER_PAYLOAD"])
packaged = pathlib.Path((tools / "volume.txt").read_text()) / "Install Jaso NFC.app/Contents/Resources/Jaso NFC.app"
def contents(path):
    return {str(item.relative_to(path)): item.read_bytes() for item in path.rglob("*") if item.is_file()}
assert contents(source) == contents(packaged), "Embedded payload changed after finalization"
checksum, basename = image.with_suffix(".dmg.sha256").read_text().split()
assert checksum == hashlib.sha256(image.read_bytes()).hexdigest()
assert basename == image.name
assert (packaged / "Contents/Resources/LICENSE.txt").is_file()
if os.environ["JASO_RELEASE_MODE"] == "1":
    assert (packaged / "Contents/CodeResources").is_file()
    assert image.read_bytes().endswith(b"STAPLED DMG FIXTURE")
    assert image.name == "Jaso-NFC-0.2.1-arm64.dmg"
else:
    assert image.name == "Jaso-NFC-0.2.1-arm64-local.dmg"
'''


@unittest.skipUnless(sys.platform == "darwin", "Requires the real macOS codesign parser")
class RealRequirementParser(unittest.TestCase):
    def test_helper_requirement_parses_and_rejects_adhoc_by_policy(self):
        spec = importlib.util.spec_from_file_location("release_signing", PROJECT / "scripts/release_signing.py")
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        with tempfile.TemporaryDirectory(prefix="jaso-requirement-test-") as directory:
            binary = Path(directory) / "adhoc-fixture"
            shutil.copyfile("/bin/echo", binary)
            binary.chmod(0o755)
            env = {"PATH": "/usr/bin:/bin", "LC_ALL": "C",
                   "JASO_SIGNING_IDENTITY": IDENTITY, "JASO_TEAM_ID": TEAM}
            for arguments in (("--force", "--sign", "-", "--timestamp=none"),
                              ("--verify", "--strict", "--all-architectures")):
                result = subprocess.run(["/usr/bin/codesign", *arguments, str(binary)],
                                        env=env, capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            # Call the production helper, without substituting its expression or
            # mocking codesign: a syntax/path error must not satisfy this test.
            with mock.patch.dict(os.environ, env, clear=True):
                with self.assertRaises(helper.ReleaseError) as failure:
                    helper.verify_code(binary, executable=True)
            self.assertIn("code failed to satisfy specified code requirement(s)", str(failure.exception))
            self.assertNotIn("invalid requirement specification", str(failure.exception))


class ReleaseSigning(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="jaso-release-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.project = self.root / "repo"
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.state = {}
        self.configure()
        tool = self.tools / "tool.py"
        tool.write_text(f"#!{sys.executable}\n" + TOOL)
        tool.chmod(0o755)
        for name in ("uname", "security", "codesign", "lipo", "xcrun", "spctl", "ditto",
                     "clang", "cargo", "iconutil", "hdiutil", "shasum", "mv", "ln"):
            (self.tools / name).symlink_to(tool)
        (self.tools / "python3").symlink_to(sys.executable)
        for name in ("scripts/build-native.sh", "scripts/build-installer.sh", "scripts/release_signing.py",
                     "native/macos/Info.plist", "native/tests/test_release_metadata.py", "LICENSE",
                     "Cargo.toml", "Cargo.lock", "pyproject.toml", "src/jaso_nfc/__init__.py"):
            destination = self.project / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(PROJECT / name, destination)
        for name in ("test_app_trampoline.py", "test_installed_runtime.py", "test_workspace_cli.py"):
            (self.project / "native/tests" / name).write_text("# Compilation/runtime fixture only.\n")
        (self.project / "native/tests/test_installer_package.py").write_text(PACKAGE_CHECK)
        self.app = self.root / "caller/Jaso NFC.app"
        (self.app / "Contents/MacOS").mkdir(parents=True)
        (self.app / "Contents/Resources").mkdir()
        shutil.copy2(PROJECT / "native/macos/Info.plist", self.app / "Contents/Info.plist")
        shutil.copy2(PROJECT / "LICENSE", self.app / "Contents/Resources/LICENSE.txt")
        (self.app / "Contents/Resources/JasoNFC.icns").write_text("ICON FIXTURE")
        for name in ("Jaso NFC", "jaso-nfc"):
            binary = self.app / "Contents/MacOS" / name
            binary.write_text("EXECUTABLE FIXTURE")
            binary.chmod(0o755)
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("JASO_")}
        self.env.update(PATH=f"{self.tools}:{os.environ.get('PATH', '')}",
                        JASO_SIGNING_IDENTITY=IDENTITY, JASO_TEAM_ID=TEAM,
                        JASO_NOTARY_PROFILE="fixture-profile")

    def configure(self, **values):
        self.state.update(values)
        (self.tools / "state.json").write_text(json.dumps(self.state))

    def events(self):
        path = self.tools / "events.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def execute(self, arguments, env=None):
        return subprocess.run(arguments, cwd=self.project, env=env or self.env,
                              capture_output=True, text=True, timeout=30)

    def helper(self, *arguments):
        return self.execute([sys.executable, "scripts/release_signing.py", *map(str, arguments)])

    def installer(self, *arguments):
        return self.execute(["sh", "scripts/build-installer.sh", *arguments, "--app", str(self.app)])

    def assert_failed_without_artifact(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(list((self.project / "dist").glob("*.dmg*")))

    def app_contents(self):
        return {str(path.relative_to(self.app)): (path.stat().st_mode, path.read_bytes())
                for path in self.app.rglob("*") if path.is_file()}

    def output_pair(self):
        image = self.project / "dist/Jaso-NFC-0.2.1-arm64.dmg"
        return image, image.with_suffix(".dmg.sha256")

    def previous_pair(self):
        image, checksum = self.output_pair()
        image.parent.mkdir(exist_ok=True)
        image.write_bytes(b"PREVIOUS DMG")
        checksum.write_text(hashlib.sha256(image.read_bytes()).hexdigest() + "  " + image.name + "\n")
        return {path.name: path.read_bytes() for path in (image, checksum)}

    def assert_published_pair(self, previous=None):
        image, checksum = self.output_pair()
        self.assertEqual(checksum.read_text().split(),
                         [hashlib.sha256(image.read_bytes()).hexdigest(), image.name])
        if previous is not None:
            self.assertEqual({path.name: path.read_bytes() for path in (image, checksum)}, previous)

    def assert_publication_cleaned(self):
        self.assertFalse(list((self.project / "build").glob("installer.*")))
        self.assertFalse(list((self.project / "dist").glob(".installer-publish.*")))

    def test_missing_release_inputs_fail_before_build_or_credentials(self):
        for script, missing in (("build-native.sh", "JASO_SIGNING_IDENTITY"),
                                ("build-native.sh", "JASO_TEAM_ID"),
                                ("build-installer.sh", "JASO_NOTARY_PROFILE")):
            with self.subTest(script=script, missing=missing):
                env = {key: value for key, value in self.env.items() if key != missing}
                result = self.execute(["sh", "scripts/" + script, "--release"], env)
                self.assert_failed_without_artifact(result)
                self.assertIn(missing, result.stderr)
        self.assertTrue(all(event[0] == "uname" for event in self.events()))

    def test_invalid_identity_or_team_input_is_rejected_before_build(self):
        for value in ("-", "Developer ID Installer: Fixture Company (ABCDE12345)",
                      "Developer ID Application: Fixture Company (WRONG12345)"):
            with self.subTest(identity=value):
                self.env["JASO_SIGNING_IDENTITY"] = value
                self.assert_failed_without_artifact(self.installer("--release"))
        self.assertTrue(all(event[0] == "uname" for event in self.events()))

    def test_unavailable_identity_and_bad_profile_fail_before_build(self):
        for failure in ({"identity_available": False}, {"identity_available": True, "invalid_profile": True}):
            with self.subTest(failure=failure):
                self.configure(**failure)
                result = self.execute(["sh", "scripts/build-installer.sh", "--release"])
                self.assert_failed_without_artifact(result)
        self.assertFalse(any(event[0] in ("cargo", "clang", "ditto") for event in self.events()))

    def test_signature_integrity_alone_does_not_accept_adhoc_or_wrong_metadata(self):
        for bad in ("adhoc", "signer", "team", "runtime", "timestamp"):
            with self.subTest(metadata=bad):
                self.configure(bad_metadata=bad)
                result = self.helper("verify", "payload", self.app)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        requirements = [event[event.index("--test-requirement") + 1] for event in self.events()
                        if "--test-requirement" in event]
        self.assertTrue(requirements)
        self.assertTrue(all("anchor apple generic" in value and "100.6.1.13" in value
                            and TEAM in value for value in requirements))

    def test_each_architecture_is_verified(self):
        self.configure(architectures="arm64 x86_64", bad_metadata="runtime", bad_architecture="x86_64")
        result = self.helper("verify", "payload", self.app)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        displays = [event for event in self.events() if "--display" in event]
        self.assertEqual([event[event.index("--architecture") + 1] for event in displays],
                         ["arm64", "x86_64"])

    def test_signature_verification_failure_is_fatal(self):
        self.configure(invalid_signature=True)
        self.assertNotEqual(self.helper("verify", "payload", self.app).returncode, 0)

    def test_unexpected_nested_code_is_rejected(self):
        extra = self.app / "Contents/Resources/unexpected"
        extra.write_bytes(bytes.fromhex("cffaedfe") + b"MACH-O FIXTURE")
        result = self.helper("verify", "payload", self.app)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("Unexpected executable", result.stderr)
        self.assertFalse(self.events())

    def test_caller_symlink_is_rejected_before_license_or_signature_writes(self):
        external = self.root / "caller/do-not-change.txt"
        external.write_text("CALLER CONTENT")
        license_path = self.app / "Contents/Resources/LICENSE.txt"
        license_path.unlink()
        license_path.symlink_to(external)
        self.assert_failed_without_artifact(self.installer("--release"))
        self.assertEqual(external.read_text(), "CALLER CONTENT")
        self.assertTrue(license_path.is_symlink())
        self.assertFalse(any("--sign" in event for event in self.events()))

    def test_release_payload_failures_never_submit(self):
        for metadata in ("signer", "team", "runtime", "timestamp"):
            with self.subTest(metadata=metadata):
                self.configure(bad_metadata=metadata)
                self.assert_failed_without_artifact(self.installer("--release"))
        self.assertFalse(any(event[:3] == ["xcrun", "notarytool", "submit"] for event in self.events()))

    def test_two_notarizations_order_immutable_input_and_final_checksum(self):
        original = self.app_contents()
        result = self.installer("--release")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.app_contents(), original)
        events = self.events()
        submits = [index for index, event in enumerate(events) if event[:3] == ["xcrun", "notarytool", "submit"]]
        self.assertEqual(len(submits), 2)
        self.assertTrue(events[submits[0]][3].endswith("payload.zip"))
        self.assertTrue(events[submits[1]][3].endswith(".dmg"))
        payload_staple = next(index for index, event in enumerate(events)
                              if event[:3] == ["xcrun", "stapler", "staple"])
        payload_validate = next(index for index, event in enumerate(events)
                                if event[:3] == ["xcrun", "stapler", "validate"])
        embedded = next(index for index, event in enumerate(events)
                        if event[0] == "ditto" and event[-1].endswith("Contents/Resources/Jaso NFC.app"))
        signs = [(index, event) for index, event in enumerate(events) if event[0] == "codesign" and "--sign" in event]
        self.assertEqual(len(signs), 4)
        gui, payload, installer, dmg = signs
        self.assertTrue(gui[1][-1].endswith("Contents/MacOS/Jaso NFC"))
        self.assertTrue(payload[1][-1].endswith("Jaso NFC.app"))
        self.assertTrue(installer[1][-1].endswith("Install Jaso NFC.app"))
        self.assertLess(gui[0], payload[0])
        self.assertLess(payload[0], submits[0])
        self.assertLess(submits[0], payload_staple)
        self.assertLess(payload_staple, payload_validate)
        self.assertLess(payload_validate, embedded)
        self.assertLess(embedded, installer[0])
        self.assertLess(installer[0], dmg[0])
        self.assertLess(dmg[0], submits[1])
        for _, event in signs:
            self.assertEqual(event[event.index("--sign") + 1], IDENTITY)
            self.assertIn("--timestamp", event)
            self.assertNotIn("--deep", event)
            self.assertNotIn("--entitlements", event)
        for _, event in signs[:3]:
            self.assertEqual(event[event.index("--options") + 1], "runtime")
        checksum_index = next(index for index, event in enumerate(events) if event[0] == "shasum")
        dmg_validate = max(index for index, event in enumerate(events) if event[:3] == ["xcrun", "stapler", "validate"])
        self.assertGreater(checksum_index, dmg_validate)
        self.assertGreater(events.index(["package-check"]), checksum_index)
        image = self.project / "dist/Jaso-NFC-0.2.1-arm64.dmg"
        self.assertEqual(image.with_suffix(".dmg.sha256").read_text().split(),
                         [hashlib.sha256(image.read_bytes()).hexdigest(), image.name])
        self.assertEqual(len(list((self.project / "build").glob("notary.*/*/submission.json"))), 2)
        self.assertEqual(len(list((self.project / "build").glob("notary.*/*/log.json"))), 2)

    def test_rejected_or_pending_notary_status_with_zero_exit_is_fatal(self):
        for phase, status in (("payload", "Invalid"), ("payload", "In Progress"), ("dmg", "Rejected")):
            with self.subTest(phase=phase, status=status):
                self.configure(payload_status="Accepted", dmg_status="Accepted")
                self.configure(**{phase + "_status": status})
                self.assert_failed_without_artifact(self.installer("--release"))
        receipts = list((self.project / "build").glob("notary.*/*/submission.json"))
        self.assertTrue(any(json.loads(path.read_text())["status"] == "Invalid" for path in receipts))
        self.assertTrue(list((self.project / "build").glob("notary.*/*/log.json")))

    def test_notary_command_failure_despite_accepted_json_is_fatal(self):
        self.configure(payload_exit=1)
        self.assert_failed_without_artifact(self.installer("--release"))
        self.assertFalse(any(event[:3] == ["xcrun", "stapler", "staple"] for event in self.events()))

    def test_malformed_receipt_and_log_failure_are_fatal(self):
        for changes in ({"invalid_receipt": True}, {"invalid_receipt": False, "log_failure": True}):
            with self.subTest(changes=changes):
                self.configure(**changes)
                self.assert_failed_without_artifact(self.installer("--release"))

    def test_staple_or_validation_failure_leaves_no_publishable_artifact(self):
        original = self.app_contents()
        for failure in ("payload-staple", "payload-validate", "dmg-staple", "dmg-validate"):
            with self.subTest(failure=failure):
                self.configure(fail_stapler=failure)
                self.assert_failed_without_artifact(self.installer("--release"))
                self.assertEqual(self.app_contents(), original)

    def test_gatekeeper_without_notarized_source_is_fatal(self):
        self.configure(assessment="accepted\nsource=Developer ID")
        self.assert_failed_without_artifact(self.installer("--release"))

    def test_package_verification_failure_leaves_no_publishable_artifact(self):
        self.configure(fail_package=True)
        self.assert_failed_without_artifact(self.installer("--release"))

    def test_failed_release_preserves_existing_final_files(self):
        output = self.project / "dist"
        output.mkdir()
        previous = {"Jaso-NFC-0.2.1-arm64.dmg": b"PREVIOUS DMG",
                    "Jaso-NFC-0.2.1-arm64.dmg.sha256": b"PREVIOUS CHECKSUM"}
        for name, content in previous.items():
            (output / name).write_bytes(content)
        self.configure(fail_package=True)
        result = self.installer("--release")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual({path.name: path.read_bytes() for path in output.iterdir()}, previous)

    def test_second_final_move_failure_restores_pair_or_leaves_neither_file(self):
        for has_previous in (False, True):
            with self.subTest(has_previous=has_previous):
                previous = self.previous_pair() if has_previous else None
                self.configure(fail_final_move=True)
                result = self.installer("--release")
                self.assertEqual(result.returncode, 73, result.stdout + result.stderr)
                if previous is not None:
                    self.assert_published_pair(previous)
                else:
                    self.assert_failed_without_artifact(result)
                self.assert_publication_cleaned()

    def test_handled_signals_after_first_final_move_roll_back(self):
        for signal_name in ("HUP", "INT", "TERM"):
            for has_previous in (False, True):
                with self.subTest(signal=signal_name, has_previous=has_previous):
                    for path in self.output_pair():
                        path.unlink(missing_ok=True)
                    previous = self.previous_pair() if has_previous else None
                    self.configure(publish_signal=signal_name)
                    result = self.installer("--release")
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    if previous is not None:
                        self.assert_published_pair(previous)
                    else:
                        self.assert_failed_without_artifact(result)
                    self.assert_publication_cleaned()

    def test_backup_failure_preserves_prior_pair_without_final_moves(self):
        previous = self.previous_pair()
        self.configure(fail_backup=True)
        result = self.installer("--release")
        self.assertEqual(result.returncode, 74, result.stdout + result.stderr)
        self.assert_published_pair(previous)
        self.assertFalse(any(event[0] == "mv" for event in self.events()))
        self.assert_publication_cleaned()

    def test_rollback_failure_keeps_complete_recovery_pair_outside_staging(self):
        previous = self.previous_pair()
        self.configure(fail_final_move=True, fail_restore=True)
        result = self.installer("--release")
        self.assert_failed_without_artifact(result)
        backups = list((self.project / "dist").glob(".installer-publish.*"))
        self.assertEqual(len(backups), 1)
        backup = backups[0]
        self.assertIn("Publication rollback failed; recovery files retained at: " + str(backup.resolve()), result.stderr)
        self.assertEqual({path.name: path.read_bytes() for path in backup.iterdir()}, previous)
        self.assertFalse(list((self.project / "build").glob("installer.*")))
        # Both backups survive even though the first restore succeeded. They
        # are sufficient to recover the exact prior pair after the I/O fault.
        for path in backup.iterdir():
            os.link(path, backup.parent / path.name)
        self.assert_published_pair(previous)

    def test_publication_replaces_complete_prior_pair(self):
        previous = self.previous_pair()
        result = self.installer("--release")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_published_pair()
        self.assertNotEqual(self.output_pair()[0].read_bytes(), previous[self.output_pair()[0].name])
        self.assert_publication_cleaned()

    def test_publication_rejects_symlink_and_directory_final_paths(self):
        external = self.root / "do-not-change"
        external.write_bytes(b"OWNED EXTERNAL FIXTURE")
        for index in (0, 1):
            for kind in ("symlink", "dangling", "directory"):
                with self.subTest(index=index, kind=kind):
                    previous = self.previous_pair()
                    path = self.output_pair()[index]
                    path.unlink()
                    if kind == "directory":
                        path.mkdir()
                    else:
                        path.symlink_to(external if kind == "symlink" else self.root / "absent")
                    result = self.installer("--release")
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("Publication requires a regular file or absent path", result.stderr)
                    other = self.output_pair()[1 - index]
                    self.assertEqual(other.read_bytes(), previous[other.name])
                    self.assertEqual(external.read_bytes(), b"OWNED EXTERNAL FIXTURE")
                    if kind == "directory":
                        self.assertEqual(list(path.iterdir()), [])
                        path.rmdir()
                    else:
                        self.assertTrue(path.is_symlink())
                        path.unlink()
                    self.assert_publication_cleaned()

    def test_publication_rejects_symlink_dist(self):
        external = self.root / "owned-external-dist"
        external.mkdir()
        (self.project / "dist").symlink_to(external, target_is_directory=True)
        result = self.installer("--release")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Publication dist must not be a symlink", result.stderr)
        self.assertEqual(list(external.iterdir()), [])
        self.assertTrue((self.project / "dist").is_symlink())
        self.assert_publication_cleaned()

    def test_publication_preserves_and_rejects_incomplete_prior_pair(self):
        for index in (0, 1):
            with self.subTest(missing=index):
                previous = self.previous_pair()
                self.output_pair()[index].unlink()
                result = self.installer("--release")
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("incomplete outputs were preserved", result.stderr)
                other = self.output_pair()[1 - index]
                self.assertEqual(other.read_bytes(), previous[other.name])
                self.assertFalse(self.output_pair()[index].exists())
                self.assert_publication_cleaned()

    def test_native_release_signs_without_a_notary_profile(self):
        del self.env["JASO_NOTARY_PROFILE"]
        result = self.execute(["sh", "scripts/build-native.sh", "--release"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        events = self.events()
        self.assertLess(next(index for index, event in enumerate(events) if event[0] == "security"),
                        next(index for index, event in enumerate(events) if event[0] == "cargo"))
        self.assertFalse(any(event[0] == "xcrun" for event in events))
        signs = [event for event in events if "--sign" in event]
        self.assertEqual(len(signs), 2)
        for event in signs:
            self.assertEqual(event[event.index("--sign") + 1], IDENTITY)
            self.assertEqual(event[event.index("--options") + 1], "runtime")
            self.assertIn("--timestamp", event)

    def test_local_mode_has_no_credential_dependency(self):
        self.env = {key: value for key, value in self.env.items() if not key.startswith("JASO_")}
        native = self.execute(["sh", "scripts/build-native.sh"])
        self.assertEqual(native.returncode, 0, native.stdout + native.stderr)
        result = self.installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.project / "dist/Jaso-NFC-0.2.1-arm64-local.dmg").is_file())
        self.assertFalse(any(event[0] in ("security", "xcrun", "spctl") for event in self.events()))
        signs = [event for event in self.events() if event[0] == "codesign" and "--sign" in event]
        self.assertEqual(len(signs), 3)
        self.assertTrue(all(event[event.index("--sign") + 1] == "-" for event in signs))


if __name__ == "__main__":
    unittest.main(verbosity=2)
