#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT

compiler_bf="$work_dir/stage2-compiler.bf"
compiler_source="$work_dir/stage2-compiler.bfc"
test_bf="$work_dir/stage2-tests.bf"
test_source="$work_dir/stage2-tests.bfc"

"$repo_dir/scripts/concat-stage2-compiler.sh" main >"$compiler_source"
"$repo_dir/scripts/concat-stage2-compiler.sh" test >"$test_source"

cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-compiler --bin bfc -- --unlimited-tape "$test_source" >"$test_bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$test_bf" \
    >"$work_dir/stage2-tests.actual"
printf 'ok\n' >"$work_dir/stage2-tests.expected"
cmp "$work_dir/stage2-tests.expected" "$work_dir/stage2-tests.actual"

cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-compiler --bin bfc -- --unlimited-tape \
    "$compiler_source" >"$compiler_bf"

printf 'void main(){cell value;if(value&1);}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage8-lexer.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$work_dir/invalid-stage8-lexer.actual"; then
    echo "stage-10 compiler accepted a single ampersand" >&2
    exit 1
fi

printf 'cell broken(cell value){if(value)return 1;}void main(){}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage8-return.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$work_dir/invalid-stage8-return.actual"; then
    echo "stage-10 compiler accepted a missing scalar return path" >&2
    exit 1
fi

printf 'void main(){cell[4] values;output(values);}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage8-whole-array.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$work_dir/invalid-stage8-whole-array.actual"; then
    echo "stage-10 compiler accepted a whole array as a scalar" >&2
    exit 1
fi

printf 'void main(){cell[4] values;output(values[4]);}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage8-bounds.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$work_dir/invalid-stage8-bounds.actual"; then
    echo "stage-10 compiler accepted an out-of-bounds constant index" >&2
    exit 1
fi

stage7_bf="$work_dir/stage7-globals.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage7_globals.bfc" >"$stage7_bf"
if LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$stage7_bf"; then
    echo "stage-10 compiler rejected stage7_globals.bfc" >&2
    exit 1
fi
if LC_ALL=C grep -q '[^][<>+.,-]' "$stage7_bf"; then
    echo "stage-10 compiler emitted a non-Brainfuck byte for stage7_globals.bfc" >&2
    exit 1
fi

printf '\002\024\004Z' | cargo run --quiet --release \
    --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$stage7_bf" \
    >"$work_dir/stage7-globals.actual"
printf '\007\000\000\001\015\006\002\012\005\024AZ' \
    >"$work_dir/stage7-globals.expected"
cmp "$work_dir/stage7-globals.expected" "$work_dir/stage7-globals.actual"

printf 'void take(cell[3] value){}void main(){cell[4] value;take(value);}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage8-array-type.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$work_dir/invalid-stage8-array-type.actual"; then
    echo "stage-10 compiler accepted mismatched array argument lengths" >&2
    exit 1
fi

stage8_bf="$work_dir/stage8-aggregates.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage8_aggregates.bfc" >"$stage8_bf"
if LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$stage8_bf" \
    || LC_ALL=C grep -q '[^][<>+.,-]' "$stage8_bf"; then
    echo "stage-10 compiler rejected or corrupted stage8_aggregates.bfc" >&2
    exit 1
fi
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$stage8_bf" \
    >"$work_dir/stage8-aggregates.actual"
printf '\001\002\003\004\001\013\013\001\001\012\013\014\015' \
    >"$work_dir/stage8-aggregates.expected"
cmp "$work_dir/stage8-aggregates.expected" "$work_dir/stage8-aggregates.actual"

snapshot_bf="$work_dir/stage8-snapshots.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage8_snapshots.bfc" >"$snapshot_bf"
if LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$snapshot_bf" \
    || LC_ALL=C grep -q '[^][<>+.,-]' "$snapshot_bf"; then
    echo "stage-10 compiler rejected or corrupted stage8_snapshots.bfc" >&2
    exit 1
fi
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$snapshot_bf" \
    >"$work_dir/stage8-snapshots.actual"
printf '\001\011' >"$work_dir/stage8-snapshots.expected"
cmp "$work_dir/stage8-snapshots.expected" "$work_dir/stage8-snapshots.actual"

printf 'enum Bad{One=1}void main(){}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage9-enum.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$work_dir/invalid-stage9-enum.actual"; then
    echo "stage-10 compiler accepted an enum without a zero variant" >&2
    exit 1
fi

printf 'struct Loop{Loop value;}void main(){}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage9-cycle.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$work_dir/invalid-stage9-cycle.actual"; then
    echo "stage-10 compiler accepted a recursive struct layout" >&2
    exit 1
fi

stage9_types_bf="$work_dir/stage9-types.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage9_types.bfc" >"$stage9_types_bf"
if LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$stage9_types_bf" \
    || LC_ALL=C grep -q '[^][<>+.,-]' "$stage9_types_bf"; then
    echo "stage-10 compiler rejected or corrupted stage9_types.bfc" >&2
    exit 1
fi
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$stage9_types_bf" \
    >"$work_dir/stage9-types.actual"
printf '\001\013\026\026' >"$work_dir/stage9-types.expected"
cmp "$work_dir/stage9-types.expected" "$work_dir/stage9-types.actual"

stage9_projections_bf="$work_dir/stage9-projections.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage9_projections.bfc" \
    >"$stage9_projections_bf"
if LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$stage9_projections_bf" \
    || LC_ALL=C grep -q '[^][<>+.,-]' "$stage9_projections_bf"; then
    echo "stage-10 compiler rejected or corrupted stage9_projections.bfc" >&2
    exit 1
fi
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$stage9_projections_bf" \
    >"$work_dir/stage9-projections.actual"
printf '\001\013\011\013' >"$work_dir/stage9-projections.expected"
cmp "$work_dir/stage9-projections.expected" \
    "$work_dir/stage9-projections.actual"

printf 'const cell A=B;const cell B=A;void main(){}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage10-constant-cycle.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' \
    "$work_dir/invalid-stage10-constant-cycle.actual"; then
    echo "stage-10 compiler accepted a constant cycle" >&2
    exit 1
fi

printf 'void main(){cell[2] other;cell[] value=other;}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage10-inferred-array.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' \
    "$work_dir/invalid-stage10-inferred-array.actual"; then
    echo "stage-10 compiler accepted non-string cell[] inference" >&2
    exit 1
fi

printf 'void main(){cell[2] value="abc";}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage10-string-length.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE10_ERROR' \
    "$work_dir/invalid-stage10-string-length.actual"; then
    echo "stage-10 compiler accepted a mismatched string length" >&2
    exit 1
fi

stage10_bf="$work_dir/stage10-compile-time.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage10_compile_time.bfc" \
    >"$stage10_bf"
if LC_ALL=C grep -q 'BFC_STAGE10_ERROR' "$stage10_bf" \
    || LC_ALL=C grep -q '[^][<>+.,-]' "$stage10_bf"; then
    echo "stage-10 compiler rejected or corrupted stage10_compile_time.bfc" >&2
    exit 1
fi
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$stage10_bf" \
    >"$work_dir/stage10-compile-time.actual"
printf '\003\101\012\000\004\102\042\134\041\002\002\003' \
    >"$work_dir/stage10-compile-time.expected"
cmp "$work_dir/stage10-compile-time.expected" \
    "$work_dir/stage10-compile-time.actual"

echo "stage-10 self-host verification passed"
