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
`continuation_effects` が read／write／clobber、動的部分書き込み、Call result、I/O を分類し、
allocator・frame fusion・virtual cleanup で共用する。

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
correctness と削減能力を検証している。この表は初版（`15c5c51`）の値。後続の storage 削減結果は次節に記録する。
自動選択では、実 allocation と backend scratch を含む frame cost を扱う。

テストは0／1／複数反復、nested Call、全 scalar return path、複数 caller、複数対象の inline、
短絡評価、RHS→index→store、aggregate 値渡し・subrange・部分 result 更新、alias parameter、
zero-sized aggregate、portal、Abort、直接／相互再帰、source span、ID 65,535 と ID 枯渇を含む。
全339テスト成功（benchmark export 1件は ignored）、Clippy 警告なし。

## Virtual storage と初期化の削減

各 activation が明示的に使う result の種類だけを確保する。実 activation の ABI inbox と
論理 result の対応が一意なら、最終 materialization で物理 ABI storage を再利用し、配送コピーを
省く。別の inline activation へ配送する内側 Call がある場合は分離を維持する。
aggregate を物理 outbox へ戻すには、残存 Call の return capacity が論理 inbox 全体をカバーする
ことも必要。したがって、inner return が outer outbox の tail を上書きする問題を再導入しない。

`virtual_cleanup` は CFG と structured branch／loop の liveness を解き、後で読まれない
local write・copy・初期化を削除して未使用 storage を取り除く。aggregate は cell ごとの set に
展開せず interval で扱い、部分更新・動的更新後の未更新 cell を保持する。I/O・global write・
hard operation・loop の実行は削除しない。inline のゼロ初期化も実際に必要な invocation では残す。

次は初版の inline 後→cleanup 後（いずれも B1）の比較。

| Fixture | caller chunks | raw | RLE |
|---|---:|---:|---:|
| yielding work、2反復 | 2→2 | 9,864→9,849 | 2,350→2,303 |
| 2 caller、f のみ | 各2→2 | 42,081→39,572 | 7,120→6,009 |
| aggregate＋global＋portal | 13→11 | 108,708→93,668 | 7,000→6,497 |
| alias parameter、index=1 | 6→6 | 64,217→51,814 | 3,987→3,378 |
| outer outbox tail 保存、outer のみ | 7→7 | 163,723→142,869 | 9,767→9,539 |
| tick、3反復 | 2→2 | 1,427→1,418 | 229→220 |

この段階でも dispatcher 削減は維持する。96種類の destructive operand alias と structured cycle、
aggregate interval の穴、Call／portal をまたぐ部分更新を追加検証した。structured branch の arm が
condition を書き換える場合、VM が終了時のゼロ化を省いていた不一致も修正し、IR の規約・B0・B1 と
一致させた。全343テスト成功、Clippy 警告なし。

## 計画の残作業

1. stack frame／global navigation cost を用いた
   自動 inline 選択を実装する。dispatcher 削減能力は明示指定のテストで維持する。
2. HIR cheap inline と CIR inline を同じ fixture で比較し、correctness・frame・dispatch・raw／RLE を確認する。
   その後 HIR の汎用 inline を撤去する。高レベル専用処理を残すなら独立した効果の測定を添える。
3. B1 と CIR inline の既定経路を確定し、source／公開 CIR API／CLI／selfhost への影響を検証する。
   frame cost・scratch・展開上限による fallback を記録し、全 regression matrix を再確認する。

この migration は未完了であり、上記までを継続する。
