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
        >"$work_dir/invalid-stage6-lexer.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE6_ERROR' "$work_dir/invalid-stage6-lexer.actual"; then
    echo "stage-6 compiler accepted a single ampersand" >&2
    exit 1
fi

printf 'cell broken(cell value){if(value)return 1;}void main(){}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage6-return.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE6_ERROR' "$work_dir/invalid-stage6-return.actual"; then
    echo "stage-6 compiler accepted a missing scalar return path" >&2
    exit 1
fi

printf 'void main(){cell[4] values;cell index;output(values[index]);}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage6-dynamic-index.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE6_ERROR' "$work_dir/invalid-stage6-dynamic-index.actual"; then
    echo "stage-6 compiler accepted a dynamic array index" >&2
    exit 1
fi

printf 'void main(){cell[4] values;output(values[4]);}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage6-bounds.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE6_ERROR' "$work_dir/invalid-stage6-bounds.actual"; then
    echo "stage-6 compiler accepted an out-of-bounds constant index" >&2
    exit 1
fi

stage6_bf="$work_dir/stage6-arrays.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage6_arrays.bfc" >"$stage6_bf"
if LC_ALL=C grep -q 'BFC_STAGE6_ERROR' "$stage6_bf"; then
    echo "stage-6 compiler rejected stage6_arrays.bfc" >&2
    exit 1
fi
if LC_ALL=C grep -q '[^][<>+.,-]' "$stage6_bf"; then
    echo "stage-6 compiler emitted a non-Brainfuck byte" >&2
    exit 1
fi

printf 'Z' | cargo run --quiet --release \
    --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$stage6_bf" \
    >"$work_dir/stage6-arrays.actual"
printf '\000\010BA\011C\006\000\000Z' \
    >"$work_dir/stage6-arrays.expected"
cmp "$work_dir/stage6-arrays.expected" "$work_dir/stage6-arrays.actual"

echo "stage-6 self-host verification passed"
