#!/usr/bin/env python3

"""
Partial build script for GitHub Actions CI.

Runs on Windows and Linux. Mac may be added later.

TODO Receives input via environment variables set via the environment matrix,
not command line arguments.
"""

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import typing as T
from contextlib import contextmanager
from pathlib import Path


@contextmanager
def pushd(new_dir: T.Union[Path, str]) -> T.Iterator[None]:
    previous_dir = os.getcwd()
    os.chdir(str(new_dir))
    try:
        yield
    finally:
        os.chdir(previous_dir)


"""TODO Debug or release build."""
CONFIGURATION = "release"

BUILD_DIR = f"target/{CONFIGURATION}"


ARCHIVE_ROOT = "archive-root"
EXE_NAME = "spectro2"


def archive():
    root_dir = Path().resolve()
    build_dir = Path(BUILD_DIR).resolve()

    shutil.rmtree(ARCHIVE_ROOT, ignore_errors=True)
    os.mkdir(ARCHIVE_ROOT)

    def copy_to_cwd(dir: Path, in_file: str):
        """Copies a file to the current directory without renaming it."""
        shutil.copy(dir / in_file, in_file)

    def copytree_to_cwd(dir: Path, in_file: str):
        """Copies a folder to the current directory without renaming it."""
        shutil.copytree(dir / in_file, in_file)

    with pushd(ARCHIVE_ROOT):
        if sys.platform == "win32":
            copy_to_cwd(build_dir, f"{EXE_NAME}.exe")
        elif sys.platform.startswith("linux"):
            copy_to_cwd(build_dir, EXE_NAME)
        else:
            raise Exception(f"unknown OS {sys.platform}, cannot determine binary name")

        copytree_to_cwd(root_dir, "shaders")
        copy_to_cwd(root_dir, "README.md")


class DefaultHelpParser(argparse.ArgumentParser):
    def error(self, message):
        sys.stderr.write("error: %s\n" % message)
        self.print_help(sys.stderr)
        sys.exit(2)


def main():
    # create the top-level parser
    parser = DefaultHelpParser()

    def f():
        subparsers = parser.add_subparsers(dest="cmd")
        subparsers.required = True

        subparsers.add_parser("archive")

    f()
    args = parser.parse_args()

    if args.cmd == "archive":
        archive()


if __name__ == "__main__":
    main()
