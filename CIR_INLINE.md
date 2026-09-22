# CIR graph inline migration

2026-09-23。allocation 前の CIR graph に clone／splice を実装した。
現在は `continuation_inline::inline_selected` の内部明示指定で検証する。
source の既定経路は空の対象リストを渡し、既存 HIR inline を維持している。
新しい source 構文は追加しない。B1 region emission も引き続き内部オプション。

## 実装と invariant

- 全 reachable 関数を virtual slot／aggregate ID の CIR に lowering する。
- 各 activation の `AbiValue` と `Outbox` を private な virtual storage に正規化する。
  graph 上の各 Call が論理 result destination を持ち、clone した内側の Call は
  clone 元 activation の destination を fresh な storage へ remap する。
- continuation ID、function ownership、scalar slot、aggregate ID、通常 edge と resume edge を
  call site ごとに remap する。global ID と内側の callee reference は保持する。
- callee の全 storage を **inline 呼び出しのたびに** zero 初期化し、評価済み引数を順にコピーする。
  aggregate subrange と aggregate element に置いた scalar parameter の alias を保存する。
- `Return` は元 Call の論理 result を更新する local 命令と resume への Goto にする。
  void／aggregate return の scalar result はゼロ。aggregate は返す範囲だけを書き、
  result inbox の残りは保存する。`Abort` は program exit のまま残す。
- 最終的に残った実 Call にだけ、物理 ABI result を論理 storage へ配送する resume block を生成する。
  物理 outbox の容量は残存 Call から再計算する。main から到達しない関数と block は除去する。
- graph cleanup／local reconstruction → frame fusion → frame allocation → 保守的 cleanup の後に
  backend region plan を作る。B1 は配送 block と通常 resume 間の soft edge も直接実行する。
- 直接／相互再帰の SCC は inline しない。continuation ID の上限に達する候補も保持する。
- callee 命令の source span を保持し、合成した初期化・引数・result コピーは call site に対応付ける。
  frame fusion は block 全体へ先頭の span を付け直す処理をやめ、元命令の index から span を引き継ぐ。

`continuation_operands` に operand remap を集約し、allocator と inliner で共用する。
read／write／clobber 分類の共用化は引き続き後続作業。

検証中に、既存 Call backend が aggregate parameter と重なる scalar parameter を
加算してしまうケースも修正した。callee frame がゼロという前提は最初の書き込みにだけ使い、
後続 parameter が同じ payload cell を書く場合には clear してからコピーする。

## 差分実行と測定

```sh
cargo test -p bf-compiler inline_probe -- --nocapture --test-threads=1
cargo test -p bf-compiler explicit_inline_retains_call_when_no_clone_ids_fit
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`abi_codegen/inline_probe.rs` は HIR inline をバイパスし、同一 unallocated CIR の inline 前後を
CIR VM と B0／B1 の生成 BF で実行する。期待出力、入力消費、残存 hard operation の実行数、
profile 有無の BF／raw／RLE カウンタを比較する。下表は **B1 を共通に使用した inline 前→後**。
訪問数・BF bytes・raw／RLE は全 program。frame は対象 caller の値。

| Fixture / 明示対象 | 実 Call | 実 BF 訪問 | caller frame chunks | BF bytes | raw 命令 | RLE 命令 |
|---|---:|---:|---:|---:|---:|---:|
| early return＋内側 Call、2反復 / work | 4→2 | 9→5 | 2→2 | 7,223→9,063 | 13,270→9,864 | 2,918→2,350 |
| 2 caller / f のみ | 6→4 | 13→9 | 各2→2 | 9,222→11,841 | 43,166→42,081 | 6,632→7,120 |
| 2 caller / leaf, f, a, b | 6→0 | 13→1 | 2→2 | 9,222→5,768 | 43,166→24,567 | 6,632→4,293 |
| nested aggregate＋global＋portal / work | 2→1 | 6→4 | 7→13 | 500,455→506,675 | 74,324→108,708 | 6,133→7,000 |
| alias parameter＋portal、index=1 / callee | 1→0 | 4→2 | 4→6 | 620,498→619,680 | 58,311→64,217 | 4,891→3,987 |
| outer outbox tail 保存 / outer | 3→2 | 7→5 | 2→7 | 4,477→6,547 | 81,642→163,723 | 7,195→9,767 |
| 繰り返し zero 初期化、3反復 / tick | 3→0 | 7→1 | 2→2 | 2,024→1,103 | 4,874→1,427 | 1,154→229 |

dispatcher 削減と raw／RLE の改善は別々に評価する。明示指定では増大ケースも許可し、
correctness と削減能力を検証している。現状は使わない論理 inbox も一律に確保しており、
自動選択へ進む前にこの storage と不要な初期化の削減、実 allocation に基づく frame cost を扱う。

テストは0／1／複数反復、nested Call、全 scalar return path、複数 caller、複数対象の inline、
短絡評価、RHS→index→store、aggregate 値渡し・subrange・部分 result 更新、alias parameter、
zero-sized aggregate、portal、Abort、直接／相互再帰、source span、ID 65,535 と ID 枯渇を含む。
全339テスト成功（benchmark export 1件は ignored）、Clippy 警告なし。

## 計画の残作業

1. 使用しない論理 result と初期化を削減し、stack frame／global navigation cost を用いた
   自動 inline 選択を実装する。dispatcher 削減能力は明示指定のテストで維持する。
2. HIR cheap inline と CIR inline を同じ fixture で比較し、correctness・frame・dispatch・raw／RLE を確認する。
   その後 HIR の汎用 inline を撤去する。高レベル専用処理を残すなら独立した効果の測定を添える。
3. read／write／clobber と remap の内部 API を整理し、新 hard operation が pattern matcher を必要としない構成にする。
4. B1 と CIR inline の既定経路を確定し、source／公開 CIR API／CLI／selfhost への影響を検証する。
   frame cost・scratch・展開上限による fallback を記録し、全 regression matrix を再確認する。

この migration は未完了であり、上記までを継続する。
