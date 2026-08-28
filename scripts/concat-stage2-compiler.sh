#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
entry=${1:-main}

if [[ "$entry" != main && "$entry" != test ]]; then
    echo "usage: $0 [main|test]" >&2
    exit 2
fi

# ファイル境界で字句が連結しないよう、各ソースの後ろに改行を補う。
for source in "$repo_dir"/selfhost/stage2/compiler/[0-9][0-9]_*.bfc; do
    cat "$source"
    printf '\n'
done

if [[ "$entry" == main ]]; then
    cat "$repo_dir/selfhost/stage2/compiler/main.bfc"
    printf '\n'
else
    for source in "$repo_dir"/selfhost/stage2/tests/*_test.bfc; do
        cat "$source"
        printf '\n'
    done
    cat "$repo_dir/selfhost/stage2/tests/test_support.bfc"
    printf '\n'
    cat "$repo_dir/selfhost/stage2/tests/test.bfc"
    printf '\n'
fi
