#!/usr/bin/env bash
set -euo pipefail

experiment_root=${1:?experiment output directory}
repo_root=${2:?repository root}
out_root=$experiment_root/profile-samples/source-hello
interpreter=$experiment_root/bin/bf-interpreter-candidate
input=$experiment_root/source/hello.bfc

if [[ -e "$out_root" ]] && find "$out_root" -mindepth 1 -print -quit | grep -q .; then
    echo "refusing to overwrite existing profile samples: $out_root" >&2
    exit 2
fi
mkdir -p "$out_root"
for variant in baseline candidate; do
    artifact=$experiment_root/artifacts/source/$variant.bf
    map=$experiment_root/artifacts/source/$variant.bfmap.json
    /usr/bin/time -v -o "$out_root/$variant.time" "$interpreter" \
        --unlimited-tape --no-progress --profile-map "$map" \
        --profile-mode sample --profile-sample-interval 2ms \
        --profile-output "$out_root/$variant.profile.json" \
        --profile-format json "$artifact" < "$input" \
        > "$out_root/$variant.output" 2> "$out_root/$variant.log"
done
