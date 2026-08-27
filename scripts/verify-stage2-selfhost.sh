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
    -p bf-compiler --bin bfc -- "$test_source" >"$test_bf"
cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$test_bf" \
    >"$work_dir/stage2-tests.actual"
printf 'ok\n' >"$work_dir/stage2-tests.expected"
cmp "$work_dir/stage2-tests.expected" "$work_dir/stage2-tests.actual"

cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-compiler --bin bfc -- \
    "$compiler_source" >"$compiler_bf"

printf 'void main(){cell value;if(value!=1);}' | \
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- "$compiler_bf" \
        >"$work_dir/invalid-stage3.actual"
if ! LC_ALL=C grep -q 'BFC_STAGE3_ERROR' "$work_dir/invalid-stage3.actual"; then
    echo "stage-3 compiler accepted a nonzero != operand" >&2
    exit 1
fi

compile_example() {
    local name=$1
    local generated="$work_dir/$name.bf"
    cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
        -p bf-interpreter --bin bf-interpreter -- "$compiler_bf" \
        <"$repo_dir/selfhost/stage2/examples/$name.bfc" >"$generated"

    if LC_ALL=C grep -q 'BFC_STAGE3_ERROR' "$generated"; then
        echo "stage-3 compiler rejected $name.bfc" >&2
        return 1
    fi
    if LC_ALL=C grep -q '[^][<>+.,-]' "$generated"; then
        echo "stage-3 compiler emitted a non-Brainfuck byte for $name.bfc" >&2
        return 1
    fi
}

compile_example hello
compile_example arithmetic
compile_example scopes
compile_example hexadecimal
compile_example control_flow
compile_example stage3_logic
compile_example condition_input

cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/hello.bf" \
    >"$work_dir/hello.actual"
printf 'A!\n' >"$work_dir/hello.expected"
cmp "$work_dir/hello.expected" "$work_dir/hello.actual"

printf '\012\004' | cargo run --quiet --release \
    --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/arithmetic.bf" \
    >"$work_dir/arithmetic.actual"
printf '\015\014\374' >"$work_dir/arithmetic.expected"
cmp "$work_dir/arithmetic.expected" "$work_dir/arithmetic.actual"

cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/scopes.bf" \
    >"$work_dir/scopes.actual"
printf 'BAC\000' >"$work_dir/scopes.expected"
cmp "$work_dir/scopes.expected" "$work_dir/scopes.actual"

cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/hexadecimal.bf" \
    >"$work_dir/hexadecimal.actual"
printf 'Aa\001!' >"$work_dir/hexadecimal.expected"
cmp "$work_dir/hexadecimal.expected" "$work_dir/hexadecimal.actual"

printf '\003' | cargo run --quiet --release \
    --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/control_flow.bf" \
    >"$work_dir/control-flow-nonzero.actual"
printf '\003\002\001' >"$work_dir/control-flow-nonzero.expected"
cmp "$work_dir/control-flow-nonzero.expected" "$work_dir/control-flow-nonzero.actual"

printf '\000' | cargo run --quiet --release \
    --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/control_flow.bf" \
    >"$work_dir/control-flow-zero.actual"
printf 'Z' >"$work_dir/control-flow-zero.expected"
cmp "$work_dir/control-flow-zero.expected" "$work_dir/control-flow-zero.actual"

cargo run --quiet --release --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/stage3_logic.bf" \
    >"$work_dir/stage3-logic.actual"
printf '\001\000\000\001TE21' >"$work_dir/stage3-logic.expected"
cmp "$work_dir/stage3-logic.expected" "$work_dir/stage3-logic.actual"

printf '\002\001\000' | cargo run --quiet --release \
    --manifest-path "$repo_dir/Cargo.toml" \
    -p bf-interpreter --bin bf-interpreter -- "$work_dir/condition_input.bf" \
    >"$work_dir/condition-input.actual"
printf 'xx' >"$work_dir/condition-input.expected"
cmp "$work_dir/condition-input.expected" "$work_dir/condition-input.actual"

echo "stage-3 self-host verification passed"
