#!/usr/bin/env bash
set -euo pipefail

experiment_root=${1:?experiment output directory}
repo_root=${2:?repository root}
out_root=$experiment_root/ir-metrics
bfc=$experiment_root/bin/bfc-candidate
source_program=$experiment_root/source/stage2-compiler.bfc
cir_program=$experiment_root/artifacts/cir/stage2-compiler.cir

if [[ -e "$out_root" ]] && find "$out_root" -mindepth 1 -print -quit | grep -q .; then
    echo "refusing to overwrite existing IR metrics: $out_root" >&2
    exit 2
fi
mkdir -p "$out_root"
for family in source cir; do
    for input_name in hello stage5_functions stage8_aggregates; do
        for variant in baseline candidate; do
            dir=$out_root/$family/$input_name/$variant
            mkdir -p "$dir"
            args=(--run-ir --ir-progress-interval 15s --ir-metrics "$dir/metrics.json")
            if [[ $variant == candidate ]]; then
                args+=(--enable-2c)
            else
                args+=(--disable-2c)
            fi
            if [[ $family == source ]]; then
                args+=("$source_program")
            else
                args+=(--cir-input "$cir_program")
            fi
            /usr/bin/time -v -o "$dir/time" "$bfc" "${args[@]}" \
                < "$experiment_root/source/$input_name.bfc" \
                > "$dir/output.bf" 2> "$dir/run.log"
        done
    done
done
