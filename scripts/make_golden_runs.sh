#!/usr/bin/env bash
# Rewrite the golden run folders under schema/golden/runs/ from today's code:
#
#   scripts/make_golden_runs.sh
#
# Each is written by its Rust test on the simulator (Random unpaced, the Recorder
# in real time, as it always runs), with the simulator's fixed seed, and
# normalized (wall times, the run's unique identity and the build fields fixed). Without this script the same tests only compare
# today's code against the folders. Review the difference before committing it:
# a changed golden folder is a changed run record.
#
# Written for bash 3.2, the bash macOS ships.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
HERTZFORGE_WRITE_GOLDEN_RUNS=1 cargo test --locked -p sds-control --test random_golden_run \
  -- --exact todays_code_writes_the_golden_random_run_folder --nocapture </dev/null
HERTZFORGE_WRITE_GOLDEN_RUNS=1 cargo test --locked -p sds-control --test sine_golden_run \
  -- --exact todays_code_writes_the_golden_sine_run_folder --nocapture </dev/null
HERTZFORGE_WRITE_GOLDEN_RUNS=1 cargo test --locked -p sds-control --test recorder_golden_run \
  -- --exact todays_code_writes_the_golden_recorder_run_folder --nocapture </dev/null
HERTZFORGE_WRITE_GOLDEN_RUNS=1 cargo test --locked -p sds-control --test classical_shock_golden_run \
  -- --exact todays_code_writes_the_golden_classical_shock_run_folder --nocapture </dev/null
HERTZFORGE_WRITE_GOLDEN_RUNS=1 cargo test --locked -p sds-control --test mixed_golden_run \
  -- --exact todays_code_writes_the_golden_mixed_run_folder --nocapture </dev/null
