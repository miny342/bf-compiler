#!/usr/bin/env bash
set -euo pipefail

experiment_root=$(cd "${1:?experiment output directory}" && pwd)
repo_root=$(cd "${2:?repository root}" && pwd)
local_option=${3:---disable-local-control-flow}
case "$local_option" in
    --enable-local-control-flow|--disable-local-control-flow) ;;
    *) echo "invalid local control-flow option: $local_option" >&2; exit 2 ;;
esac
out_root=$experiment_root/phase-portal-overhead
bfc=$experiment_root/bin/bfc-candidate
program=$experiment_root/source/phase-portal-overhead.bfc
config=$experiment_root/source/phase-config-overhead.json

if [[ -e "$out_root" ]] && find "$out_root" -mindepth 1 -print -quit | grep -q .; then
    echo "refusing to overwrite phase/portal overhead results: $out_root" >&2
    exit 2
fi
mkdir -p "$out_root"
artifact_id=$(python3 "$repo_root/scripts/selfhost-2c/write_overhead_phase_config.py" \
    "$experiment_root" "$local_option")

printf 'variant\tpair\torder\tstarted\tended\tstatus\toutput\tlog\ttime\tmetrics\texecute_ns\n' \
    > "$out_root/runs.tsv"

run_one() {
    local variant=$1 pair=$2 order=$3
    local output=$out_root/${variant}-p${pair}-${order}.output
    local log=$out_root/${variant}-p${pair}-${order}.log
    local time_log=$out_root/${variant}-p${pair}-${order}.time
    local metrics=$out_root/${variant}-p${pair}-${order}.metrics.json
    local started ended status execute_ns
    started=$(date --iso-8601=ns)
    set +e
    if [[ $variant == phase_on ]]; then
        /usr/bin/time -v -o "$time_log" "$bfc" \
            --run-ir "$local_option" --ir-progress-interval 86400s \
            --ir-metrics "$metrics" \
            --ir-phase-config "$config" --ir-artifact-id "$artifact_id" \
            "$program" < /dev/null > "$output" 2> "$log"
    else
        /usr/bin/time -v -o "$time_log" "$bfc" \
            --run-ir "$local_option" --ir-progress-interval 86400s \
            --ir-metrics "$metrics" "$program" \
            < /dev/null > "$output" 2> "$log"
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
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$variant" "$pair" "$order" "$started" "$ended" "$status" \
        "$output" "$log" "$time_log" "$metrics" "$execute_ns" >> "$out_root/runs.tsv"
    [[ $status -eq 0 ]]
}

for variant in phase_off phase_on; do
    run_one "$variant" warmup W
done
for pair in $(seq 1 10); do
    if (( pair % 2 == 1 )); then
        run_one phase_on "$pair" A
        run_one phase_off "$pair" B
    else
        run_one phase_off "$pair" B
        run_one phase_on "$pair" A
    fi
done
