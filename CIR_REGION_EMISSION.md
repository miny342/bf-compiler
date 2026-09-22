# CIR soft boundary の region emission prototype

2026-09-22。案 B の prototype と soft cycle 対応を実装した。B0 は既存の CFG optimizer と
continuation ごとの ABI emission、B1 は同じ semantic CIR を参照して soft edge を
直接実行する非公開の backend 出力計画である。

公開 API と CLI の既定動作は B0。B1 は
`abi_codegen::lower_continuations_annotated_with_options` の内部引数
`region_emission` で有効化し、`abi_codegen/region_probe.rs` から比較する。
公開 `AbiCodegenOptions`、CIR JSON、selfhost wire format、ABI field layout は変更していない。

## 実装範囲

`Terminator` に内部用の boundary 分類、通常／resume edge の列挙、successor remap、
callee reference を集約した。既存 optimizer と local reconstruction も edge API を使う。
read/write/clobber と operand remap の統合は、allocation／CIR inline の後続作業に残す。

`continuation_regions.rs` は function entry とすべての hard resume target を再入 root とする。
通常 edge のみをたどり、hard terminal または exit で止める。共有 successor の body は
複製可能だが、同じ terminal は領域内で一度だけ出力する。semantic block 自体は変更しない。
通常 edge からも到達する resume target は直接実行でき、その dispatcher entry も保持する。

初版は soft cycle とその predecessor を従来 emission に戻していた。続く実装では、
soft path の展開中に同じ祖先 block へ戻ったら backedge として記録し、その祖先を
native BF loop の header にする。通常の branch と同じ graph traversal で処理し、
Call／portal ごとの loop pattern は追加しない。既存 local reconstruction が構造化した
local loop も `FrameInstruction::Loop` としてそのまま実行する。

各 loop の先頭で専用 restart flag を消費する。backedge は対象 header の flag だけを立てる。
内側の loop から外側の header へ戻る場合、内側の flag はゼロなのでそこを抜けてから外側を再実行する。
hard terminal に達した経路はすべての loop flag をゼロに保ち、frame-relative な loop と branch
を閉じてから既存 terminal gate へ進む。resume が loop の途中にあっても、その resume target を
root として同じ計画を作れる。複数入口の cycle も path の複製で処理できる。

これは有限の CFG 展開と native loop の組み合わせであり、soft block ごとの PC／dispatcher は
追加しない。graph 全体の dominator tree を作る必要もない。閉じた soft SCC に到達して
hard terminal が一つもない領域は終了しない BF loop になり、terminal selector を出力しない。

上限は領域ごとに terminal 255個、展開 node（backedge marker を含む）4,096個、
backedge marker を除く path depth 128 block。
上限を超える root は従来 emission に戻し、successor を独立した root として検討する。
深い chain と共有 tail による指数的な展開も有限に抑える。追加した出力計画は semantic IR
ではなく、block ID を参照する非公開の tree である。

## Context 移動の安全条件

local region は frame instruction と terminal selector の書き込みだけを行う。
分岐には専用の2個の frame temporary を使う。元の condition は分岐時に消費し、
successor 実行後に再度 clear しない。これにより allocated slot を successor の値や
Call 引数として再利用できる。`BranchWithBodies` の既存 arm cleanup は successor の前に終える。

すべての frame-relative な分岐を閉じた後、専用 frame selector を ABI `PcLow` へ移す。
一度限りの countdown gate が terminal を選び、`PcLow` と `Branch` を消費してから
既存 Call／Return／portal／exit emitter を呼ぶ。移動後には共通 ABI offset だけを参照する。

- Call と直接 portal は移動先の gate cell をゼロ初期化する。
- Return は、Call 前に gate を消費済みの caller context へ戻る。
- Global portal の staging と program exit は現在のゼロ gate を保持する。
- terminal 後に旧 frame の condition、temporary、selector を参照しない。

selector 用に1 cell、region 分岐の深さに応じて2 cellずつ、native loop の深さに応じて
1 cellずつの scratch が必要になる。
既存 local branch／portal scratch と合わせて allocation 後の物理 layout に含める。
仮想 slot や aggregate region の割当自体は変更しない。

soft-only entry は dispatcher 出力から除き、残った entry を private PC encoding で
compact にする。semantic continuation ID と portal resume protocol はそのまま保持する。
複製 body と遅延 terminal の profile site は元 block の source span を引き継ぐ。

## 再現と実測

```sh
cargo test -p bf-compiler region_probe -- --nocapture
cargo test --workspace
cargo fmt --all -- --check
```

測定 fixture は `region_probe.rs` に固定した。最初の2ケースは source frontend の
既定 optimizer を通し、conditional early return により callee の HIR inline を防いでいる。
BF bytes は全 program の最適化済み通常 BF、BF 命令数は全 program の実行命令数。
過去の調査用 fixture の BF bytes と直接比較する値ではない。

| 本 prototype の fixture | 対象の semantic blocks | 対象の CIR 実行数 | 全 program の emitted entries B0→B1 | 対象の実 BF dispatcher 訪問 B0→B1 | BF bytes B0→B1 | BF 実行命令 B0→B1 |
|---|---:|---:|---:|---:|---:|---:|
| 2-call loop、n=2 | 5 | 8 | 10→6 | **8→5** | 4,826→6,179 | 15,077→12,657 |
| both-arm yielding if、true arm | 6 | 4 | 11→6 | **4→2** | 4,788→5,696 | 6,015→4,640 |
| 16-slot frame＋global Copy 4回 | 3 | 2 | 5→3 | 2→1 | 5,565→7,344 | 394,009→528,944 |

