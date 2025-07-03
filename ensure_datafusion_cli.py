    #!/usr/bin/env python3
"""
DataFusion CLI build script for ClickBench analysis

This script finds all commits to the datafusion repository in the last N days
and calls `build_datafusion_cli.sh` to build the datafusion-cli binary for each commit.

It runs up to num_builds in parallel, each in its own datafusion checkout

The datafusion checkouts are named as follows:

datafusion
datafusion2
datafusion3
...

Prerequisites:
    TBD

Usage:
    # Compiles a datafusion-cli binary for the last 7 days, up to 5 builds in parallel
    # leaving the builds in the builds/ directory.
    # uses directories datafusion, datafusion2, datafusion3, datafusion4, and
    # datafusion5 for the checkouts.
    python ensure_datafusion_cli.py --num-builds 5 --days 7
    
    Here is an example of how to run this script printing status to a log file:
    PYTHONUNBUFFERED=1 nice python ensure_datafusion_cli.py > build.log 2>&1
"""

import argparse
import os
import subprocess
import sys
from datetime import datetime, timedelta
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
import threading

def get_recent_commits(datafusion_dir, days):
    """Get commits from the last N days from the datafusion repository."""
    print(f"Finding commits from the last {days} days in {datafusion_dir}")

    # Calculate the date N days ago
    since_date = (datetime.now() - timedelta(days=days)).strftime('%Y-%m-%d')

    try:
        # Get commits from the last N days
        cmd = [
            'git', '--no-pager', 'log',
            f'--since={since_date}',
            '--pretty=format:%H',  # Just the commit hash
            '--reverse',  # Oldest first
            'origin/main'
        ]

        result = subprocess.run(cmd, cwd=datafusion_dir, capture_output=True, text=True, check=True)
        commits = result.stdout.strip().split('\n') if result.stdout.strip() else []

        print(f"Found {len(commits)} commits from the last {days} days")
        return commits

    except subprocess.CalledProcessError as e:
        print(f"Error getting commits: {e}")
        return []

def setup_datafusion_checkout(checkout_dir, source_dir):
    """Setup a datafusion checkout directory if it doesn't exist."""
    if not os.path.exists(checkout_dir):
        print(f"Creating datafusion checkout at {checkout_dir}")
        try:
            # Clone from the existing datafusion directory
            subprocess.run(['git', 'clone', source_dir, checkout_dir], check=True)
        except subprocess.CalledProcessError as e:
            print(f"Error creating checkout {checkout_dir}: {e}")
            return False
    return True

def build_commit(commit_hash, datafusion_dir, thread_id):
    """Build a specific commit using the build script."""
    print(f"[Thread {thread_id}] Building commit {commit_hash[:8]} in {datafusion_dir}")

    try:
        # Set environment variable for the datafusion directory
        env = os.environ.copy()
        env['DATAFUSION_DIR'] = datafusion_dir

        # Run the build script
        result = subprocess.run(
            ['bash', './build_datafusion_cli.sh', commit_hash],
            env=env,
            capture_output=True,
            text=True,
            check=True
        )

        print(f"[Thread {thread_id}] Successfully built {commit_hash[:8]}")
        return commit_hash, True, result.stdout

    except subprocess.CalledProcessError as e:
        error_msg = f"Error building {commit_hash[:8]}: {e.stderr}"
        print(f"[Thread {thread_id}] {error_msg}")
        return commit_hash, False, error_msg

def check_existing_builds():
    """Check what builds already exist."""
    builds_dir = Path('builds')
    if not builds_dir.exists():
        return set()

    existing_builds = set()
    for build_file in builds_dir.iterdir():
        if build_file.is_file() and build_file.name.startswith('datafusion-cli@'):
            # Extract commit hash from filename (format: datafusion-cli@<commit>@<timestamp>)
            parts = build_file.name.split('@')
            if len(parts) >= 2:
                existing_builds.add(parts[1])

    return existing_builds

def main():
    parser = argparse.ArgumentParser(description='Build DataFusion CLI binaries for recent commits')
    parser.add_argument('--num-builds', type=int, default=2,
                        help='Number of parallel builds (default: 2)')
    parser.add_argument('--days', type=int, default=7,
                        help='Number of days to look back for commits (default: 7)')
    parser.add_argument('--datafusion-dir', default='datafusion',
                        help='Primary datafusion checkout directory (default: datafusion)')

    args = parser.parse_args()

    # Ensure the primary datafusion directory exists
    if not os.path.exists(args.datafusion_dir):
        print(f"Error: Primary datafusion directory '{args.datafusion_dir}' does not exist")
        print("Please clone the datafusion repository first:")
        print("git clone https://github.com/apache/datafusion.git")
        sys.exit(1)

    # Get recent commits
    commits = get_recent_commits(args.datafusion_dir, args.days)
    if not commits:
        print("No commits found in the specified time range")
        return

    # Check which builds already exist
    existing_builds = check_existing_builds()
    commits_to_build = [c for c in commits if c not in existing_builds]

    if not commits_to_build:
        print("All commits in the time range have already been built")
        return

    print(f"Need to build {len(commits_to_build)} commits (skipping {len(commits) - len(commits_to_build)} existing builds)")

    # Setup datafusion checkout directories
    checkout_dirs = []
    for i in range(args.num_builds):
        if i == 0:
            checkout_dir = args.datafusion_dir
        else:
            checkout_dir = f"{args.datafusion_dir}{i + 1}"

        if setup_datafusion_checkout(checkout_dir, args.datafusion_dir):
            checkout_dirs.append(checkout_dir)
        else:
            print(f"Failed to setup checkout directory {checkout_dir}")

    if not checkout_dirs:
        print("No valid checkout directories available")
        sys.exit(1)

    print(f"Using {len(checkout_dirs)} checkout directories: {checkout_dirs}")

    # Build commits in parallel
    successful_builds = 0
    failed_builds = 0

    with ThreadPoolExecutor(max_workers=len(checkout_dirs)) as executor:
        # Submit build tasks
        future_to_commit = {}
        checkout_index = 0

        for commit in commits_to_build:
            checkout_dir = checkout_dirs[checkout_index % len(checkout_dirs)]
            thread_id = checkout_index % len(checkout_dirs) + 1

            future = executor.submit(build_commit, commit, checkout_dir, thread_id)
            future_to_commit[future] = commit
            checkout_index += 1

        # Process completed builds
        for future in as_completed(future_to_commit):
            commit, success, output = future.result()

            if success:
                successful_builds += 1
            else:
                failed_builds += 1
                print(f"Build failed for {commit[:8]}: {output}")

    print(f"\nBuild summary:")
    print(f"  Successful: {successful_builds}")
    print(f"  Failed: {failed_builds}")
    print(f"  Total: {len(commits_to_build)}")

if __name__ == '__main__':
    main()
