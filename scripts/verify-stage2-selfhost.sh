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
        >"$work_dir/invalid-stage5-lexer.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE5_ERROR' "$work_dir/invalid-stage5-lexer.actual"; then
    echo "stage-5 compiler accepted a single ampersand" >&2
    exit 1
fi

printf 'cell broken(cell value){if(value)return 1;}void main(){}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
        >"$work_dir/invalid-stage5-return.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE5_ERROR' "$work_dir/invalid-stage5-return.actual"; then
    echo "stage-5 compiler accepted a missing scalar return path" >&2
    exit 1
fi

stage5_bf="$work_dir/stage5-functions.bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- --unlimited-tape "$compiler_bf" \
    <"$repo_dir/selfhost/stage2/examples/stage5_functions.bfc" >"$stage5_bf"
if LC_ALL=C grep -q 'BFC_STAGE5_ERROR' "$stage5_bf"; then
    echo "stage-5 compiler rejected stage5_functions.bfc" >&2
    exit 1
fi
if LC_ALL=C grep -q '[^][<>+.,-]' "$stage5_bf"; then
    echo "stage-5 compiler emitted a non-Brainfuck byte" >&2
    exit 1
fi

printf 'AB' | cargo run --quiet --release \
    --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$stage5_bf" \
    >"$work_dir/stage5-functions.actual"
printf '\006\001\001AA\001\000A\000\001B' \
    >"$work_dir/stage5-functions.expected"
cmp "$work_dir/stage5-functions.expected" "$work_dir/stage5-functions.actual"

echo "stage-5 self-host verification passed"
