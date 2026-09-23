# CIR soft boundary の region emission

2026-09-22。案 B の prototype、soft cycle 対応、yielding control の通常 CFG 化を実装した。
B0 は既存の CFG optimizer と
continuation ごとの ABI emission、B1 は同じ semantic CIR を参照して soft edge を
直接実行する非公開の backend 出力計画である。

2026-09-23、公開 API と CLI の既定を B1 に切り替えた。
`AbiCodegenOptions::region_emission = false` または `bfc --disable-region-emission` で
B0 を比較できる。出力計画は非公開型のままで、selfhost wire format と ABI field layout は変更しない。
B1 の追加 scratch により frame／main tape capacity に収まらなくなる場合は、B0 の layout と emission
へ戻す。容量内では frame や BF size が増えても soft dispatcher 削減を優先する。
通常 CFG 化に伴い、公開 `Terminator::BranchWithBodies` と VM の対応する統計種別、
viewer の読み取り処理は削除した。旧 variant を使う Rust API 利用側は arm を通常 block へ
移し、旧 variant を含む CIR JSON は再生成する。互換 variant は残さない。

## 実装範囲

`Terminator` に内部用の boundary 分類、通常／resume edge の列挙、successor remap、
callee reference を集約した。既存 optimizer と local reconstruction も edge API を使う。
operand remap は `continuation_operands`、read/write/clobber は `continuation_effects` に集約し、
allocation／CIR inline／virtual cleanup で共用する。

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
Call 引数として再利用できる。

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
cargo clippy --workspace --all-targets -- -D warnings
(cd tools/cir-viewer && npm run build)
```

測定 fixture は `region_probe.rs` に固定した。最初の2ケースは source frontend の
既定 CFG optimizer を通し、CIR inline を明示的に無効化して backend の効果を単独で測る。
初版では conditional early return により当時の HIR inline を防いでいた。
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
B1 はこの理由で候補を棄却しない。この表は初版の記録であり、frame と navigation の費用を別に報告する。

## Soft cycle 対応の追加測定

上の2-call／both-arm の削減結果は維持している。さらに、非 yield 経路で循環できる source を
固定し、対象関数の実 BF 訪問が **entry 1回＋実行した hard operation の resume 回数** に
一致することを検証した。非 yield 経路の周回数は dispatcher 訪問を増やさない。

| Fixture | semantic blocks／CIR 実行数 | 対象 BF 訪問 B0→B1 | 全 program emitted entries B0→B1 | BF bytes B0→B1 | BF 実行命令 B0→B1 |
|---|---:|---:|---:|---:|---:|
| 4反復のうち2回だけ Call | 6／14 | **14→3** | 11→5 | 4,439→6,044 | 15,250→10,620 |
| nested loop の内側で2回 Call | 8／21 | **21→3** | 13→5 | 5,127→7,884 | 17,889→10,473 |
| 3反復のうち2回 aggregate store、後続で2回 load | 8／13 | **13→5** | 10→6 | 1,099,731→1,105,446 | 85,304→78,105 |

| Fixture | slots／aggregate regions／outbox cells | backend scratch B0→B1 | frame chunks B0→B1 | 全 BF 訪問 B0→B1 | 複製 block／instruction | B1 selector 命令 |
|---|---:|---:|---:|---:|---:|---:|
| optional Call | 2／0／0 | 0→8 | 2→2 | 20→7 | 6／8 | 697 |
| nested optional Call | 3／0／0 | 0→13 | 2→2 | 27→7 | 13／21 | 613 |
| aggregate portal | 5／2／0 | 2→10 | 7→7 | 33→25 | 6／27 | 464 |

Call／Return 回数は前2ケースが3／3、portal ケースが1／1で、両方式で一致する。
portal ケースの accessor／resume／router の実 BF 訪問は **18→18**。
native loop の制御命令は selector cost には含めず、全 BF 命令数へ含める。

frame とコードサイズの増加は引き続き計測し、削減能力を保った上で後続の cost model で扱う。
現時点では、frame chunks／BF bytes の増加だけを理由に region を取り消す判定は追加していない。

## Yielding control の通常 CFG 化

`yielding_call_split`、Call 1個の loop 専用 lowering、片 arm を terminator 内に埋める
lowering を撤去した。yield する if／while は通常の `Branch`／`Goto`／hard terminal に
lowering し、frame-only な if／while の `FrameInstruction` 化は維持する。
`BranchWithBodies` の validator、allocator、VM、backend、region planner、viewer の
専用処理も削除した。allocator は arm 内を一括して保守的に live と扱う必要がなくなる。

次表は同一 fixture の **通常 CFG 化前（`3e22f9e`）→後** の B1 測定であり、B0→B1 ではない。
専用 lowering 撤去後も、entry＋hard resume だけの訪問数を維持することをテストで固定した。

| Fixture | 対象 B1 訪問 | scalar slots | B1 frame chunks | 全 program B1 bytes | 全 program B1 実行命令 |
|---|---:|---:|---:|---:|---:|
| single-call loop、n=2 | 3→3 | 3→2 | 2→2 | 5,556→5,560 | 7,694→7,723 |
| if の local arm | 1→1 | 2→2 | 2→2 | 4,417→4,405 | 1,862→1,859 |
| if の Call arm | 2→2 | 2→2 | 2→2 | 4,417→4,405 | 3,302→3,297 |
| 反転条件の loop | 2→2 | 3→2 | 2→2 | 5,553→5,563 | 4,324→4,348 |
| nested prefix／suffix | 3→3 | 6→3 | 2→2 | 5,883→5,805 | 7,819→7,776 |
| aggregate local arm | 1→1 | 2→2 | 4→4 | 7,408→7,408 | 3,925→3,925 |
| nested optional Call | 3→3 | 4→3 | **3→2** | 10,357→7,884 | 13,431→10,473 |

B0 では、片 arm が local な if の local 経路が2→3訪問、optional Call loop が12→14訪問、
nested optional Call loop が18→21訪問になる。B1 はこれらの soft edge を直接実行するため
訪問数が増えない。B1 の既定有効化は別途行い、この移行では dispatcher 削減能力を優先する。
上の soft cycle 表は通常 CFG 化後の値へ更新済み。

## RLE 実行命令数

2026-09-23、通常 CFG 化済みの `d654320` を基準に RLE の測定も追加した。
以後の `region_probe` は raw BF／RLE の両カウンタと selector の内訳を出力する。
RLE は隣接する同一の `+`／`-`／`<`／`>` を1命令にまとめた場合の動的実行数であり、
ファイルの圧縮 byte 数や interpreter の native optimization 実行数ではない。
profile 有無で raw／RLE の総数が一致することも検証する。

| Fixture | 全 program RLE B0→B1 | B1 selector RLE |
|---|---:|---:|
| 2-call loop | 4,548→3,975 | 172 |
| both-arm if | 1,876→1,505 | 69 |
| global/frame growth | 51,850→60,033 | 23 |
| optional Call | 4,485→3,211 | 126 |
| nested optional Call | 5,503→3,253 | 126 |
| aggregate portal | 12,046→9,799 | 80 |
| single-call loop | 2,821→2,398 | 126 |
| local arm | 732→453 | 23 |
| Call arm | 1,227→991 | 68 |
| inverted loop | 1,665→1,368 | 80 |
| nested prefix／suffix | 2,870→2,447 | 126 |
| aggregate local arm | 922→644 | 23 |

## 検証方法と制限

region emission の16テストでは、未融合 CIR VM を意味の基準にして、B0／B1 の生成 BF の出力、入力消費、
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

CIR inline と virtual inbox／outbox、自動 frame cost 判定、HIR inline の撤去は
[CIR_INLINE.md](CIR_INLINE.md) に記録する。展開上限を超える graph は引き続き部分的に dispatcher へ戻す。
任意の大きな CFG から dispatcher を完全に除去することは、この実装の保証に含めない。

検証時の `cargo test --workspace`（329成功、1件は benchmark export 用のため ignored）、
format／diff check、`cargo clippy --workspace --all-targets -- -D warnings`、
CIR viewer の production build は成功。
既存の5警告も修正した。aggregate clear の重複した長さ引数を除去し、
profile site lookup と lowering の分岐、HIR inline の Option／cost 走査を整理している。

## Allocation 前の program pipeline

2026-09-23。`lower_hir_unallocated` で全 reachable 関数を virtual storage のまま生成し、
`continuation_pipeline::finish` で CFG cleanup／local reconstruction → 関数ごとの frame fusion
→ frame allocation → 保守的な empty Goto cleanup の順に処理するよう変更した。
公開 CIR optimization API は allocated CIR に対しても従来どおり使用できる。
本段階で上記 fixture の dispatcher 訪問・frame・raw／RLE 命令数はすべて維持した。
この移動時点では全329テストと Clippy を確認済み。その後の CIR inline と virtual result は
[CIR_INLINE.md](CIR_INLINE.md) に記録する。

## 既定化と互換性

- source、公開 CIR、flat selfhost CIR のいずれも B1 を使用する。profile 有無・通常／圧縮出力で同じ BF を生成する。
- source の自動 CIR inline と B1 は独立して無効化できる。B0／B1 の acceptance テストでは
  inline を無効化し、2-call loop の8→5訪問、both-arm if の4→2訪問を引き続き直接 BF で検証する。
- `AbiCodegenOptions` の Rust struct literal は `region_emission` または `..Default::default()` を指定する。
- scratch が frame 上限を1 chunk超える入力で、既定 backend の BF が B0 と一致し、正常終了することを検証する。
- 旧 HIR 専用の block 数や関数残存数に依存したテストは、目的に応じて明示的に inline を無効化するか、
  新しい Call 除去と意味の一致を検証する形に更新した。

2026-09-23 の既定化時点の再測定（CIR inline は無効、同一 CIR の B0→B1）。
訪問数は対象関数、raw／RLE と BF bytes は全 program。

| Fixture | 実 BF 訪問 | frame chunks | BF bytes | raw | RLE |
|---|---:|---:|---:|---:|---:|
| 2-call loop、n=2 | 8→5 | 2→2 | 4,668→6,019 | 14,251→11,831 | 4,428→3,855 |
| both-arm if | 4→2 | 2→2 | 4,318→5,144 | 5,709→4,294 | 1,831→1,459 |
| optional Call loop | 14→3 | 2→2 | 4,281→5,884 | 14,522→9,892 | 4,383→3,109 |
| nested optional Call | 21→3 | 2→2 | 4,969→7,724 | 17,371→9,955 | 5,429→3,179 |
| soft loop＋aggregate portal | 13→5 | 7→7 | 1,099,395→1,105,110 | 84,972→77,773 | 12,023→9,776 |
| global access＋frame 増加 | 2→1 | 2→3 | 5,565→7,344 | 394,009→528,944 | 51,850→60,033 |

portal の内部訪問は18→18を保つ。最後の例は dispatcher 削減と実行コスト改善が一致しないケースで、
B1 の selector／scratch と global navigation の調整課題として残す。
