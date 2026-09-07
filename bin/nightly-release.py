#!/usr/bin/env python3
"""Coordinate immutable PTB releases. Requires Python 3.11+, git, and gh.

The workflow serializes plan/prepare/dispatch. Tags reserve numbers forever;
their annotations identify the source main commit. A release-body receipt is
written only after publication, so a partial release cannot suppress recovery.
"""

import argparse
import copy
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib


REPOSITORY = "smudgy-mud/smudgy"
BASE_RE = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-ptb")
TAG_RE = re.compile(r"v([0-9]+\.[0-9]+\.[0-9]+-ptb)(?:[.-]?([1-9][0-9]*))?")
ANNOTATION = "Smudgy-Nightly: "


def command(*args, input_text=None):
    return subprocess.run(args, input=input_text, text=True, encoding="utf-8",
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout.strip()


def git(*args):
    return command("git", *args)


def api(endpoint, *, payload=None, missing_ok=False):
    args = ["gh", "api", f"repos/{REPOSITORY}/{endpoint}"]
    if payload is not None:
        args += ["--method", "PATCH", "--input", "-"]
    result = subprocess.run(args, input=json.dumps(payload) if payload is not None else None,
                            text=True, encoding="utf-8", capture_output=True)
    if result.returncode:
        # Only an actual 404 means absent. Auth, rate-limit, and server failures
        # must stop the coordinator rather than allocate duplicate releases.
        try:
            status = str(json.loads(result.stdout).get("status"))
        except (ValueError, AttributeError):
            status = ""
        if missing_ok and status == "404":
            return None
        raise RuntimeError(f"GitHub API failed for {endpoint}: {result.stderr.strip()}")
    return json.loads(result.stdout)


def pages(endpoint, key):
    page = 1
    while True:
        separator = "&" if "?" in endpoint else "?"
        result = api(f"{endpoint}{separator}per_page=100&page={page}")
        items = result[key] if key else result
        yield from items
        if len(items) < 100:
            return
        page += 1


def next_version(base, tags):
    if not BASE_RE.fullmatch(base):
        raise ValueError("main must use an unnumbered X.Y.Z-ptb version")
    numbers = [int(match[2] or 0) for tag in tags
               if (match := TAG_RE.fullmatch(tag)) and match[1] == base]
    return f"{base}.{max(numbers, default=0) + 1}"


def receipt(source, tag):
    return f"<!-- smudgy-nightly-complete source={source} tag={tag} -->"


def tag_metadata(tag):
    text = git("for-each-ref", "--format=%(contents)", f"refs/tags/{tag}")
    lines = [line.removeprefix(ANNOTATION) for line in text.splitlines()
             if line.startswith(ANNOTATION)]
    if not lines:
        return None # Legacy/manual tags still reserve numbers.
    if len(lines) != 1:
        raise ValueError(f"Ambiguous nightly annotation on {tag}")
    data = json.loads(lines[0])
    if (data.get("schema") != 1 or data.get("version") != tag.removeprefix("v")
            or not re.fullmatch(r"[0-9a-f]{40}", data.get("source_sha", ""))):
        raise ValueError(f"Invalid nightly annotation on {tag}")
    # A nightly is exactly one version-only commit on top of its source.
    if git("rev-list", "--parents", "-n", "1", f"{tag}^{{commit}}").split()[1:] != [data["source_sha"]]:
        raise ValueError(f"Nightly tag {tag} does not have its declared source as sole parent")
    return data


def existing_action(source, tag, metadata, release, runs, now):
    if (release and not release["draft"]
            and receipt(source, tag) in (release.get("body") or "")):
        return "skip", f"{tag} already published source {source}."
    if any(run["status"] != "completed" for run in runs):
        return "skip", f"{tag} is already queued or running (including retries)."
    if runs:
        run = max(runs, key=lambda item: item["id"])
        # Recovery belongs to the Release retry workflow. Do not reset its
        # run_attempt by dispatching a fresh run every night.
        return "skip", (f"{tag} has run {run['id']} ({run['conclusion']}, attempt "
                        f"{run['run_attempt']}). Automatic retries keep this tag; "
                        "after attempt 3, inspect and re-run it manually.")
    created = datetime.fromisoformat(metadata["created_at"])
    if now - created > timedelta(hours=24):
        # A run can have been deleted or expired. Absence is not permission to
        # restart an exhausted release forever. Retain the reservation.
        return "skip", f"{tag} has no retained run; inspect and dispatch Release on this tag manually."
    return "dispatch", f"Recovering {tag}: reserved recently, but no Release run exists."


def make_plan():
    source = git("rev-parse", "HEAD")
    if source != git("rev-parse", "refs/remotes/origin/main"):
        raise ValueError("Coordinator checkout must be origin/main")
    base = tomllib.loads(Path("ui/Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    tags = git("tag", "--list", "v*-ptb*").splitlines()
    version = next_version(base, tags)
    matches = []
    for tag in tags:
        match = TAG_RE.fullmatch(tag)
        if match and match[1] == base:
            metadata = tag_metadata(tag)
            if metadata and metadata["source_sha"] == source:
                matches.append((tag, metadata))
    if len(matches) > 1:
        raise ValueError(f"Multiple nightly reservations for source {source}; inspect manually")
    if matches:
        tag, metadata = matches[0]
        version = metadata["version"]
        release = api(f"releases/tags/{tag}", missing_ok=True)
        release_sha = git("rev-parse", f"{tag}^{{commit}}")
        done = release and not release["draft"] and receipt(source, tag) in (release.get("body") or "")
        runs = [] if done else list(pages(f"actions/workflows/release.yml/runs?head_sha={release_sha}", "workflow_runs"))
        action, reason = existing_action(source, tag, metadata, release, runs, datetime.now(timezone.utc))
    else:
        tag = f"v{version}"
        action, reason = "create", f"New main source {source}; reserve {tag}."
    return {"source_sha": source, "version": version, "tag": tag, "action": action, "reason": reason}


def package_manifests():
    manifests = {}
    for path in git("ls-files", "*/Cargo.toml").splitlines():
        if path.count("/") != 1:
            continue
        data = tomllib.loads(Path(path).read_text(encoding="utf-8"))
        if data.get("package", {}).get("name", "").startswith("smudgy_"):
            manifests[path] = data
    return manifests


def normalized_lock(lock, names):
    lock = copy.deepcopy(lock)
    for package in lock["package"]:
        if package["name"] in names and "source" not in package:
            package["version"] = "<application-version>"
        for index, dependency in enumerate(package.get("dependencies", [])):
            fields = dependency.split()
            if len(fields) == 2 and fields[0] in names:
                package["dependencies"][index] = f"{fields[0]} <application-version>"
    return lock


def validate_bump(source, version):
    if git("rev-parse", "HEAD") != source:
        raise ValueError("Source changed after planning")
    manifests = package_manifests()
    if "ui/Cargo.toml" not in manifests:
        raise ValueError("No application manifests found")
    expected = {*manifests, "Cargo.lock", "assets/installer.iss"}
    changed = set(git("diff", "--name-only", source).splitlines())
    if changed != expected:
        raise ValueError(f"Version diff has missing or unexpected files: {changed ^ expected}")
    for path, after in manifests.items():
        before = tomllib.loads(git("show", f"{source}:{path}"))
        before["package"]["version"] = version
        if before != after:
            raise ValueError(f"Unexpected manifest change in {path}")
    before_installer = git("show", f"{source}:assets/installer.iss")
    expected_installer, count = re.subn(r'^#define MyAppVersion "[^"]+"',
                                      f'#define MyAppVersion "{version}"', before_installer, flags=re.M)
    if count != 1 or expected_installer != Path("assets/installer.iss").read_text(encoding="utf-8").strip():
        raise ValueError("Unexpected installer change")
    before_lock = tomllib.loads(git("show", f"{source}:Cargo.lock"))
    after_lock = tomllib.loads(Path("Cargo.lock").read_text(encoding="utf-8"))
    names = {data["package"]["name"] for data in manifests.values()}
    local = {item["name"]: item["version"] for item in after_lock["package"]
             if item["name"] in names and "source" not in item}
    if local != dict.fromkeys(names, version):
        raise ValueError("Cargo.lock does not contain every bumped application crate")
    if normalized_lock(before_lock, names) != normalized_lock(after_lock, names):
        raise ValueError("Cargo.lock changed dependency resolution, not just application versions")
    return sorted(expected)


def prepare(plan):
    if plan["action"] != "create":
        raise ValueError("Only a create plan can prepare a release")
    paths = validate_bump(plan["source_sha"], plan["version"])
    git("checkout", "--detach", plan["source_sha"])
    git("add", "--", *paths)
    git("-c", "user.name=github-actions[bot]", "-c", "user.email=41898282+github-actions[bot]@users.noreply.github.com",
        "commit", "-m", f"chore: nightly {plan['version']}")
    metadata = {"schema": 1, "version": plan["version"], "source_sha": plan["source_sha"],
                "created_at": datetime.now(timezone.utc).isoformat()}
    git("-c", "user.name=github-actions[bot]", "-c", "user.email=41898282+github-actions[bot]@users.noreply.github.com",
        "tag", "-a", plan["tag"], "-m", ANNOTATION + json.dumps(metadata, sort_keys=True))
    git("push", "origin", f"refs/tags/{plan['tag']}:refs/tags/{plan['tag']}")


def complete(tag):
    if not TAG_RE.fullmatch(tag):
        return # Stable releases and other preview channels have no nightly state.
    metadata = tag_metadata(tag)
    if metadata is None:
        return
    release = api(f"releases/tags/{tag}")
    if release["draft"] or not release["prerelease"]:
        raise ValueError("Nightly completion requires a published prerelease")
    version = metadata["version"]
    required = {f"smudgy-v{version}-{suffix}" for suffix in
                ("x86_64.flatpak", "aarch64.flatpak", "setup.exe", "universal.dmg")}
    assets = list(pages(f"releases/{release['id']}/assets", key=None))
    if not required <= {asset["name"] for asset in assets if asset["size"] > 0}:
        raise ValueError("Nightly release is missing platform assets")
    marker = receipt(metadata["source_sha"], tag)
    body = release.get("body") or ""
    if marker not in body:
        body += f"\n\nNightly source: `{metadata['source_sha']}`\n\n{marker}\n"
        api(f"releases/{release['id']}", payload={"body": body})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["plan", "prepare", "dispatch", "complete"])
    parser.add_argument("--plan", type=Path)
    parser.add_argument("--tag")
    args = parser.parse_args()
    if args.operation == "complete":
        if not args.tag:
            parser.error("complete requires --tag")
        complete(args.tag)
        return
    if args.plan is None:
        parser.error("this operation requires --plan")
    if args.operation == "plan":
        plan = make_plan()
        args.plan.write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
        print(plan["reason"])
        if output := os.environ.get("GITHUB_OUTPUT"):
            with open(output, "a", encoding="utf-8") as handle:
                handle.write(f"action={plan['action']}\nversion={plan['version']}\n")
        if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(summary, "a", encoding="utf-8") as handle:
                handle.write(plan["reason"] + "\n")
    else:
        plan = json.loads(args.plan.read_text(encoding="utf-8"))
        if args.operation == "prepare":
            prepare(plan)
        elif plan["action"] in {"create", "dispatch"}:
            command("gh", "workflow", "run", "release.yml", "--repo", REPOSITORY, "--ref", plan["tag"])
            print(f"Dispatched Release on {plan['tag']}.")


if __name__ == "__main__":
    main()