| Fixture | 対象の scalar slots | aggregate regions／outbox cells | backend scratch B0→B1 | frame chunks B0→B1 | 全 program の実 BF訪問 B0→B1 | Call／Return（両方式同じ） | 複製 block／frame instruction | B1 selector 実行命令 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 2-call loop | 2 | 0／0 | 0→3 | 2→2 | 18→11 | 5／5 | 2／3 | 1,117 |
| both-arm if | 2 | 0／0 | 0→3 | 2→2 | 8→5 | 2／2 | 1／2 | 446 |
| global/frame growth | 16 | 0／0 | 0→3 | 2→3 | 4→3 | 1／1 | 0／0 | 159 |

最初の2ケースは dispatcher 削減目標を満たし、frame chunks は増えていない。
最後の手動 CIR fixture は frame 境界の影響を分離するために16 scalar slotsを固定している。
scratch による1 chunk増加が global navigation を長くし、訪問数が減っても BF 実行命令は増えた。
B1 はこの理由で候補を棄却しない。現段階では既定有効化を行わず、この費用を別に報告する。

## Soft cycle 対応の追加測定

上の2-call／both-arm の削減結果は維持している。さらに、非 yield 経路で循環できる source を
固定し、対象関数の実 BF 訪問が **entry 1回＋実行した hard operation の resume 回数** に
一致することを検証した。非 yield 経路の周回数は dispatcher 訪問を増やさない。

| Fixture | semantic blocks／CIR 実行数 | 対象 BF 訪問 B0→B1 | 全 program emitted entries B0→B1 | BF bytes B0→B1 | BF 実行命令 B0→B1 |
|---|---:|---:|---:|---:|---:|
| 4反復のうち2回だけ Call | 5／12 | **12→3** | 10→5 | 4,379→6,087 | 14,310→10,678 |
| nested loop の内側で2回 Call | 7／18 | **18→3** | 12→5 | 5,066→10,357 | 16,598→13,431 |
| 3反復のうち2回 aggregate store、後続で2回 load | 7／12 | **12→5** | 9→6 | 1,099,590→1,105,460 | 84,497→78,104 |

| Fixture | slots／aggregate regions／outbox cells | backend scratch B0→B1 | frame chunks B0→B1 | 全 BF 訪問 B0→B1 | 複製 block／instruction | B1 selector 命令 |
|---|---:|---:|---:|---:|---:|---:|
| optional Call | 3／0／0 | 1→8 | 2→2 | 18→7 | 5／8 | 683 |
| nested optional Call | 4／0／0 | 1→13 | 2→3 | 24→7 | 11／21 | 857 |
| aggregate portal | 6／2／0 | 3→10 | 7→7 | 32→25 | 5／27 | 450 |

Call／Return 回数は前2ケースが3／3、portal ケースが1／1で、両方式で一致する。
portal ケースの accessor／resume／router の実 BF 訪問は **18→18**。
native loop の制御命令は selector cost には含めず、全 BF 命令数へ含める。

frame とコードサイズの増加は引き続き計測し、削減能力を保った上で後続の cost model で扱う。
現時点では、frame chunks／BF bytes の増加だけを理由に region を取り消す判定は追加していない。

## 検証方法と制限

追加15テストでは、未融合 CIR VM を意味の基準にして、B0／B1 の生成 BF の出力、入力消費、
各 Call／Return／portal／Halt／Abort の実行回数を照合する。profile あり／なしで最適化済み
BF 自体も一致することを検証している。

`abi.dispatch.enter.<ID>` は dispatcher の gate bracket だけに付けた profile site であり、
その実際の loop iteration 数を訪問数として計測する。`abi.region.enter.<ID>` で遅延 terminal
の実行を区別する。CIR VM の block 実行数を BF dispatcher 訪問数として代用していない。
`abi.region.select` と `abi.region.enter.*` の命令数を selector cost とする。
portal accessor／resume／router の訪問数も semantic entry とは別に数え、B0／B1 の一致を確認する。

検証ケースは0／1／複数反復、nested if／while、両 arm yield、短絡評価、scalar RHS→index→store、
複数 cell aggregate snapshot・部分更新・16-bit offset、frame／global portal、旧 ArrayLoad／Store、
scalar／aggregate の再帰 return、相互再帰、Abort、condition slot 再利用、共有 resume、source span、
ID 65,535、255／256 terminal、soft SCC、展開上限を含む。加えて32種類の有限歩行 CFG を
各3通りの入力で実行し、self edge・nested backedge・複数入口の cycle を検証する。
terminal のない閉じた SCC も BF 出力可能であることを確認する。

CIR inline、virtual inbox／outbox、allocation の pipeline 移動、展開上限を超える graph の直接実行、
`BranchWithBodies` の削除、専用 lowering の撤去は後続作業。
公開機能として有効化する前に、frame cost と追加 scratch を減らす方式の評価が必要になる。

検証時の `cargo test --workspace`、format／diff check、
`cargo clippy --workspace --all-targets -- -D warnings` は成功。
既存の5警告も修正した。aggregate clear の重複した長さ引数を除去し、
profile site lookup と lowering の分岐、HIR inline の Option／cost 走査を整理している。
