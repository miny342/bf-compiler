#!/usr/bin/env bash
set -euo pipefail

experiment_root=${1:?experiment output directory}
repo_root=${2:?repository root}
family_filter=${3:-all}
input_filter=${4:-all}
run_root=$experiment_root/measurements/runs
artifact_root=$experiment_root/artifacts
input_root=$experiment_root/source
interpreter=$experiment_root/bin/bf-interpreter-candidate

if [[ -e "$run_root/runs.tsv" ]]; then
    echo "refusing to overwrite existing measurements: $run_root/runs.tsv" >&2
    exit 2
fi
mkdir -p "$run_root"
printf 'family\tinput\tphase\tpair\torder\tvariant\tstarted\tended\tstatus\toutput\tstderr\n' \
    > "$run_root/runs.tsv"

run_one() {
    local family=$1 input_name=$2 variant=$3 phase=$4 pair=$5 order=$6
    local artifact=$artifact_root/$family/$variant.bf
    local input=$input_root/$input_name.bfc
    local set_root=$run_root/$family/$input_name
    local output=$set_root/${phase}-p${pair}-${order}-${variant}.output
    local stderr=$set_root/${phase}-p${pair}-${order}-${variant}.stderr
    local time_log=$set_root/${phase}-p${pair}-${order}-${variant}.time
    local started ended status
    mkdir -p "$set_root"
    started=$(date --iso-8601=ns)
    set +e
    /usr/bin/time -v -o "$time_log" "$interpreter" \
        --unlimited-tape --no-progress --stats --timings "$artifact" \
        < "$input" > "$output" 2> "$stderr"
    status=$?
    set -e
    ended=$(date --iso-8601=ns)
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$family" "$input_name" "$phase" "$pair" "$order" "$variant" \
        "$started" "$ended" "$status" "$output" "$stderr" \
        >> "$run_root/runs.tsv"
    if [[ $status -ne 0 ]]; then
        echo "measurement failed: $family/$input_name pair=$pair variant=$variant" >&2
        exit "$status"
    fi
}

measure_set() {
    local family=$1 input_name=$2
    run_one "$family" "$input_name" baseline warmup 0 A
    run_one "$family" "$input_name" candidate warmup 0 B
    for pair in $(seq 1 10); do
        if (( pair % 2 == 1 )); then
            run_one "$family" "$input_name" baseline pair "$pair" A
            run_one "$family" "$input_name" candidate pair "$pair" B
        else
            run_one "$family" "$input_name" candidate pair "$pair" B
            run_one "$family" "$input_name" baseline pair "$pair" A
        fi
    done
}

for family in source cir; do
    [[ $family_filter == all || $family_filter == "$family" ]] || continue
    for input_name in hello stage5_functions stage8_aggregates; do
        [[ $input_filter == all || $input_filter == "$input_name" ]] || continue
        measure_set "$family" "$input_name"
    done
done
