#!/usr/bin/env bash
set -euo pipefail

experiment_root=${1:?experiment output directory}
repo_root=${2:?repository root}
out_root=$experiment_root/ir-phase-portal
bfc=$experiment_root/bin/bfc-candidate
source_program=$experiment_root/source/stage2-compiler.bfc
cir_program=$experiment_root/artifacts/cir/stage2-compiler.cir

if [[ -e "$out_root" ]] && find "$out_root" -mindepth 1 -print -quit | grep -q .; then
    echo "refusing to overwrite existing phase/portal results: $out_root" >&2
    exit 2
fi
mkdir -p "$out_root"

source_id=$(sha256sum "$source_program" | awk '{print $1}')
cir_id=$(sha256sum "$cir_program" | awk '{print $1}')
python3 "$repo_root/scripts/selfhost-2c/write_phase_configs.py" \
    "$experiment_root" "$source_id" "$cir_id"

for family in source cir; do
    for input_name in hello stage5_functions stage8_aggregates; do
        dir=$out_root/$family/$input_name
        mkdir -p "$dir"
        if [[ $family == source ]]; then
            program_args=("$source_program" "--ir-phase-config"
                "$experiment_root/source/phase-config-source.json"
                "--ir-artifact-id" "$source_id")
        else
            program_args=(--cir-input "$cir_program" "--ir-phase-config"
                "$experiment_root/source/phase-config-cir.json"
                "--ir-artifact-id" "$cir_id")
        fi
        /usr/bin/time -v -o "$dir/time" "$bfc" \
            --run-ir --ir-progress-interval 15s \
            --ir-metrics "$dir/metrics.json" "${program_args[@]}" \
            < "$experiment_root/source/$input_name.bfc" \
            > "$dir/output.bf" 2> "$dir/run.log"
    done
done
