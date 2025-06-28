#!/usr/bin/env python3
import argparse
import os
import subprocess
import glob
import re
import time

def main():
    parser = argparse.ArgumentParser(description="Execute benchmarks using specified datafusion-cli binaries")
    parser.add_argument('--output-dir', help='Directory to write benchmark results', default="results")
    parser.add_argument('--binary-pattern', default='datafusion-cli@*',
                       help='Pattern to match datafusion-cli binaries in builds/ directory (default: datafusion-cli@*)')
    parser.add_argument('--benchmarks', nargs='+', default=['clickbench'],
                       help='Benchmarks to run (default: clickbench)')
    args = parser.parse_args()

    # Create output directory if it doesn't exist
    if not os.path.exists(args.output_dir):
        os.makedirs(args.output_dir)

    # Find all datafusion-cli binaries matching the pattern
    builds_dir = os.path.join(os.path.dirname(__file__), 'builds')
    binary_pattern = os.path.join(builds_dir, args.binary_pattern)
    binaries = glob.glob(binary_pattern)

    if not binaries:
        print(f"No datafusion-cli binaries found matching pattern: {args.binary_pattern}")
        print(f"Looking in directory: {builds_dir}")
        return

    print(f"Found {len(binaries)} datafusion-cli binaries:")
    for binary in binaries:
        print(f"  {os.path.basename(binary)}")

    # Run benchmarks for each binary
    for binary_path in binaries:
        binary_name = os.path.basename(binary_path)

        # Parse version and timestamp from binary name: datafusion-cli@VERSION@TIMESTAMP
        parts = binary_name.split('@')
        if len(parts) >= 3:
            version = parts[1]
            timestamp = parts[2]
        else:
            version = "unknown"
            timestamp = "unknown"

        print(f"\nRunning benchmarks with {binary_name}")
        print(f"Version: {version}, Timestamp: {timestamp}")

        # Run each benchmark
        for benchmark in args.benchmarks:
            if benchmark == 'clickbench':
                run_clickbench_benchmark(binary_path, version, timestamp, args.output_dir)
            else:
                print(f"Unknown benchmark: {benchmark}")

def run_clickbench_benchmark(binary_path, version, timestamp, output_dir):
    """Run clickbench benchmark with the specified datafusion-cli binary"""
    print(f"  Running clickbench benchmark...")

    # Make the binary executable
    os.chmod(binary_path, 0o755)

    try:
        # Run the clickbench script
        script_path = os.path.join(os.path.dirname(__file__), 'run_clickbench.py')
        cmd = [
            'python3', script_path,
            '--output-dir', output_dir,
            '--git-revision', version,
            '--git-revision-timestamp', timestamp,
            '--datafusion-binary', binary_path
        ]

        print(f"    Executing: {' '.join(cmd)}")
        # Execute the command and pipe the output back to the console
        result = subprocess.run(cmd)

        if result.returncode == 0:
            print(f"    ✓ Clickbench benchmark completed successfully")
        else:
            print(f"    ✗ Clickbench benchmark failed:")
            print(f"    stdout: {result.stdout}")
            print(f"    stderr: {result.stderr}")

    except Exception as e:
        print(f"    ✗ Error running clickbench benchmark: {e}")

if __name__ == "__main__":
    main()
