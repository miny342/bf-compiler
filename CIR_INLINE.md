# CIR graph inline migration

2026-09-23。allocation 前の CIR graph に clone／splice を実装した。
source の既定経路は frame cost に基づく自動 CIR inline を使用する。HIR の汎用 inliner は撤去した。
`ContinuationOptimizationOptions::inline_functions = false` または `bfc --disable-function-inline` で
比較できる。公開の allocated CIR／selfhost flat CIR に再 allocation や自動 inline は適用しない。
内部の `inline_selected` は削減能力と細かい activation invariant の検証用に残す。
新しい source 構文は追加しない。backend の B1 region emission も既定で有効。

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

## 自動選択と HIR cheap case の比較

`inline_automatic` は callee から caller の順で call site を試し、graph cleanup／allocation と
B1 backend layout を実行して実 frame chunks を比較する。selector、loop gate、portal scratch も含む。
global を直接または残存 Call 経由で使う関数は persistent frame の増加を許可しない。
global-free caller は ABI layout に収まる範囲で増加を許可する。再帰 SCC と ID 枯渇は保持する。
展開作業量は元 CIR の instruction＋block 数の8倍（下限8,192、上限1,000,000）で制限する。
不採用の試行も計上し、BF bytes の増加だけでは棄却しない。

読み取り専用 scalar parameter は評価済み caller frame 値を参照できる。書き換える parameter、
global、aggregate element は snapshot を維持する。allocator は死ぬ scalar Copy の source と
destination の同居を許し、独立して生存する値の interference は保持する。
実 Call がなくなった関数の scalar result は virtual slot のまま allocation し、物理 ABI への
不要な往復を避ける。

同じ7 fixtureを、旧 HIR inline と自動 CIR inline で別々に lowering した結果（共通 B1）。
全て caller frame は2 chunks。CIR は全て実 Call が0、実 dispatcher 訪問が1になった。

| Fixture | HIR→CIR 実 Call | 実 BF 訪問 | BF bytes | raw | RLE |
|---|---:|---:|---:|---:|---:|
| void＋global、2 call site | 0→0 | 1→1 | 1,882→3,657 | 9,735→11,067 | 1,224→1,394 |
| nested void＋global | 0→0 | 1→1 | 1,972→1,876 | 3,097→3,005 | 402→404 |
| pure scalar、2 call site | 0→0 | 1→1 | 492→744 | 626→1,233 | 128→279 |
| side-effecting argument | 1→0 | 3→1 | 1,638→277 | 4,634→250 | 702→117 |
| input を使う return | 0→0 | 1→1 | 270→279 | 215→252 | 79→117 |
| pure scalar in loop | 0→0 | 1→1 | 637→1,835 | 1,948→3,434 | 320→644 |
| nested Call＋early return | 4→0 | 9→1 | 6,419→3,513 | 11,870→3,735 | 2,744→1,165 |

correctness・frame・dispatcher は既存 cheap case を満たす。一方、HIR の式置換で消えていた
定数／引数コピーや source loop と CFG 再構成の差により、RLE は一部増える。dispatcher 削減を
優先し、この差は後段 CIR の最適化課題として記録する。HIR 汎用 inliner の維持理由にはしない。

17個の入力 local を保持する callee で、global の有無による frame guard を検証した。
global ありでは内側 helper のみ inline して外側 Call を保持し、global なしでは両方を inline して
caller frame の増加を許可する。入力消費と値の保持は生成 BF でも一致する。

## 既定経路への移行

HIR の汎用 inliner とその試験 lowering の frame estimator を削除した。cheap case の同一 fixture は
CIR での意味・frame chunks・実 dispatcher 訪問を固定する回帰テストとして残す。上の HIR 比較は
`885eddd` で両経路を実行した記録であり、HIR 実装をテスト専用に残してはいない。

自動選択の試行は caller と callee の graph だけを複製する。残存 Call の signature は保持し、
caller の materialization／virtual cleanup／CFG cleanup／allocation／B1 layout を実行する。
全 program に必要な global portal route scratch も計上する。テストでは全 program の試験 allocation と
同じ caller cost になることを照合する。採用後の caller template も不要 storage を削減し、次の clone で
死んだ領域の初期化が増殖することを避ける。

CLI の artifact identity は `bfc-ir-artifact-v6` とし、`inline_functions` を hash と measurement options
に含める。identity helper／phase config generator／固定 fixture も更新した。関数を phase 境界にする
計測では `--disable-function-inline` を使い、同じオプションで identity と config を再生成する。
古い hash や config は意図的に拒否する。これは計測 identity の変更で、CIR wire schema の変更ではない。

