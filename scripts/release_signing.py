#!/usr/bin/env python3
"""Internal Developer ID checks for the explicit --release build path.

Uses only the selected signing identity and a pre-stored notarytool profile.
There is deliberately no ad hoc fallback or credential provisioning here.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import uuid


class ReleaseError(RuntimeError):
    pass


def run(arguments):
    result = subprocess.run([str(arg) for arg in arguments], capture_output=True,
                            text=True, env={**os.environ, "LC_ALL": "C"})
    if result.returncode:
        raise ReleaseError(f"{arguments[0]} failed: {result.stderr.strip()}")
    return result.stdout + result.stderr


def identity():
    name = os.environ.get("JASO_SIGNING_IDENTITY", "")
    team = os.environ.get("JASO_TEAM_ID", "")
    if not re.fullmatch(r"[A-Z0-9]{10}", team):
        raise ReleaseError("JASO_TEAM_ID must be the exact 10-character Apple team ID.")
    if not re.fullmatch(r"Developer ID Application: [^\r\n]+ \(" + team + r"\)", name):
        raise ReleaseError("JASO_SIGNING_IDENTITY must be the full Developer ID Application "
                           "certificate name ending in (JASO_TEAM_ID).")
    return name, team


def notary_profile():
    profile = os.environ.get("JASO_NOTARY_PROFILE", "")
    if not profile.strip() or any(char in profile for char in "\r\n"):
        raise ReleaseError("JASO_NOTARY_PROFILE must name a pre-stored notarytool Keychain profile.")
    return profile


def preflight(notary=False):
    name, _ = identity()
    profile = notary_profile() if notary else None
    # Do not print the keychain's identity inventory or notarization history.
    available = run(["security", "find-identity", "-v", "-p", "codesigning"])
    names = re.findall(r'^\s*\d+\)\s+[0-9A-Fa-f]{40}\s+"([^"\n]+)"\s*$',
                       available, re.MULTILINE)
    if names.count(name) != 1:
        raise ReleaseError("The exact signing identity must resolve to one valid keychain identity.")
    if notary:
        run(["xcrun", "--find", "stapler"])
        # Authenticate before compiling; a missing/expired profile must fail early.
        run(["xcrun", "notarytool", "history", "--keychain-profile", profile,
             "--output-format", "json"])


def binaries(kind, path):
    if kind == "payload":
        return [path / "Contents/MacOS/Jaso NFC", path / "Contents/MacOS/jaso-nfc"]
    if kind == "installer":
        return [path / "Contents/MacOS/Jaso NFC Installer", *binaries(
            "payload", path / "Contents/Resources/Jaso NFC.app")]
    return []


def check_layout(kind, path):
    """This distribution has two payload executables and one installer executable."""
    if path.is_symlink() or not path.is_dir():
        raise ReleaseError(f"Expected a real release bundle directory: {path}")
    expected = set(binaries(kind, path))
    for binary in expected:
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ReleaseError(f"Missing executable: {binary}")
    macho = {bytes.fromhex(value) for value in (
        "feedface", "cefaedfe", "feedfacf", "cffaedfe",
        "cafebabe", "bebafeca", "cafebabf", "bfbafeca")}
    for item in path.rglob("*"):
        if item.is_symlink() or not (item.is_file() or item.is_dir()):
            raise ReleaseError(f"Release bundles require regular files/directories: {item}")
        if item.is_file() and item not in expected:
            with item.open("rb") as stream:
                is_code = stream.read(4) in macho
            if is_code or item.stat().st_mode & 0o111:
                raise ReleaseError(f"Unexpected executable in release bundle: {item}")


def verify_code(path, executable=False):
    name, team = identity()
    # Integrity alone accepts ad hoc signatures. Require Apple's Developer ID
    # Application chain as well as the expected team, for every architecture.
    requirement = ('anchor apple generic and '
                   'certificate 1[field.1.2.840.113635.100.6.2.6] exists and '
                   'certificate leaf[field.1.2.840.113635.100.6.1.13] exists and '
                   f'certificate leaf[subject.OU] = "{team}"')
    run(["codesign", "--verify", "--strict", "--all-architectures",
         "--test-requirement", requirement, path])
    architectures = run(["lipo", "-archs", path]).split() if executable else [None]
    if not architectures:
        raise ReleaseError(f"No executable architectures: {path}")
    for architecture in architectures:
        options = ["--architecture", architecture] if architecture else []
        details = run(["codesign", "--display", "--verbose=4", *options, path])
        authorities = re.findall(r"^Authority=(.+)$", details, re.MULTILINE)
        teams = re.findall(r"^TeamIdentifier=(.+)$", details, re.MULTILINE)
        timestamps = re.findall(r"^Timestamp=(.+)$", details, re.MULTILINE)
        if not authorities or authorities[0] != name or teams != [team]:
            raise ReleaseError(f"Unexpected Developer ID signer or team: {path}")
        if len(timestamps) != 1 or timestamps[0].strip().lower() in ("", "none", "not set"):
            raise ReleaseError(f"Missing secure timestamp: {path}")
        flags = re.search(r"\bflags=0x([0-9a-fA-F]+)\b", details)
        if executable and (not flags or not int(flags[1], 16) & 0x10000):
            raise ReleaseError(f"Missing hardened runtime: {path} ({architecture})")


def verify(kind, path):
    if kind != "dmg":
        check_layout(kind, path)
        run(["codesign", "--verify", "--deep", "--strict", "--all-architectures", path])
        for binary in binaries(kind, path):
            verify_code(binary, executable=True)
        if kind == "installer":
            verify_code(path / "Contents/Resources/Jaso NFC.app")
    verify_code(path)


def sign(kind, path):
    name, _ = identity()
    if kind != "dmg":
        check_layout(kind, path)
    if kind == "payload":
        # Check links before writing: a caller's copied --app may contain a link
        # outside staging. Keep the original MIT notice in every installed copy.
        shutil.copyfile(Path(__file__).resolve().parents[1] / "LICENSE",
                        path / "Contents/Resources/LICENSE.txt")
        run(["codesign", "--force", "--sign", name, "--options", "runtime", "--timestamp",
             "--identifier", "io.github.garlicvread.jaso-nfc.menu", binaries(kind, path)[0]])
    options = [] if kind == "dmg" else ["--options", "runtime", "--identifier",
        "io.github.garlicvread.jaso-nfc" + (".installer" if kind == "installer" else "")]
    # Signing the bundle signs its main executable; never re-sign nested payload
    # code after its ticket has been stapled. --deep is only used for verification.
    run(["codesign", "--force", "--sign", name, *options, "--timestamp", path])
    verify(kind, path)


def assess(kind, path):
    options = (["--type", "open", "--context", "context:primary-signature"]
               if kind == "dmg" else ["--type", "execute"])
    result = run(["spctl", "--assess", *options, "--verbose=2", path])
    if "source=Notarized Developer ID" not in result.splitlines():
        raise ReleaseError(f"Gatekeeper did not report Notarized Developer ID: {path}")


def notarize(kind, path, receipts):
    profile = notary_profile()
    receipts.mkdir(parents=True, exist_ok=True, mode=0o700)
    print(f"Notarization receipts: {receipts}", file=sys.stderr)
    with tempfile.TemporaryDirectory(prefix="notary-upload-", dir=path.parent) as directory:
        archive = path
        if kind == "payload":
            archive = Path(directory) / "payload.zip"
            run(["ditto", "-c", "-k", "--keepParent", path, archive])
        submitted = subprocess.run(
            ["xcrun", "notarytool", "submit", str(archive), "--keychain-profile", profile,
             "--wait", "--output-format", "json"], capture_output=True, text=True)
        (receipts / "submission.json").write_text(submitted.stdout)
        (receipts / "submission.stderr.txt").write_text(submitted.stderr)
    try:
        receipt = json.loads(submitted.stdout)
        if not isinstance(receipt, dict) or not isinstance(receipt.get("id"), str):
            raise ValueError("Missing submission ID")
        submission_id = str(uuid.UUID(receipt["id"]))
    except (ValueError, KeyError, TypeError) as error:
        raise ReleaseError(f"Invalid notarytool receipt; see {receipts}") from error
    logged = subprocess.run(
        ["xcrun", "notarytool", "log", submission_id, "--keychain-profile", profile,
         str(receipts / "log.json")], capture_output=True, text=True)
    (receipts / "log-output.txt").write_text(logged.stdout + logged.stderr)
    if submitted.returncode or receipt.get("status") != "Accepted":
        raise ReleaseError(f"Notarization was not Accepted; see {receipts}")
    if logged.returncode or not (receipts / "log.json").is_file():
        raise ReleaseError(f"Could not save the notarization log; see {receipts}")
    for action in ("staple", "validate"):
        result = subprocess.run(["xcrun", "stapler", action, str(path)],
                                capture_output=True, text=True)
        (receipts / f"{action}.txt").write_text(result.stdout + result.stderr)
        if result.returncode:
            raise ReleaseError(f"Stapler {action} failed; see {receipts}")
    verify(kind, path)
    assess(kind, path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("preflight").add_argument("--notary", action="store_true")
    for command in ("sign", "verify", "assess", "notarize"):
        child = commands.add_parser(command)
        child.add_argument("kind", choices=("payload", "dmg") if command == "notarize"
                           else ("payload", "installer", "dmg"))
        child.add_argument("path", type=Path)
        if command == "notarize":
            child.add_argument("receipts", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "preflight":
            preflight(args.notary)
        elif args.command == "notarize":
            notarize(args.kind, args.path, args.receipts)
        else:
            {"sign": sign, "verify": verify, "assess": assess}[args.command](args.kind, args.path)
    except (ReleaseError, OSError) as error:
        parser.exit(1, f"Release failed: {error}\n")


if __name__ == "__main__":
    main()
