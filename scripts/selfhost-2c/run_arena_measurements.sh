#!/usr/bin/env bash
set -euo pipefail

experiment_root=${1:?experiment output directory}
repo_root=${2:?repository root}
interpreter=$experiment_root/bin/bf-interpreter-candidate
input=$experiment_root/source/arena_micro.bfc
artifact_root=$experiment_root/artifacts/arena
run_root=$experiment_root/measurements/arena

if [[ -e "$run_root/runs.tsv" ]]; then
    echo "refusing to overwrite existing arena measurements: $run_root/runs.tsv" >&2
    exit 2
fi
mkdir -p "$run_root"
printf 'route\tphase\tpair\torder\tvariant\tstarted\tended\tstatus\toutput\tstderr\ttime\n' \
    > "$run_root/runs.tsv"

run_one() {
    local route=$1 variant=$2 phase=$3 pair=$4 order=$5
    local dir=$run_root/$route
    local artifact=$artifact_root/$route/$variant.bf
    local output=$dir/${phase}-p${pair}-${order}-${variant}.output
    local stderr=$dir/${phase}-p${pair}-${order}-${variant}.stderr
    local time_log=$dir/${phase}-p${pair}-${order}-${variant}.time
    local started ended status
    mkdir -p "$dir"
    started=$(date --iso-8601=ns)
    set +e
    /usr/bin/time -v -o "$time_log" "$interpreter" \
        --unlimited-tape --no-progress --stats --timings "$artifact" \
        < "$input" > "$output" 2> "$stderr"
    status=$?
    set -e
    ended=$(date --iso-8601=ns)
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$route" "$phase" "$pair" "$order" "$variant" "$started" "$ended" "$status" \
        "$output" "$stderr" "$time_log" >> "$run_root/runs.tsv"
    [[ $status -eq 0 ]]
}

for route in source cir; do
    run_one "$route" baseline warmup 0 A
    run_one "$route" candidate warmup 0 B
    for pair in $(seq 1 10); do
        if (( pair % 2 == 1 )); then
            run_one "$route" baseline pair "$pair" A
            run_one "$route" candidate pair "$pair" B
        else
            run_one "$route" candidate pair "$pair" B
            run_one "$route" baseline pair "$pair" A
        fi
    done
done
