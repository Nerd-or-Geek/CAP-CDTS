#!/usr/bin/env python3
"""Rebuild CAP-CDTS in a fresh folder.

This script is meant for Raspberry Pi / Linux use, and is used by the app's
updater as a source-build fallback when a GitHub Release binary is not
available.

It clones the repo into a new timestamped directory, builds with a cached
CARGO_TARGET_DIR (for faster subsequent builds), then copies the resulting
binary into that build folder and prints the artifact path on stdout.

Stdout: prints ONLY the artifact path (single line) so callers can parse it.
Logs go to stderr.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def eprint(*args: object) -> None:
    print(*args, file=sys.stderr)


def run(cmd: list[str], *, cwd: Path | None = None, env: dict[str, str] | None = None) -> None:
    eprint("+", " ".join(cmd))
    subprocess.run(cmd, cwd=str(cwd) if cwd else None, env=env, check=True)


def acquire_lock(lock_path: Path) -> None:
    # Cross-process lock using an exclusive create.
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        fd = os.open(str(lock_path), os.O_CREAT | os.O_EXCL | os.O_WRONLY)
        try:
            os.write(fd, f"{os.getpid()}\n".encode("utf-8"))
        finally:
            os.close(fd)
    except FileExistsError as exc:
        raise RuntimeError(f"Update already running (lock exists): {lock_path}") from exc


def release_lock(lock_path: Path) -> None:
    try:
        lock_path.unlink(missing_ok=True)  # py3.8+: missing_ok
    except TypeError:
        # Python 3.7 fallback
        try:
            lock_path.unlink()
        except FileNotFoundError:
            pass


def cleanup_old_builds(builds_dir: Path, keep: int) -> None:
    if keep <= 0:
        return

    if not builds_dir.exists():
        return

    builds = sorted([p for p in builds_dir.iterdir() if p.is_dir()])
    if len(builds) <= keep:
        return

    for old in builds[: max(0, len(builds) - keep)]:
        eprint(f"Cleaning old build dir: {old}")
        shutil.rmtree(old, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo-url", required=True, help="Git URL to clone")
    ap.add_argument(
        "--updater-root",
        default=None,
        help="Updater cache root (default: $CAP_CDTS_UPDATER_DIR or ~/.cache/cap-cdts-updater)",
    )
    ap.add_argument(
        "--bin-name",
        default="rfid-cyberdeck-rust",
        help="Binary name (default: rfid-cyberdeck-rust)",
    )
    ap.add_argument("--keep", type=int, default=3, help="How many build folders to keep")
    args = ap.parse_args()

    default_root = os.environ.get("CAP_CDTS_UPDATER_DIR")
    if not default_root:
        if os.name == "posix":
            default_root = str(Path.home() / ".cache" / "cap-cdts-updater")
        else:
            default_root = str(Path(tempfile.gettempdir()) / "cap-cdts-updater")

    updater_root = args.updater_root or default_root

    root = Path(updater_root).expanduser().resolve()
    builds_dir = root / "builds"
    target_dir = root / "cargo-target"
    lock_path = root / "update.lock"

    acquire_lock(lock_path)
    try:
        builds_dir.mkdir(parents=True, exist_ok=True)
        target_dir.mkdir(parents=True, exist_ok=True)

        build_id = time.strftime("%Y%m%d-%H%M%S") + f"-{os.getpid()}"
        build_dir = builds_dir / build_id
        repo_dir = build_dir / "repo"
        artifact_dir = build_dir / "artifact"
        artifact_dir.mkdir(parents=True, exist_ok=True)

        eprint(f"Build dir: {build_dir}")

        # Fresh shallow clone.
        run(
            [
                "git",
                "clone",
                "--depth",
                "1",
                "--single-branch",
                args.repo_url,
                str(repo_dir),
            ]
        )

        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(target_dir)

        build_cmd = ["cargo", "build", "--release"]
        if (repo_dir / "Cargo.lock").exists():
            build_cmd.append("--locked")

        run(build_cmd, cwd=repo_dir, env=env)

        # Locate the built binary (from the cached target dir).
        bin_name = args.bin_name
        if os.name == "nt" and not bin_name.lower().endswith(".exe"):
            bin_name += ".exe"

        built_bin = target_dir / "release" / bin_name
        if not built_bin.exists():
            raise RuntimeError(f"Built binary not found: {built_bin}")

        artifact_path = artifact_dir / built_bin.name
        shutil.copy2(built_bin, artifact_path)

        if os.name == "posix":
            try:
                artifact_path.chmod(0o755)
            except Exception:
                # Not fatal
                pass

        cleanup_old_builds(builds_dir, args.keep)

        # Print ONLY the artifact path for machine parsing.
        print(str(artifact_path))
        return 0
    except subprocess.CalledProcessError as exc:
        eprint(f"Command failed with exit code {exc.returncode}: {exc.cmd}")
        return exc.returncode or 1
    except Exception as exc:
        eprint(f"Rebuild failed: {exc}")
        return 1
    finally:
        release_lock(lock_path)


if __name__ == "__main__":
    raise SystemExit(main())
