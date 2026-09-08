#!/usr/bin/env bash
set -euo pipefail

repo_root=${1:?checkout containing scripts/concat-stage2-compiler.sh}
output=${2:?output source path}

"$repo_root/scripts/concat-stage2-compiler.sh" main \
    | sed '/^void main() {$/,$d' > "$output"
cat >> "$output" <<'EOF'

void emit_arena_case(NodeId position, cell amount) {
    NodeId result = arena_advance(position, amount);
    output(result.bank);
    output(result.page);
    output(result.slot);
}

void main() {
    NodeId zero;
    NodeId page_end;
    NodeId bank_page_end;
    cell repeat;
    page_end.slot = 0xff;
    bank_page_end.page = ARENA_LAST_PAGE;
    bank_page_end.slot = 0xff;
    while (repeat < 32) {
        emit_arena_case(zero, 0);
        emit_arena_case(zero, 1);
        emit_arena_case(zero, 0xff);
        emit_arena_case(page_end, 1);
        emit_arena_case(bank_page_end, 1);
        emit_arena_case(bank_page_end, 0xff);
        repeat += 1;
    }
}
EOF
