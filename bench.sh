set -e -x
##
# This script runs the tpch query benchmarks against a branch and main
##

REPO_URL='https://github.com/yjshen/arrow-datafusion'
BRANCH_NAME='agg_filter'

REPO_URL=https://github.com/synnada-ai/arrow-datafusion.git
BRANCH_NAME=feature/stream_groupby4

NICE_BRANCH_NAME="$(urlencode ${BRANCH_NAME})"

# Commands to run
TPCH_BENCHMARK_SF1_PARQUET="cargo run --release --bin tpch -- benchmark datafusion --iterations 5 --path /home/alamb/tpch_data/parquet_data_SF1 --format parquet"
TPCH_BENCHMARK_SF1_MEM="cargo run --release --bin tpch -- benchmark datafusion --iterations 5 --path /home/alamb/tpch_data/parquet_data_SF1 --format parquet -m "

#OUTDIR="/home/alamb/benchmarking/$(date +%Y-%m-%d-%s)_${NICE_BRANCH_NAME}"
OUTDIR="/home/alamb/benchmarking/${NICE_BRANCH_NAME}"
#OUTDIR="/home/alamb/2023-04-10-TEST"

OUT_TPCH_SF1_PARQUET_MAIN="${OUTDIR}/tpch_sf1_parquet_main.json"
OUT_TPCH_SF1_MEM_MAIN="${OUTDIR}/tpch_sf1_parquet_mem.json"
OUT_TPCH_SF1_PARQUET_BRANCH="${OUTDIR}/tpch_sf1_parquet_branch.json"
OUT_TPCH_SF1_MEM_BRANCH="${OUTDIR}/tpch_sf1_mem_branch.json"

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
${TPCH_BENCHMARK_SF1_PARQUET} -o "${OUT_TPCH_SF1_PARQUET_MAIN}"
${TPCH_BENCHMARK_SF1_MEM}     -o "${OUT_TPCH_SF1_MEM_MAIN}"

# checkout branch code and rerun the benchmarks
git checkout "${BRANCH_NAME}"
${TPCH_BENCHMARK_SF1_PARQUET} -o "${OUT_TPCH_SF1_PARQUET_BRANCH}"
${TPCH_BENCHMARK_SF1_MEM}     -o "${OUT_TPCH_SF1_MEM_BRANCH}"


# compare the results
source ~/venv/bin/activate
pip install rich
echo "****** TPCH SF1 (Parquet) ******"
python3 ~/arrow-datafusion/benchmarks/compare.py  "${OUT_TPCH_SF1_PARQUET_MAIN}" "${OUT_TPCH_SF1_PARQUET_BRANCH}"

echo "****** TPCH SF1 (mem) ******"
python3 ~/arrow-datafusion/benchmarks/compare.py  "${OUT_TPCH_SF1_MEM_MAIN}"     "${OUT_TPCH_SF1_MEM_BRANCH}"
