"""Validate a prepared release without creating or moving repository refs."""
import os
import re
import subprocess
from pathlib import Path


def resolve(version, changelog, event, ref, subject, requested="", dry_run=True):
    publish = False
    candidate = version
    if event == "push" and ref.startswith("refs/tags/"):
        candidate = ref.removeprefix("refs/tags/v")
        publish = True
    elif event == "push" and ref == "refs/heads/master" and subject.startswith("chore: release v"):
        match = re.fullmatch(r"chore: release v(\d+\.\d+\.\d+)", subject)
        if not match:
            raise ValueError("Release subject must be exactly 'chore: release vX.Y.Z'")
        candidate = match[1]
        publish = True
    elif event == "workflow_dispatch":
        if ref != "refs/heads/master":
            raise ValueError("Manual releases must use master")
        candidate = requested or version
        publish = not dry_run
    if not re.fullmatch(r"\d+\.\d+\.\d+", candidate) or candidate != version:
        raise ValueError("Requested/tag version must equal CMake BASE_VERSION")
    if publish and f"## [{version}]" not in changelog:
        raise ValueError("Release requires a versioned changelog entry")
    return candidate, publish


if __name__ == "__main__":
    version = re.search(r'^set\(BASE_VERSION "([^"]+)"\)', Path("CMakeLists.txt").read_text(), re.M)[1]
    changelog = Path("CHANGELOG.md").read_text() if Path("CHANGELOG.md").exists() else ""
    subject = subprocess.check_output(["git", "log", "-1", "--pretty=%s"], text=True).strip()
    version, publish = resolve(version, changelog, os.environ["GITHUB_EVENT_NAME"],
                               os.environ["GITHUB_REF"], subject, os.getenv("INPUT_VERSION", ""),
                               os.getenv("INPUT_DRY_RUN", "true") == "true")
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"version={version}\npublish={str(publish).lower()}\n")
