#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
entry=${1:-main}

if [[ "$entry" != main && "$entry" != compressed && "$entry" != cir && "$entry" != test ]]; then
    echo "usage: $0 [main|compressed|cir|test]" >&2
    exit 2
fi

# ファイル境界で字句が連結しないよう、各ソースの後ろに改行を補う。
for source in "$repo_dir"/selfhost/stage2/compiler/[0-9][0-9]_*.bfc; do
    if [[ "$entry" == main || "$entry" == compressed || "$entry" == cir ]]; then
        case "${source##*/}" in
            00_legacy_stage4.bfc|03_legacy_symbols.bfc|04_legacy_codegen.bfc|05_parser.bfc)
                continue
                ;;
        esac
        if [[ "$entry" != cir && "${source##*/}" == 11_cir_serialization.bfc ]]; then
            continue
        fi
    fi
    cat "$source"
    printf '\n'
done

if [[ "$entry" == main || "$entry" == compressed || "$entry" == cir ]]; then
    if [[ "$entry" == cir ]]; then
        cat "$repo_dir/selfhost/stage2/compiler/cir_main.bfc"
    elif [[ "$entry" == compressed ]]; then
        cat "$repo_dir/selfhost/stage2/compiler/compressed_main.bfc"
    else
        cat "$repo_dir/selfhost/stage2/compiler/main.bfc"
    fi
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
