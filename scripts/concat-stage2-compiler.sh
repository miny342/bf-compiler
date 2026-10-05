#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
entry=main
nibble_transfer=0
entry_selected=0
for argument in "$@"; do
    case "$argument" in
        --enable-nibble-transfer)
            nibble_transfer=1
            ;;
        main|compressed|profile|cir|test)
            if (( entry_selected )); then
                echo "usage: $0 [main|compressed|profile|cir|test] [--enable-nibble-transfer]" >&2
                exit 2
            fi
            entry=$argument
            entry_selected=1
            ;;
        *)
            echo "usage: $0 [main|compressed|profile|cir|test] [--enable-nibble-transfer]" >&2
            exit 2
            ;;
    esac
done

emit_entry() {
    sed "s/^const cell NIBBLE_BF_TRANSFER = 0;$/const cell NIBBLE_BF_TRANSFER = $nibble_transfer;/" "$1"
}

# ファイル境界で字句が連結しないよう、各ソースの後ろに改行を補う。
for source in "$repo_dir"/selfhost/stage2/compiler/[0-9][0-9]_*.bfc; do
    if [[ "$entry" != test ]]; then
        case "${source##*/}" in
            00_legacy_stage4.bfc|03_legacy_symbols.bfc|04_legacy_codegen.bfc|05_parser.bfc)
                continue
                ;;
        esac
        if [[ "$entry" != cir && "${source##*/}" == 11_cir_serialization.bfc ]]; then
            continue
        fi
        if [[ "$entry" == cir ]]; then
            case "${source##*/}" in
                04_codegen.bfc|09_bf_profile.bfc|09_bf_serialization.bfc|10_*.bfc)
                    continue
                    ;;
            esac
        fi
    fi
    cat "$source"
    printf '\n'
done

if [[ "$entry" != test ]]; then
    if [[ "$entry" == cir ]]; then
        emit_entry "$repo_dir/selfhost/stage2/compiler/cir_main.bfc"
    elif [[ "$entry" == compressed ]]; then
        emit_entry "$repo_dir/selfhost/stage2/compiler/compressed_main.bfc"
    elif [[ "$entry" == profile ]]; then
        emit_entry "$repo_dir/selfhost/stage2/compiler/profile_main.bfc"
    else
        emit_entry "$repo_dir/selfhost/stage2/compiler/main.bfc"
    fi
    printf '\n'
else
    for source in "$repo_dir"/selfhost/stage2/tests/*_test.bfc; do
        cat "$source"
        printf '\n'
    done
    cat "$repo_dir/selfhost/stage2/tests/test_support.bfc"
    printf '\n'
    emit_entry "$repo_dir/selfhost/stage2/tests/test.bfc"
    printf '\n'
fi
