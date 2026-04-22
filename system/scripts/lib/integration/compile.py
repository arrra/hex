"""Compile bundle event policies via hex-events CLI."""
import os
import subprocess
import sys


HEX_EVENTS_CLI = os.path.expanduser("~/.hex-events/hex_events_cli.py")
GENERATED_MARKER = "# generated_from:"


class CompileError(Exception):
    pass


def _policy_stem(source_path: str) -> str:
    return os.path.splitext(os.path.basename(source_path))[0]


def _output_name(bundle_name: str, stem: str) -> str:
    return f"{bundle_name}-{stem}.yaml"


def compile_policies(
    bundle_dir: str,
    bundle_name: str,
    policies_dir: str,
    dry_run: bool = False,
) -> list[str]:
    """
    Compile all events/*.yaml in bundle_dir via `hex-events compile`.
    Returns list of output policy file paths written.
    Raises CompileError on compile failure.
    """
    cmd = [sys.executable, HEX_EVENTS_CLI, "compile", bundle_dir]
    if dry_run:
        cmd.append("--dry-run")

    result = subprocess.run(cmd, capture_output=True, text=True)

    # Always surface compile output so the user sees check failures
    if result.stderr:
        print(result.stderr, file=sys.stderr, end="")
    if result.stdout:
        print(result.stdout, file=sys.stderr, end="")

    if result.returncode != 0:
        raise CompileError(
            f"hex-events compile failed (exit {result.returncode})"
        )

    # Parse "  compiled: <abs-path>" lines from stdout
    written: list[str] = []
    for line in result.stdout.splitlines():
        if line.startswith("  compiled: "):
            written.append(line[len("  compiled: "):].strip())

    return written


def list_compiled_policies(bundle_name: str, policies_dir: str) -> list[str]:
    """List compiled policy files for a bundle (by generated_from marker)."""
    if not os.path.isdir(policies_dir):
        return []
    result = []
    prefix = f"{bundle_name}-"
    for fname in os.listdir(policies_dir):
        if not fname.startswith(prefix) or not fname.endswith(".yaml"):
            continue
        fpath = os.path.join(policies_dir, fname)
        try:
            with open(fpath) as f:
                first_line = f.readline()
            if GENERATED_MARKER in first_line:
                result.append(fpath)
        except OSError:
            pass
    return sorted(result)


def remove_compiled_policies(bundle_name: str, policies_dir: str) -> list[str]:
    """Remove all compiled policies for a bundle. Returns list of removed paths."""
    removed = []
    for fpath in list_compiled_policies(bundle_name, policies_dir):
        os.unlink(fpath)
        removed.append(fpath)
    return removed