selfhost 全体の差分検証は次で再実行できる。

```sh
cargo test --release -p bf-compiler --test cir_selfhost_migration -- --ignored --nocapture
```

この検証は compiler 自体を CIR VM 上で動かし、inline 有無で出力 CIR bytes・入力消費・終了状態を
照合する。生成された loop／aggregate snapshot／recursion のプログラムを B0 と B1 の BF で実行し、
raw／RLE も記録する。不正 return のエラーも両方で照合する。巨大な selfhost compiler 全体の BF 実行時間を
この測定から推定しない。

## selfhost と最終 regression matrix

同じ selfhost compiler source を source lowering した比較では、inline 無効→有効で
197→161関数、3,926→7,507 semantic block となった。lowering 時間はこの環境で1,322→201,757 ms。
Call は減るが、試行 allocation と複製によるコンパイル時間増加は大きい。これは BF の実行時間ではない。

| selfhost への入力 | compiler の実 Call | compiler の CIR 訪問 | 出力 CIR bytes |
|---|---:|---:|---:|
| scalar loop | 4,444→2,324 | 15,232→10,872 | 89（一致） |
| aggregate snapshot | 15,461→8,143 | 52,777→37,715 | 200（一致） |
| recursion | 7,348→3,833 | 25,201→17,968 | 151（一致） |
| 不正 return | 2,492→1,216 | 8,636→5,941 | error 21（一致） |

入力消費と終了状態も一致。生成物の B0→B1 は、loop が raw 5,512／RLE 555で不変、
snapshot が raw 32,917／RLE 3,513で不変、recursion が raw 24,590→23,025／RLE 3,401→3,166。
compiler 自体の CIR 訪問数を BF dispatcher 訪問数として扱わない。

元の計画の各項目は次の回帰群で確認する。

| 計画のケース | 主な回帰 |
|---|---|
| 1. 1-call loop | `region_probe::source_yield_lowering_preserves_dispatch_minimum` |
| 2. 2-call loop | `region_probe::two_call_loop_reaches_five_real_dispatcher_visits` |
| 3–4. nested if／while、soft cycle | `region_probe` の optional/nested Call と32種類の有限 CFG |
| 5. both-arm yielding if | `region_probe::both_yielding_arms_reach_two_real_dispatcher_visits` |
| 6–7. RHS→index→portal、aggregate snapshot／部分更新 | `region_probe` と `inline_probe` の Call/portal、subrange、outbox tail |
| 8–9. yielding short-circuit | `inline_probe::inline_keeps_short_circuit_evaluation_and_zero_sized_returns` |
| 10. yielding inline、内側 Call 保持 | `inline_probe::yielding_and_early_return_inline_preserves_inner_calls_and_iteration_order` |
| 11. 複数 caller | `inline_probe::multiple_callers_and_nested_inline_have_fresh_storage` |
| 12. scalar return、毎回の初期化 | `inline_probe` の fresh storage／loop invocation／readonly snapshot |
| 13. aggregate 値渡し・結果 | `inline_probe` の nested results／parameter subrange／outbox tail／zero-sized return |
| 14. 直接／相互再帰 | `inline_probe::direct_and_mutual_recursive_sccs_are_never_inlined` と automatic budget test |
| 15. Abort／early return | `inline_probe`、`structured_frame_lowering`、selfhost error case |
| condition 再利用・共有 resume・source span・ID上限 | `region_probe`／`inline_probe`／`explicit_inline_retains_call_when_no_clone_ids_fit` |
| frame／展開上限 | automatic global frame guard／work budget、region capacity fallback／255・256 terminal／depth・node limit |
| 公開API・CLI・wire・profile・圧縮 | `public_api`／`ir_metrics_cli`／`cir_selfhost_migration` |

残る性能調整は、cheap case の定数／コピー簡約、B1 の追加 scratch と selector cost、
大規模入力の試行 allocation／複製量である。dispatcher 削減を保証するテストを維持したまま調整する。

selfhost の全テストソース（`concat-stage2-compiler.sh test`）も既定の CIR inline で lowering し、
CIR VM 上で `ok\n` を確認した。lowering は58,114 ms、287関数・12,659 block、実行は
508,746 CIR訪問・132,390 Call・3,517,301 frame instruction。通常 workspace からは重い
selfhost 2検証を ignored にし、上記 release コマンドで両方を実行する。

最終確認：`cargo test --workspace` は342成功・3 ignored（benchmark export 1件、別途成功した selfhost 2件）。
`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、diff check は成功。
Python の artifact identity helper と Rust CLI の hash も inline 有効／無効の両方で一致した。
