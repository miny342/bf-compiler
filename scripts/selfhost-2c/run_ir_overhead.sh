#!/usr/bin/env bash
set -euo pipefail

experiment_root=${1:?experiment output directory}
repo_root=${2:?repository root}
out_root=$experiment_root/ir-overhead
bfc=$experiment_root/bin/bfc-candidate
program=$experiment_root/source/ir-overhead.bfc

if [[ -e "$out_root" ]] && find "$out_root" -mindepth 1 -print -quit | grep -q .; then
    echo "refusing to overwrite existing IR overhead results: $out_root" >&2
    exit 2
fi
mkdir -p "$out_root"
printf 'variant\tpair\torder\tstarted\tended\tstatus\toutput\tlog\ttime\texecute_ns\n' \
    > "$out_root/runs.tsv"

run_one() {
    local variant=$1 pair=$2 order=$3
    local output=$out_root/${variant}-p${pair}-${order}.output
    local log=$out_root/${variant}-p${pair}-${order}.log
    local time_log=$out_root/${variant}-p${pair}-${order}.time
    local started ended status execute_ns
    started=$(date --iso-8601=ns)
    set +e
    if [[ $variant == on ]]; then
        /usr/bin/time -v -o "$time_log" "$bfc" \
            --run-ir --disable-local-control-flow --ir-progress-interval 86400s "$program" \
            < <(printf '\040') > "$output" 2> "$log"
    else
        /usr/bin/time -v -o "$time_log" "$bfc" \
            --run-ir --disable-local-control-flow --no-ir-transitions --ir-progress-interval 86400s "$program" \
            < <(printf '\040') > "$output" 2> "$log"
    fi
    status=$?
    set -e
    ended=$(date --iso-8601=ns)
    if [[ $status -eq 0 ]]; then
        execute_ns=$(sed -n 's/.*phase=execute status=finished elapsed_ns=\([0-9][0-9]*\).*/\1/p' "$log")
        [[ $execute_ns =~ ^[0-9]+$ ]]
    else
        execute_ns=0
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$variant" "$pair" "$order" "$started" "$ended" "$status" \
        "$output" "$log" "$time_log" "$execute_ns" >> "$out_root/runs.tsv"
    [[ $status -eq 0 ]]
}

for pair in $(seq 1 10); do
    if (( pair % 2 == 1 )); then
        run_one on "$pair" A
        run_one off "$pair" B
    else
        run_one off "$pair" B
        run_one on "$pair" A
    fi
done
