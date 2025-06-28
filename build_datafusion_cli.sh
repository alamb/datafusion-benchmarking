##!/bin/bash
# This script builds datafusion-cli at a particular git sha/rev (tag, branch, etc.)
#
# It outputs the built binary to builds/datafusion-cli@<rev>@<timestamp>
#
# For example, datafusion-cli@46.0.0@2025-03-04T22:00:39+08:00 came from the
# git sha 46.0.0, which was committed (not built!) at the timestamp 2025-03-04T22:00:39+08:00.

# requires
# git clone git@github.com:apache/datafusion.git
#
# when run via run_clickbench.py
# 1. Build the datafusion-cli at that rev
# 2. Save datafusion-cli at that rev to builds/datafusion-cli@<rev>@<timestamp>

# Note the naming of the binary is important, as it will be used to identify the
# version of datafusion-cli

# Usage: ./build_datafusion_cli.sh <rev>
#
# Alternate checkout directory can be specified via DATAFUSION_DIR environment variable.
# DATAFUSION_DIR=datafusion2 ./build_datafusion_cli.sh 1.0.0
set -e
if [ -z "$1" ]; then
  echo "Usage: $0 <rev>"
  exit 1
fi

REV=$1
if [ -z "$DATAFUSION_DIR" ]; then
  DATAFUSION_DIR=datafusion
fi

mkdir -p builds
echo "Building datafusion-cli in ${DATAFUSION_DIR} at rev: $REV"

pushd "$DATAFUSION_DIR" || exit 1
git stash
git checkout $REV
# figure out the commit timestamp from the git log
REV_TIME=`git --no-pager log -1 --pretty='format:%cI' --date='format:%Y-%m-%dZ%H:%M:%S'`
OUTPUT="builds/datafusion-cli@${REV}@${REV_TIME}"

echo "Output will be saved to: \"${OUTPUT}\""
cargo build --release --bin datafusion-cli
popd
cp -f "${DATAFUSION_DIR}"/target/release/datafusion-cli "${OUTPUT}"
echo "datafusion-cli at rev: $REV saved to $OUTPUT"
