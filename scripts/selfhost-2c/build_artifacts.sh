#!/usr/bin/env bash
set -euo pipefail

run_root=${1:?new output directory under the repository}
repo_root=${2:-$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)}
baseline_commit=bd3560470b9f7c5c8f06f413d1a8cd6224878c34

if [[ -e "$run_root" ]] && find "$run_root" -mindepth 1 -print -quit | grep -q .; then
    echo "output directory must be new or empty: $run_root" >&2
    exit 2
fi
mkdir -p "$run_root"/{baseline-checkout,bin,build,artifacts/{source,cir,arena/{source,cir}},source}
mkdir -p "$run_root/cargo-tmp"

git -C "$repo_root" archive --format=tar "$baseline_commit" \
    | tar -xf - -C "$run_root/baseline-checkout"

TMPDIR="$run_root/cargo-tmp" CARGO_TARGET_DIR="$run_root/build/baseline" cargo build --release \
    --manifest-path "$run_root/baseline-checkout/Cargo.toml" \
    -p bf-compiler -p bf-interpreter
TMPDIR="$run_root/cargo-tmp" CARGO_TARGET_DIR="$run_root/build/candidate" cargo build --release \
    --manifest-path "$repo_root/Cargo.toml" \
    -p bf-compiler -p bf-interpreter

baseline_bfc=$run_root/build/baseline/release/bfc
candidate_bfc=$run_root/build/candidate/release/bfc
cp "$baseline_bfc" "$run_root/bin/bfc-baseline"
cp "$candidate_bfc" "$run_root/bin/bfc-candidate"
cp "$run_root/build/candidate/release/bf-interpreter" "$run_root/bin/bf-interpreter-candidate"

baseline_scripts=$run_root/baseline-checkout/scripts
"$baseline_scripts/concat-stage2-compiler.sh" main > "$run_root/source/stage2-compiler.bfc"
"$baseline_scripts/concat-stage2-compiler.sh" cir > "$run_root/source/stage2-cir-compiler.bfc"
for input in hello stage5_functions stage8_aggregates; do
    cp "$run_root/baseline-checkout/selfhost/stage2/examples/$input.bfc" \
        "$run_root/source/$input.bfc"
done

generate_source_artifact() {
    local variant=$1 compiler=$2
    local options=(--unlimited-tape --profile-map-output
        "$run_root/artifacts/source/$variant.bfmap.json"
        --profile-granularity continuation)
    if [[ $variant == candidate ]]; then
        options+=(--enable-2c)
    fi
    "$compiler" "${options[@]}" "$run_root/source/stage2-compiler.bfc" \
        > "$run_root/artifacts/source/$variant.bf"
}

generate_source_artifact baseline "$baseline_bfc"
generate_source_artifact candidate "$candidate_bfc"

"$baseline_bfc" --run-ir "$run_root/source/stage2-cir-compiler.bfc" \
    < "$run_root/source/stage2-compiler.bfc" \
    > "$run_root/artifacts/cir/stage2-compiler.cir" \
    2> "$run_root/artifacts/cir/stage2-compiler-generation.log"

generate_cir_artifact() {
    local variant=$1 compiler=$2
    local options=(--cir-input "$run_root/artifacts/cir/stage2-compiler.cir"
        --unlimited-tape --profile-map-output
        "$run_root/artifacts/cir/$variant.bfmap.json"
        --profile-granularity continuation)
    if [[ $variant == candidate ]]; then
        options+=(--enable-2c)
    fi
    "$compiler" "${options[@]}" > "$run_root/artifacts/cir/$variant.bf"
}

generate_cir_artifact baseline "$baseline_bfc"
generate_cir_artifact candidate "$candidate_bfc"

bash "$repo_root/scripts/selfhost-2c/make_arena_micro.sh" \
    "$run_root/baseline-checkout" "$run_root/source/arena_micro.bfc"
cp "$repo_root/scripts/selfhost-2c/fixtures/ir-overhead.bfc" \
    "$run_root/source/ir-overhead.bfc"
cp "$repo_root/scripts/selfhost-2c/fixtures/phase-portal-overhead.bfc" \
    "$run_root/source/phase-portal-overhead.bfc"

mkdir -p "$run_root/artifacts/arena"
"$baseline_bfc" --run-ir "$run_root/source/stage2-cir-compiler.bfc" \
    < "$run_root/source/arena_micro.bfc" \
    > "$run_root/artifacts/arena/arena_micro.cir" \
    2> "$run_root/artifacts/arena/arena-micro-cir-generation.log"

for route in source cir; do
    for variant in baseline candidate; do
        if [[ $variant == baseline ]]; then
            compiler=$baseline_bfc
        else
            compiler=$candidate_bfc
        fi
        options=(--unlimited-tape --profile-granularity continuation)
        if [[ $variant == candidate ]]; then
            options+=(--enable-2c)
        fi
        if [[ $route == source ]]; then
            "$compiler" "${options[@]}" --profile-map-output \
                "$run_root/artifacts/arena/source/$variant.bfmap.json" \
                "$run_root/source/arena_micro.bfc" \
                > "$run_root/artifacts/arena/source/$variant.bf"
        else
            "$compiler" --cir-input "$run_root/artifacts/arena/arena_micro.cir" \
                "${options[@]}" --profile-map-output \
                "$run_root/artifacts/arena/cir/$variant.bfmap.json" \
                > "$run_root/artifacts/arena/cir/$variant.bf"
        fi
    done
done

sha256sum "$run_root"/source/* "$run_root"/artifacts/cir/stage2-compiler.cir \
    > "$run_root/source-sha256.txt"
