set -e -x

REPO_URL='https://github.com/yjshen/arrow-datafusion'
BRANCH_NAME='agg_filter'

TPCH_BENCHMARK="cargo run --release --bin tpch -- benchmark datafusion --iterations 5 --path /home/alamb/tpch_data/parquet_data_SF1 --format parquet"


OUTDIR="/home/alamb/"`date  +%Y-%m-%d-%s`
#OUTDIR="/home/alamb/2023-04-10-TEST"
echo "Writing results to $OUTDIR"
mkdir -p "${OUTDIR}"

pushd ~/arrow-datafusion
pwd

# Fetch the remote branch
git checkout main
git branch -D "${BRANCH_NAME}" || true # clean any old copy
git branch -D "main_compare" || true # clean any old copy

git fetch --force "${REPO_URL}" "${BRANCH_NAME}:${BRANCH_NAME}"
git fetch -p apache

# checkout main at the right location
MERGE_BASE=`git merge-base apache/main ${BRANCH_NAME}`
echo "Checking out merge-base ${MERGE_BASE} where ${BRANCH_NAME} deviates from HEAD"
git checkout -b main_compare ${MERGE_BASE}

# run the benchmark (initially focuced on TPCH )
cd benchmarks
${TPCH_BENCHMARK} -o ${OUTDIR}/tpch_main.json


# checkout branch code and rerun the benchmarks
git checkout "${BRANCH_NAME}"
${TPCH_BENCHMARK} -o "${OUTDIR}/tpch_${BRANCH_NAME}.json"


# compare the results
source ~/venv/bin/activate
pip install rich
python3 ~/arrow-datafusion/benchmarks/compare.py  ${OUTDIR}/tpch_main.json  ${OUTDIR}/tpch_${BRANCH_NAME}.json
