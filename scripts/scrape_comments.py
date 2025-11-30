#!/usr/bin/env python3
"""
Scrape fresh PR review comments on apache/datafusion for benchmark triggers using a single
GitHub API call.

Behavior:
1. Gets recent PR comments via GitHub API
2. Filters to allowed authors and trigger phrases:
   - "run benchmarks"
       - "run benchmark <name>" where <name> is in ALLOWED_BENCHMARKS
3. For allowed users, schedules benchmark jobs; for non-allowed users posting a trigger,
   replies on the PR explaining the whitelist. For any "run benchmark" request that does
   not match a supported benchmark, replies with the supported benchmark list.
Only comments created within the last TIME_WINDOW_SECONDS are processed.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from json import loads
from shutil import which
from typing import Iterable, List, Mapping


ALLOWED_USERS = {"alamb", "Dandandan", "adriangb", "rluvaton"}
ALLOWED_BENCHMARKS = {"tpch", "clickbench_partitioned", "clickbench_extended"}
SCRIPT_MARKDOWN_LINK = "[scrape_comments.py](scripts/scrape_comments.py)"
_issue_comment_cache: dict[str, list[str]] = {}

REPO = os.environ.get("REPO", "apache/datafusion")
# for some reason the API doesn't return really recent comments unless we give a bit of a buffer
TIME_WINDOW_SECONDS = 3600
PER_PAGE = 100
SCRIPT_MARKDOWN_LINK = "[`scrape_comments.py`](https://github.com/alamb/datafusion-benchmarking/blob/main/scripts/scrape_comments.py)"


def ensure_tool(name: str) -> None:
    if which(name) is None:
        print(f"{name} is required", file=sys.stderr)
        sys.exit(1)


def run_gh_api(args: List[str]) -> str:
    cmd = ["gh", "api", *args]
    print(f"Running command: {' '.join(cmd)}")
    result = subprocess.run(cmd, check=False, capture_output=True, text=True)
    if result.returncode != 0:
        print(result.stderr.strip() or f"gh command failed: {' '.join(cmd)}", file=sys.stderr)
        sys.exit(result.returncode)
    return result.stdout


def fetch_recent_review_comments(now: datetime) -> Iterable[Mapping]:
    since = now - timedelta(seconds=TIME_WINDOW_SECONDS)
    since_iso = since.replace(microsecond=0).isoformat().replace("+00:00", "Z")
    print(
        f"Fetching review comments since {since_iso} "
        f"(window {TIME_WINDOW_SECONDS}s, per_page={PER_PAGE}) for {REPO}"
    )
    output = run_gh_api(
        [
            f"-XGET",
            f"/repos/{REPO}/issues/comments",
            "-f",
            f"per_page={PER_PAGE}",
            "-f",
            "sort=updated",
            "-f",
            "direction=desc",
            "-f",
            f"since={since_iso}",
        ]
    )
    try:
        data = loads(output)
    except Exception as exc:
        print(f"Failed to parse GitHub response: {exc}", file=sys.stderr)
        sys.exit(1)
    if not isinstance(data, list):
        return []
    return data


def fetch_issue_comment_bodies(pr_number: str) -> List[str]:
    if pr_number in _issue_comment_cache:
        return _issue_comment_cache[pr_number]
    print(f"Fetching existing issue comments for PR {pr_number}")
    output = run_gh_api(
        [
            "-XGET",
            f"/repos/{REPO}/issues/{pr_number}/comments",
            "-f",
            f"per_page={PER_PAGE}",
        ]
    )
    try:
        data = loads(output)
    except Exception as exc:
        print(f"Failed to parse issue comments: {exc}", file=sys.stderr)
        data = []
    bodies: List[str] = []
    if isinstance(data, list):
        for item in data:
            if isinstance(item, dict):
                body = item.get("body")
                if isinstance(body, str):
                    bodies.append(body)
    _issue_comment_cache[pr_number] = bodies
    return bodies


def already_posted(pr_number: str, body: str) -> bool:
    return body in fetch_issue_comment_bodies(pr_number)


# Detects benchmark trigger in comment body.
# Returns:
# - "" for "run benchmarks"
# - "<name>" for "run benchmark <name>" if <name> is in ALLOWED_BENCHMARKS
# - None if no trigger detected
def detect_benchmark(body: str) -> str | None:
    # check for "run benchmarks"
    match = re.match(r"^\s*run\s+benchmarks\s*$", body, flags=re.IGNORECASE)
    if match:
        return ""

    # check for "run benchmark <name>"
    match = re.match(r"^\s*run\s+benchmark\s+([a-zA-Z0-9_]+)\s*$", body, flags=re.IGNORECASE)
    if not match:
        return None
    requested = match.group(1)
    if requested in ALLOWED_BENCHMARKS:
        return requested

    return None

def pr_number_from_url(url: str) -> str:
    # URL format: https://api.github.com/repos/{owner}/{repo}/pulls/{number}
    # Example: 'https://api.github.com/repos/apache/datafusion/issues/19000
    parts = url.rstrip("/").split("/")
    return parts[-1] if parts else ""

# Returns the contents of a file with the benchmark command to run
#
# When bench is empty, runs this:
# BENCHMARKS="tpch10" ./gh_compare_branch.sh https://github.com/apache/datafusion/pull/<pr_number>
#
# when bench is not empty, such as "clickbench_partitioned", runs this:
# BENCHMARKS="clickbench_partitioned" ./gh_compare_branch.sh https://github.com/apache/datafusion/pull/<pr_number>
def get_benchmark_script(pr_number: str, bench: str) -> str:
    pr_url = f"https://github.com/{REPO}/pull/{pr_number}"
    if bench:
        return f"""BENCHMARKS="{bench}" ./gh_compare_branch.sh {pr_url}"""
    else:
        return f"""./gh_compare_branch.sh {pr_url}"""


def allowed_users_markdown() -> str:
    users = sorted(ALLOWED_USERS)
    return ", ".join(f"[{u}](https://github.com/{u})" for u in users)


def post_user_notice(pr_number: str, login: str, comment_url: str) -> None:
    pr_url = f"https://github.com/{REPO}/pull/{pr_number}"
    allowed = allowed_users_markdown()
    body = (
        f"Hi @{login}, thanks for the request ({comment_url}). "
        f"{SCRIPT_MARKDOWN_LINK} only responds to whitelisted users. "
        f"Allowed users: {allowed}."
    )
    if already_posted(pr_number, body):
        print(f"Notice already posted for PR {pr_number}, skipping")
        return
    print(f"Posting notice to {pr_url} for user @{login}")
    run_gh_api(
        [
            f"/repos/{REPO}/issues/{pr_number}/comments",
            "-X",
            "POST",
            "-f",
            f"body={body}",
        ]
    )
    fetch_issue_comment_bodies(pr_number).append(body)


def post_supported_benchmarks(pr_number: str, login: str, comment_url: str) -> None:
    pr_url = f"https://github.com/{REPO}/pull/{pr_number}"
    supported = ", ".join(sorted(ALLOWED_BENCHMARKS))
    body = (
        f"Hi @{login}, thanks for the request ({comment_url}). "
        f"{SCRIPT_MARKDOWN_LINK} only supports whitelisted benchmarks: {supported}. "
        "Please choose one of these with `run benchmark <name>`."
    )
    if already_posted(pr_number, body):
        print(f"Supported benchmarks notice already posted for PR {pr_number}, skipping")
        return
    print(f"Posting supported benchmarks to {pr_url} for user @{login}")
    run_gh_api(
        [
            f"/repos/{REPO}/issues/{pr_number}/comments",
            "-X",
            "POST",
            "-f",
            f"body={body}",
        ]
    )
    fetch_issue_comment_bodies(pr_number).append(body)


def process_comment(comment: Mapping, now: datetime) -> None:
    #print(f"Processing comment: {comment}")
    body = comment.get("body") or ""
    login = comment.get("user", {}).get("login") or ""
    comment_url = comment.get("html_url") or ""
    created_at = comment.get("created_at") or ""
    issue_url = comment.get("issue_url") or ""
    comment_id = comment.get("id") or ""

    print(f"Processing comment {comment_id} by {login} at {created_at}")

    pr_number = pr_number_from_url(issue_url)
    if not pr_number:
        print(f"  Could not extract PR number from URL {issue_url}")
        return

    bench = detect_benchmark(body)
    if bench is None:
        print(f"  No benchmark trigger detected in {body}")
        if body.strip().lower().startswith("run benchmark"):
            print("  Comment starts with 'run benchmark' but benchmark is unsupported.")
            if login not in ALLOWED_USERS:
                post_user_notice(pr_number, login, comment_url)
            else:
                post_supported_benchmarks(pr_number, login, comment_url)
        return

    if login not in ALLOWED_USERS:
        print(f"  User {login} not in allowed list")
        post_user_notice(pr_number, login, comment_url)
        return
    print("  Found comment from allowed user:", login)

    # create a file to run the benchmark in jobs/<pr_number>_<id>.sh
    # if it doesn't already exist
    file_name = f"jobs/{pr_number}_{comment_id}.sh"
    if os.path.exists(file_name):
        print(f"  Job file {file_name} already exists, skipping")
        return
    # check if a jobs/<pr_number>_<id>.sh.done file exists
    done_file_name = f"{file_name}.done"
    if os.path.exists(done_file_name):
        print(f"  Job done file {done_file_name} already exists, skipping")
        return

    script_content = get_benchmark_script(pr_number, bench)
    os.makedirs("jobs", exist_ok=True)
    pr_url = f"https://github.com/{REPO}/pull/{pr_number}"
    with open(file_name, "w") as f:
        f.write("# Automatically created by scrape_comments.py\n")
        f.write(f"# PR: {pr_url}\n")
        f.write(f"# Comment: {comment_url}\n")
        f.write(f"# User: {login}\n")
        f.write(f"# Body: {body}\n")
        f.write("\n")
        f.write(script_content)
        f.write("\n")
    print(f"  Scheduling benchmark run in {file_name}")



def main() -> None:
    ensure_tool("gh")

    now = datetime.now(timezone.utc)
    print(f"Current time (UTC): {now.isoformat()}")
    print(f"Time window: last {TIME_WINDOW_SECONDS} seconds")
    print(f"Repo: {REPO}")
    comments = list(fetch_recent_review_comments(now))
    print(f"Processing {len(comments)} comments")
    for comment in comments:
        process_comment(comment, now)


if __name__ == "__main__":
    main()
