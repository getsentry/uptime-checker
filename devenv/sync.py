from devenv import constants
from devenv.lib import brew, fs, proc, uv

import shutil


def main(context: dict[str, str]) -> int:
    reporoot = context["reporoot"]

    # clean up leftover devenv uv install
    binroot = fs.ensure_binroot(reporoot)
    uv.uninstall(binroot)

    brew.install()

    proc.run(
        (f"{constants.homebrew_bin}/brew", "bundle"),
        cwd=reporoot,
    )

    if not shutil.which("rustup"):
        raise SystemExit("rustup not on PATH. Did you run `direnv allow`?")

    proc.run(
        (
            "rustup",
            "toolchain",
            "install",
            "stable",
            "--profile",
            "minimal",
            "--component",
            "rustfmt",
            "--component",
            "clippy",
            "--no-self-update",
        )
    )

    print("syncing .venv ...")

    if not shutil.which("uv"):
        raise SystemExit("uv not on PATH. Did you run `direnv allow`?")

    proc.run(
        ("uv", "sync", "--frozen", "--quiet", "--active"),
    )

    print("installing pre-commit hooks ...")
    proc.run((f"{reporoot}/.venv/bin/pre-commit", "install", "--install-hooks"))

    return 0
