##!/bin/bash
# This script is the same as build_datafusion_cli.sh  except it
# uses the old build command for datafusion-cli which used to not be in the main workspace
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
(cd datafusion-cli && cargo build --release --bin datafusion-cli)
popd
cp -f "${DATAFUSION_DIR}"/datafusion-cli/target/release/datafusion-cli "${OUTPUT}"
echo "datafusion-cli at rev: $REV saved to $OUTPUT"
