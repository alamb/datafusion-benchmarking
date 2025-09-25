# Build datafusion binaries for different releases

## 45 has datafusion-cli in a different path
DATAFUSION_DIR=datafusion1 ./build_datafusion_cli_old.sh 42.0.0 &
DATAFUSION_DIR=datafusion2 ./build_datafusion_cli_old.sh 43.0.0 &
DATAFUSION_DIR=datafusion3 ./build_datafusion_cli_old.sh 44.0.0 &
DATAFUSION_DIR=datafusion4 ./build_datafusion_cli_old.sh 45.0.0 &
DATAFUSION_DIR=datafusion5 ./build_datafusion_cli.sh 46.0.0 &
DATAFUSION_DIR=datafusion6 ./build_datafusion_cli.sh 47.0.0 &
DATAFUSION_DIR=datafusion7 ./build_datafusion_cli.sh 48.0.1 &
DATAFUSION_DIR=datafusion8 ./build_datafusion_cli.sh 49.0.2 &
DATAFUSION_DIR=datafusion9 ./build_datafusion_cli.sh branch-50 &

wait
