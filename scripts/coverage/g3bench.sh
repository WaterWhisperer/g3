#!/bin/sh

set -e

SCRIPTS_DIR=$(dirname "$0")
PROJECT_DIR=$(realpath "${SCRIPTS_DIR}/../..")


TEST_NAME="bench-ci"
. "${SCRIPTS_DIR}/enter.sh"

# build
cargo build -p g3bench -p g3mkcert -p g3proxy -p g3proxy-ctl -p g3statsd

all_binaries=$(find target/debug/ -maxdepth 1 -type f -perm /111 | awk '{print "-object "$0}')
all_objects=$(find target/debug/deps/ -type f -perm /111 -not -name "*.so" | awk '{print "-object "$0}')

# run g3bench tests

cargo test -p g3bench

RUN_DIR="${SCRIPTS_DIR}/g3bench"
. "${RUN_DIR}/run.sh"

# get all profraw files generated in each test
profraw_files=$(find . -type f -regex ".*/${TEST_NAME}.*\.profraw")

# get indexed profile data file
cargo profdata -- merge -o "${PROF_DATA_FILE}" ${profraw_files}

# report to console

IGNORE_FLAGS="--ignore-filename-regex=.cargo \
    --ignore-filename-regex=rustc \
    --ignore-filename-regex=target/debug/build \
    --ignore-filename-regex=g3mkcert \
    --ignore-filename-regex=g3fcgen \
    --ignore-filename-regex=g3proxy \
    --ignore-filename-regex=g3tiles \
    --ignore-filename-regex=g3keymess \
    --ignore-filename-regex=g3iploc"

echo "==== Coverage for all ===="
cargo cov -- report --use-color --instr-profile="${PROF_DATA_FILE}" ${IGNORE_FLAGS} ${all_binaries} ${all_objects}
cargo cov -- export --format=lcov --instr-profile="${PROF_DATA_FILE}" ${IGNORE_FLAGS} ${all_binaries} ${all_objects} > output.lcov
