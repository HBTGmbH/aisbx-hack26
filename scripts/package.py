"""Package the release binaries of aisbx for one target.

    python3 scripts/package.py version [TAG]
    python3 scripts/package.py build --target TRIPLE [--out DIR]
"""

import argparse
import gzip
import hashlib
import io
import subprocess
import sys
import tarfile
import time
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARIES = ("aisbx", "aisbx-ui")
SHIPPED = ("kits", "docs", "README.md", "LICENSE")


def version(root=ROOT):
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    return manifest["workspace"]["package"]["version"].replace("+", ".")


def check_tag(tag, root=ROOT):
    expected = "v" + version(root)
    if tag != expected:
        raise SystemExit(f"tag {tag} does not match Cargo.toml: expected {expected}")
    return expected


def is_windows(target):
    return "windows" in target


def tracked_files(root, paths):
    out = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-s", "-z", "--", *paths],
        check=True,
        capture_output=True,
    ).stdout.decode("utf-8")
    files = []
    for entry in filter(None, out.split("\0")):
        meta, path = entry.split("\t", 1)
        mode = meta.split(" ", 1)[0]
        if mode not in ("100644", "100755"):
            raise SystemExit(f"{path}: unsupported git mode {mode}")
        files.append((path, 0o755 if mode == "100755" else 0o644))
    return files


def commit_time(root):
    out = subprocess.run(
        ["git", "-C", str(root), "log", "-1", "--format=%ct"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return int(out.strip())


def entries(root, target):
    suffix = ".exe" if is_windows(target) else ""
    release = root / "target" / target / "release"
    items = []
    for name in BINARIES:
        binary = release / (name + suffix)
        if not binary.is_file():
            raise SystemExit(f"missing {binary}: run cargo build --release --target {target}")
        items.append((f"bin/{name}{suffix}", binary, 0o755))
    for path, mode in tracked_files(root, SHIPPED):
        items.append((path, root / path, mode))
    return items


def write_tar(archive, top, items, mtime):
    with open(archive, "wb") as raw, gzip.GzipFile(
        filename="", mode="wb", fileobj=raw, mtime=0
    ) as gz, tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for rel, src, mode in items:
            data = src.read_bytes()
            info = tarfile.TarInfo(f"{top}/{rel}")
            info.size = len(data)
            info.mode = mode
            info.mtime = mtime
            tar.addfile(info, io.BytesIO(data))


def write_zip(archive, top, items, mtime):
    stamp = time.gmtime(max(mtime, 315532800))[:6]
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
        for rel, src, mode in items:
            info = zipfile.ZipInfo(f"{top}/{rel}", date_time=stamp)
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(info, src.read_bytes())


def build(target, out, root=ROOT):
    top = f"aisbx-{version(root)}-{target}"
    items = entries(root, target)
    out.mkdir(parents=True, exist_ok=True)
    archive = out / (top + (".zip" if is_windows(target) else ".tar.gz"))
    write = write_zip if is_windows(target) else write_tar
    write(archive, top, items, commit_time(root))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    checksum = archive.with_name(archive.name + ".sha256")
    checksum.write_text(f"{digest}  {archive.name}\n", encoding="utf-8", newline="\n")
    return archive, checksum


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    v = sub.add_parser("version", help="print the version, or check TAG against it")
    v.add_argument("tag", nargs="?")
    b = sub.add_parser("build", help="write the archive and its .sha256 file")
    b.add_argument("--target", required=True)
    b.add_argument("--out", type=Path, default=ROOT / "dist")
    args = parser.parse_args(argv)
    if args.command == "version":
        print(check_tag(args.tag) if args.tag else version())
    else:
        for path in build(args.target, args.out):
            print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
