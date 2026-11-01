import hashlib
import os
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import package


def git(root, *args):
    env = dict(os.environ, GIT_AUTHOR_DATE=FIXTURE_DATE, GIT_COMMITTER_DATE=FIXTURE_DATE)
    subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True, env=env)


FIXTURE_DATE = "2026-03-04T05:06:08Z"
COMMIT_TIME = 1772600768


class Checkout:
    def __init__(self, target):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "repo"
        files = {
            "Cargo.toml": '[workspace.package]\nversion = "0.50.1+3"\n',
            "README.md": "readme\n",
            "LICENSE": "license\n",
            "docs/page.md": "page\n",
            "kits/demo/spec.yaml": "kind: mixin\n",
            "kits/demo/files/hook.sh": "#!/bin/sh\n",
            "core/src/lib.rs": "\n",
        }
        for rel, text in files.items():
            path = self.root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        git(self.root, "init", "-q")
        git(self.root, "add", ".")
        git(self.root, "update-index", "--chmod=+x", "kits/demo/files/hook.sh")
        git(
            self.root,
            "-c",
            "user.name=Lorem",
            "-c",
            "user.email=lorem@example.invalid",
            "commit",
            "-q",
            "-m",
            "fixture",
        )
        (self.root / "kits/demo/untracked.txt").write_text("x", encoding="utf-8")
        release = self.root / "target" / target / "release"
        release.mkdir(parents=True)
        suffix = ".exe" if package.is_windows(target) else ""
        for name in package.BINARIES:
            (release / (name + suffix)).write_bytes(b"binary " + name.encode())

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.tmp.cleanup()


class VersionTest(unittest.TestCase):
    def test_the_version_turns_the_build_metadata_into_a_fourth_number(self):
        with Checkout("x86_64-unknown-linux-gnu") as c:
            self.assertEqual(package.version(c.root), "0.50.1.3")

    def test_a_tag_must_match_the_version(self):
        with Checkout("x86_64-unknown-linux-gnu") as c:
            self.assertEqual(package.check_tag("v0.50.1.3", c.root), "v0.50.1.3")
            for tag in ("v0.50.1.2", "0.50.1.3", "v0.50.1+3"):
                with self.assertRaises(SystemExit):
                    package.check_tag(tag, c.root)

    def test_the_repo_version_is_readable(self):
        self.assertRegex(package.version(), r"^\d+\.\d+\.\d+\.\d+$")


class BuildTest(unittest.TestCase):
    expected = {
        "LICENSE",
        "README.md",
        "docs/page.md",
        "kits/demo/spec.yaml",
        "kits/demo/files/hook.sh",
    }

    def test_a_unix_archive_holds_the_binaries_beside_the_kits(self):
        target = "aarch64-apple-darwin"
        with Checkout(target) as c:
            archive, checksum = package.build(target, c.root / "dist", c.root)
            top = "aisbx-0.50.1.3-aarch64-apple-darwin"
            self.assertEqual(archive.name, top + ".tar.gz")
            with tarfile.open(archive) as tar:
                modes = {m.name.removeprefix(top + "/"): m.mode for m in tar.getmembers()}
                self.assertEqual({m.mtime for m in tar.getmembers()}, {COMMIT_TIME})
                self.assertTrue(all(m.name.startswith(top + "/") for m in tar.getmembers()))
            self.assertEqual(set(modes), self.expected | {"bin/aisbx", "bin/aisbx-ui"})
            self.assertEqual(modes["bin/aisbx"], 0o755)
            self.assertEqual(modes["kits/demo/files/hook.sh"], 0o755)
            self.assertEqual(modes["kits/demo/spec.yaml"], 0o644)
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            self.assertEqual(checksum.read_text(encoding="utf-8"), f"{digest}  {archive.name}\n")

    def test_a_windows_archive_is_a_zip_with_exe_files(self):
        target = "x86_64-pc-windows-msvc"
        with Checkout(target) as c:
            archive, _ = package.build(target, c.root / "dist", c.root)
            top = "aisbx-0.50.1.3-x86_64-pc-windows-msvc"
            self.assertEqual(archive.name, top + ".zip")
            with zipfile.ZipFile(archive) as zf:
                names = {n.removeprefix(top + "/") for n in zf.namelist()}
                stamps = {i.date_time for i in zf.infolist()}
                self.assertEqual(stamps, {(2026, 3, 4, 5, 6, 8)})
                self.assertEqual(zf.read(top + "/bin/aisbx.exe"), b"binary aisbx")
            self.assertEqual(names, self.expected | {"bin/aisbx.exe", "bin/aisbx-ui.exe"})

    def test_the_archive_is_the_same_on_each_run(self):
        target = "x86_64-unknown-linux-gnu"
        with Checkout(target) as c:
            first, _ = package.build(target, c.root / "a", c.root)
            time.sleep(1.1)
            second, _ = package.build(target, c.root / "b", c.root)
            self.assertEqual(first.read_bytes(), second.read_bytes())

    def test_a_missing_binary_stops_the_build(self):
        target = "x86_64-unknown-linux-gnu"
        with Checkout(target) as c:
            (c.root / "target" / target / "release" / "aisbx-ui").unlink()
            with self.assertRaises(SystemExit):
                package.build(target, c.root / "dist", c.root)


if __name__ == "__main__":
    unittest.main()
