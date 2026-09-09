# Selfhost 最適化実験の引き継ぎ

2026-09-09更新。2026-09-08時点の調査と、その後の実験・レビューに基づく。
現在の進捗と次の作業は冒頭に記載する。後半の調査値は当初baselineの履歴として保持する。
すべての案を一度に実装する計画ではない。各実験の結果を見て次の優先順位を更新する。

## 現在の進捗

計画・作業指示は本ファイル、実測値・評価は`SELFHOST_OPTIMIZATION_EXPERIMENTS_EVALUATION.md`
に集約する。NEXTやFIXなどの別の指示文書は作成しない。

- `bd35604`: 空Goto除去・ID compactionを採用。累積IR遷移計測を追加。
- `4a197e5`: 同一関数のBranch後継inline化（実験2c）を採用。source/CIR共通で適用。
  JSON metrics、terminator分類、source function名、計測切替を追加。
- `5dda9be`: metricsによる入力上書きを防止。評価の単位・採用理由を訂正し、
  評価文書と`scripts/selfhost-2c/`の再現スクリプトをcommit。
- 2cは複数入力でのBF execute短縮を根拠に採用した。
  end-to-end・full selfhost改善は未確認。3入力・source/CIR両経路の10ペア比較は完了。
- IR遷移収集overheadは専用benchmarkで約25.7%。BF側の性能比較には混ぜない。
- arena算術化は試作後に撤回。局所if/whileのFrameInstruction構造化は未実装。
- 実験1のphase別集計・portal要求集計を完了。source/CIRのidentity-bound設定、
  phase/transition/terminal、region/offset/連続性、D=8/D=16開始chunk再訪を
  オプトインJSONへ保存し、3入力の直接IR runner測定まで記録した。
- CIRは関数名を保持しないため、source名を流用せず固定CIRの明示function IDで集計した。
  BF hidden dispatch eventは未実装。実験3〜5も未着手。
- 2026-09-09: `5bdb6de`のレビュー修正を完了。phase設定入力の衝突防止、phase遷移での
  portal隣接切断、実入力・lowering optionsとのartifact identity照合、固定CIR mappingの
  hash検証、chunk再訪率の意味の訂正を実装・再評価した。

## 次の作業（完了）：5bdb6deのレビュー修正

作業セッション`01a08208-197c-7350-a3ab-e8b9c0aae650`、model `gpt-5.6-luna`に依頼する。
基本カウンタ・6件の出力・overhead中央値はレビューで整合を確認済みだった。
以下の4項目を修正し、回帰テストと固定source/CIRの再評価まで完了した。

1. **phase設定の上書き防止。** `--ir-metrics`と`--ir-phase-config`に同じファイルを
   指定すると設定がmetrics JSONで上書きされる。IR実行前の出力先検証に設定入力も含め、
   同一パス・相対/絶対表記・symlinkを拒否する。拒否時の内容保持と正常出力をCLIでテストする。
2. **phase境界でportal隣接を切る。** 現在は直前portal要求のphaseとだけ比較するため、
   Aで要求→Bへ移動（portalなし）→Aへ復帰→要求が同一phase内の隣接として数えられる。
   有効phaseが変わった時点で前要求との隣接を切り、offset差にも反映する。
   call/return・ネスト・unknownを含む回帰例を追加する。同じphaseの再帰についても定義を明記する。
3. **実入力とartifact identityを照合。** 設定IDとCLI IDの一致だけでは不十分。
   実際に読み込んだsource/CIRからidentityを計算して検証する。source複数ファイルは
   順序と境界を含む再現可能な定義にし、stdin CIRも読込byte列で検証する。
   ID対応に影響するloweringオプションも記録・検証する。
   `write_phase_configs.py`の固定CIR function IDを任意の新しいhashへ付け直さない。
   確認済みCIR hashに限定するか、根拠付き対応表を検証して生成し、未知CIRでは安全に拒否する。
   改変source/CIR、古い設定、正しい設定の回帰テストを追加する。
4. **評価の意味を訂正。** 開始chunk再訪率はphase全体の履歴内で既訪問だった割合であり、
   直前要求との近さではない。隣接率と区別して記載する。
   高い再訪率・同一region率だけで安全に一括処理できるとは言えず、call/I/O/alias等の
   境界は未評価である。「portal連続処理の候補を調べる根拠」までに判断を限定する。
   次の最適化をこの修正作業中に実装しない。

レビューの再現fixtureと確認scriptは`tmp/review-5bdb6de.Tp2lua/`にある。
再現でphase.jsonをmetricsへ置き換えているため、そのファイルは設定として再利用しない。
修正に必要なfixtureは追跡対象のテストに追加する。

修正後、`cargo test --workspace`、固定source/CIR×3入力の直接IR runner測定、
新しい計測ON/OFFのoverheadをAB/BA 10ペアで再実行する。
phase合計・portal合計・出力一致、実入力identity、設定IDの根拠を確認する。
巨大BF再生成・長時間selfhost・新規最適化は不要。
生ログは新しい`./tmp`専用directoryに保存し、既存結果は上書きしない。
計画の進捗とEVALUATIONを更新し、訂正前後と制約を明示する。
NEXT/FIX文書は作らない。AGENTS.mdに従いsubagentと`/tmp`は使わない。
この計画追記は依頼元が加えた今回の作業指示として、修正コード・テスト・評価と一緒にcommitしてよい。
その他の既存ユーザー変更は保持し、push/mergeはしない。
完了したらcommit、各指摘への対応、テストと再測定の結果、残課題を同セッションに報告する。
依頼元による監視・返答待ちは不要。

2026-09-09完了。修正commitではこの節自体も作業指示の履歴として保持し、詳細な差分、
再測定値、訂正後の判断、未計測の境界を`SELFHOST_OPTIMIZATION_EXPERIMENTS_EVALUATION.md`
へ追記した。次の最適化はこの修正範囲に含めていない。

## 前回の作業（完了・レビュー修正済み）：実験1のphase・portal集計

新規セッションは`gpt-5.6-luna`を使用する。本節の実装・検証・小規模計測・記録までを
今回の完了範囲とした。次の最適化そのものや長時間full selfhost実行は開始していない。

2026-09-09に、開始commit `98932d2` から専用branchで本節を完了した。計測コード、
回帰テスト、設定fixture、再実行script、3入力のsource/CIR集計、追加計測overheadを
commit対象へまとめ、実測値と制約は`SELFHOST_OPTIMIZATION_EXPERIMENTS_EVALUATION.md`
へ追記した。

### 作業条件

- `AGENTS.md`を確認する。subagentは使わず、全一時ファイル・build・ログは
  リポジトリ内`./tmp`の新しい専用directoryに保存する。`/tmp`は使わない。
- `5dda9be`以降の計画更新commitから専用branchを作り、開始commitを記録する。
  既存ユーザー変更・既存artifact・実行中processを変更・停止しない。
- 2c、arena関数、PC符号配置、portal ABI、frame allocationの動作は変更しない。
  追加計測は明示的なオプトインにし、無効時に集計mapを更新しない。
- source/CIR入力とmetrics出力の衝突防止を維持する。新しい出力先にも同じ検証を適用する。

### 実装する計測

1. IR runnerのphase別continuation・遷移・portal集計を追加する。
   phaseは単なる経過時間ではなく、stage2 compilerの関数entry/returnに対応する
   明示的な設定で切り替える。lexer/parser/semantic/lowering/codegenの実際のentryを調べ、
   利用可能な境界を設定ファイルとして保存する。ネストする場合はstackで元phaseへ戻す。
   entryが同じcontinuationになる小例、再帰、abortにも定義を持たせる。
   分類不能な範囲はunknownとして残し、同じ時間を二重計上しない。
2. 設定はlogical function IDとartifact identityに結び付ける。
   source経路は保存済みfunction名を使える。CIRに名前情報がなければ捏造せず、
   同一固定CIRに対応する明示的なID設定だけを使う。
   対応を確認できないCIR phaseはunknownとし、その制約を報告する。
   名前対応を得るためにCIR形式やBFC frontendを全面変更しない。
3. ArrayLoad/Store、AggregateLoad/Storeの実行時に、要求元continuation/function、
   phase、region、load/store、実行直前のlogical offset、転送cell数を集計する。
   frame領域は再帰activationを区別し、globalとは別のidentityにする。
   aggregate処理がoffsetを書き換える前に要求値を記録する。
4. 同一regionへの連続要求率、offset差histogram、D=8/D=16ごとの開始chunk再訪を集計する。
   「連続」は全portal要求列で隣接する2要求と定義し、phaseをまたぐ比較は分離する。
   chunk指標は開始offsetの指標であり、複数cell転送の全アクセス列を再現したとは扱わない。
   histogram・集約を基本にし、要求ごとの無制限traceは出さない。
5. JSONに全体とphase別の値、phase設定、source/CIR identity、計測オプション、
   指標の定義を保存する。既存JSONの互換性に配慮し、変更時はformat versionを明示する。
   BF hidden portalのdispatch回数やnavigation費用をIR要求数から測定済みと解釈しない。

### 検証と測定

- 小さいfixtureでphase切替・復帰・再帰・abort、global/frame領域、load/store、
  offset境界、複数cell転送、同一region連続要求を確認する。
- phase別合計が全体と一致すること、portal要求合計が既存load/storeカウンタと一致すること、
  計測ON/OFFで出力・通常カウンタが一致することをテストする。
  terminalとphase境界をまたぐ遷移の帰属規則を明記する。
- `cargo test --workspace`を実行する。今回ABIやloweringの意味を変更しない限り、
  巨大BFの生成やstage2 selfhost長時間検証は不要。
- 固定production compiler source/CIRについて、hello、stage5_functions、stage8_aggregatesの
  3入力を直接IR runnerで完走させ、phase・portal上位の表を生成する。
  既存artifactを利用するときはsource/CIR identityを確認し、別artifactのIDを流用しない。
- 代表的な小さい完走benchmarkで、高分解能のIR execute時間を使い、
  追加計測OFF/ONをAB/BA交互10ペアで比較する。反復数は分解能を十分上回るよう固定し、
  既存遷移収集の有無と新しい計測の費用を区別する。BF性能値とは別の結果として報告する。

### 成果物と完了条件

- 計測コード、回帰テスト、小さいphase設定fixture、集計・再実行スクリプトをcommitする。
  スクリプトは`scripts/`以下、生成物は`./tmp`以下。既存結果をデフォルトで上書きしない。
- 本ファイルの進捗を更新し、EVALUATIONに測定条件・結果・制約・成果物パスを追記する。
  別のNEXT/FIX文書は作らない。
- 次の判断材料として、熱いphase/関数、portal要求のregion集中度と近接性を提示する。
  局所構造化とportal連続処理のどちらを次に試すか、実測した範囲で提案する。
  IRの回数だけでBFの時間改善率を予測しない。
- 自分の変更だけをcommitし、push/mergeはしない。既存のAGENTS.md、.gitignore、
  fizzbuzz.bfc等のユーザー変更は巻き込まない。
- 完了報告はcommit、検証結果、主要な集計値、overhead、再実行方法、未完項目を含める。
  依頼元の監視や返答待ちは不要。本節の範囲を完了して新規セッション内で報告する。

## 当初調査の目的と現在地（2026-09-08）

目的は `bf-compiler` が生成する BF による selfhost の実行時間短縮。
interpreter の native operation 数だけでなく、生成 BF のサイズ、raw/RLE換算命令数、
実行時間、tape使用量も記録する。

- 直近の最適化は commit `e9ebac2` (`Optimize D16 portal offset nibble decomposition`)。
  D=16 portal の offset を、8 bitへの分解・再結合ではなく直接 nibble に分解する。
- 同commitには実装・回帰test・`BF_OPTIMIZATION_NOTES.md`の測定記録が含まれる。
- `full3.metrics` は変更前、`full4.metrics` は変更後の実行中snapshot。
  完走結果ではなく、観測したphase・実行時間も異なる。最終行は今後変わりうる。
- `full4` では page countdown、portal window 左右、global navigation が引き続き重い。
  offset改善だけで全体が大幅に速くなる状況ではない。
- 今回の調査では追加の最適化実装は行っていない。このファイルは実験計画。

既存の設計・実験記録は `BF_OPTIMIZATION_PLAN.md`、`BF_OPTIMIZATION_NOTES.md`、
`ABI.md`、`BF_PROFILING_DESIGN.md` にある。計画文書の古い「未実装」表記より現コードを優先する。

## 作業上の注意

- `AGENTS.md` に従い、ユーザーから明示的な依頼がない限り subagent を使わない。
- 調査時には `.gitignore`、`fizzbuzz.bfc` にユーザーの変更があり、metricsや`tmp.*`も存在した。
  これらを巻き込んで変更・commitしない。作業開始時にstatusを再確認する。
- 実行中の selfhost を停止したり、使用中の `tmp.bf` / `tmp.bfmap.json` を上書きしたりしない。
  実験用artifactは別directoryに出す。
- BFは約6GB、interpreterのRSSも数GBになる。長時間比較を多数同時に走らせず、
  現在の実行とのCPU・メモリ競合を確認する。
- BFとprofile mapは必ず同時に再生成する。別artifactのsite IDを使い回さない。

## 調査で得た具体的な手掛かり

`scripts/concat-stage2-compiler.sh main` のproduction sourceをRust frontendでloweringすると、
221 functions、6,068 continuations、80 globalsになった。現 `tmp.bfmap.json` の通常continuation数も
6,068で、調べたIDの所属functionも一致した。ただし、これはartifact全体の同一性の証明ではない。

| continuationの終端 | 静的個数 |
|---|---:|
| Branch | 1,104 |
| Call | 2,458 |
| Goto | 2,020 |
| Return | 407 |
| AggregateLoad | 41 |
| AggregateStore | 36 |
| その他 | 2 |

このうち **1,056個は本体が空のGoto**。すべてが無条件に削除可能という意味ではない。
entry、callのreturn target、portalのresumeなどへの参照と到達可能性を調べる必要がある。

同じIRを既存の `run_continuations_with_io` で実行し、
`selfhost/stage2/examples/hello.bfc` を入力したときの回数は次のとおり。

| 指標 | 回数 |
|---|---:|
| continuation実行 | 79,416 |
| `arena_advance` 内のcontinuation実行 | 53,728（約68%） |
| 空のGotoの実行 | 8,760（約11%） |
| Call | 5,547 |
| AggregateLoad | 563 |
| AggregateStore | 809 |

このloweringでは function 43 が `arena_advance`。
特に頻出したcontinuationは984、985、988、989、991、992、994、996、1002、998。
`full4`で重いsite 26447の親は `abi.dispatch.page.14` であり、同pageには
`arena_advance`、`node_is_null`、`node_equals`、`arena_cell_read` などが入る。

**上記は小さい入力のIR実行回数であり、full selfhostの時間比率ではない。**
IR runnerにはBF backendが追加するhidden portal/router/resumeのdispatchや、BF上の搬送費用がない。
それでも、最初に実験する関数を絞る根拠にはなる。

`arena_advance` は `selfhost/stage2/compiler/06_arena.bfc` にある。
field位置を求めるためにamount回ループし、その内部にbank/page/slotの境界判定がある。
現在のloweringでは、このwhileと内部のifが多数のcontinuationに分かれる。

## 実験1：phase別の回数・遷移・portal要求の計測

最初に、時間profileだけでは分からない「何が何回起きたか」を補う。

### 現profileの制限

- sample modeの `fast_operations=0`、`pointer_distance=0` などは未計測を表す。
- `context` は `bf-interpreter/src/main.rs` の `profile_context` がprofile mapの親を辿って求める。
  共有portalの実際の要求元を動的に追跡した結果ではない。
- countdownのsamplesから、選ばれたcontinuationの回数は復元できない。
- `profile_block_executions` はsemanticなcontinuation entry数ではない。
  未選択caseのguard確認などを実行回数と取り違えない。
- 累積top 10だけでは、phase変化・多数siteに分散した費用・正確な割合が見えない。

### 追加したい情報

1. continuation別の実行回数と `from -> to` の遷移回数。
2. Goto/Branch/Call/Return/portal要求ごとの遷移分類。
3. portal要求元、対象region、load/store、offset、転送cell数。
4. 同じregionへの連続アクセス、offset差、同じchunkの再訪率。
5. BF側では、要求1回あたりのnavigation scan steps、window jump回数、
   搬送費用、stack深度など。payload値の影響が疑われる場合はその分布も調べる。
6. phaseごとの総sample数と全siteの差分。全履歴traceより集計・histogramを優先する。

### 進め方

既存 `ContinuationRunStats::hottest_continuations` とIR runner内のカウンタを足場にする。
同じsource・同じlowering結果の実行頻度を得て、BFの時間profileと対応させる。
IR runnerだけでhidden dispatchやABIの費用まで測れたことにはしない。
必要なBF側event計測は、未選択guardと本体entryを区別し、最適化による命令統合とも整合させる。

Rust source経路と `--cir-input` 経路ではIDやIR構造が異なる。
実際、既存logs内のCIRをadaptした結果は5,854 continuationsで、上記6,068とは異なった。
source/CIRのidentity、compiler commit、生成オプション、BF/map identityを測定結果に残す。
function名・source位置とlogical IDの対応も出せるようにすると、今回の手作業での照合を省ける。

phaseは入力読み込み中・読み込み後・出力中から始め、可能ならlexer/parser/semantic/lowering/codegenを区別する。
同じelapsed timeのsnapshotが同じ仕事量を表すとは限らない。
途中で入力を切って別のプログラムにする方法では、後半phaseの代替測定にならない。

成果物は、phase別の上位continuation・遷移・portal要求と、ABI費用との対応表。
計測追加自体のoverheadを小さい完走benchmarkで確認する。

## 実験2：空Goto・局所if/whileのdispatcher往復を減らす

最初の対象を `arena_advance` とする。次の変換は別々に測る。

1. 空Gotoのjump threading、unreachable除去、安全に結合できる直列blockの結合。
2. call・portal・return等で中断しない局所if/whileを構造化されたFrameInstructionへ変換。
3. 別案として、`arena_advance` 自体を反復から桁上がり付き加算へ変更。

IRには既に `FrameInstruction::Loop` と `Branch` がある。
一方、Rustの `continuation_lowering.rs::lower_while` は通常のsource whileを条件・本体・終了に分ける。
BFC製frontendの `09_continuation_ir.bfc` も同様に分割する。
HIRで分割を避けるか、Continuation CFGから局所構造を復元するかを比較し、適用される入力経路を明記する。
HIRだけの変更はCIR入力経路には効かない。

狙いは静的なID数だけでなく、熱い反復中のdispatcher通過回数を減らすこと。
均等な二段選択の単純モデルでは、ID数を半分にしても選択距離は約0.71倍にしかならない。
局所ループ化では、反復ごとの選択・NextPC設定・PC更新を省ける可能性がある。

### 正しさの重点

- 条件の再評価、条件cellの消費、ネストした分岐・ループ、I/O順序。
- return/abortやcall/portalを含むregionを誤って単一の局所loopにしない。
- call return target、portal resume、function entryなどの参照を更新する。
- CFG変換をframe allocationの前後どちらで行うかを決め、liveness・slot再利用・branch scratchを再検証する。
- `arena_advance` の算術変更はslot=255、page末尾、bank末尾、amount=0/255、arena終端の失敗動作を保つ。
  BFでは比較も高価なので、O(amount)を消しただけで速いと判断しない。

### 比較

固定入力の関数microbenchmark、helloコンパイル、複数の小さいsource、full selfhostの代表phaseで比較する。
最初は実験1と本実験を完了し、結果を見て後続の順序を決める。

## 実験3：頻度に基づくPC符号配置

現 `abi_codegen.rs::DispatchEncoding` は密なIDを概ね平方根幅のpageへ配置し、
hidden portal IDを優先し、一部pageのlow順を反転する。通常continuationの動的頻度は使っていない。

現dispatcherは毎回先頭から選択するため、頻出continuationを隣に置くだけでは安くならない。
まずは「選択段数とPC搬送費用が少ない符号」に、実測で頻出するcontinuationを割り当てる。
page幅・page順・page内順を候補にし、hidden portal優先の現方式と比較する。

- 時間samplesを実行頻度の代用にしない。重い本体と頻繁なentryは異なる。
- logical IDとencoded PCを分け、profileとの対応と再現可能な配置を保つ。
- 別phase・別sourceでも評価し、hello専用の配置にしない。
- Call/Return/portal経由のPC搬送費用も含める。

遷移の近さを利用する別案は、同じpage内での遷移なら外側dispatcherを省く方式やfunction-local dispatcher。
これは単なる符号の並べ替えとは別の実験にする。

## 実験4：portalの連続処理と移動ABI

`full4`でwindow左右・global navigationが重いことから、offsetだけでなく往復数を減らす。
現 `jump_portal_window` は、16セルのprotocolと移動先payloadの値を交換する。
単一cellのload/storeでも複数セルの搬送が発生する。

計測したアクセス列を使い、次を順に比較する。

1. 同じnodeの複数field、連続cell、aggregate copyを一度のportal滞在にまとめる。
2. 初回offset計算後、近接offsetへ増分移動して複数操作を実行する。
3. より大きな変更として、payloadを固定し、専用制御領域・laneだけを移動するABI。

同じregionへのアクセスでも、間のcall・alias・store・I/Oを越えて安全にまとめられるとは限らない。
値のsnapshot、重複copy、再帰、global/frame境界を保つ。
専用lane案ではtape増加とnavigation距離の変化も費用に含める。

## 実験5：PCの3・4分割を独立評価

約6,000件を均等に選択する単純モデルでは、二段の約78×78に対し、
三段なら約19×19×19、四段なら約9×9×9×9となる。
選択距離は減りうるが、これは実時間の高速化率を表さない。

現headerは16セル。Pc/NextPc/ReturnPcの各2セルを素直に4セルへ増やすと22セルとなり、
D=16でportal/contextが2chunkになる。既存の1chunk専用移動最適化が使えなくなるため、
選択だけ速くなって全体が遅くなる可能性がある。

最初にproduction ABIから切り離したdispatcher benchmarkを作り、同じ論理遷移列で
二段・三段・四段を比較する。均等・偏った頻度・実測traceを使う。

- countdownの進入側だけでなく、戻り側guard確認も測る。
- PC設定・copy・搬送・return・追加scratchの費用を含める。
- scratchの生存期間を利用して16セルを維持する案を検討する。
- 2セルで保存し選択時だけ分解する案は、その分解費用も測る。
- 0やID境界、無効PCについて既存contractを確認する。

有利な結果が出た場合に初めてproduction ABIへ適用し、portal/call/returnを含めて再比較する。

## 共通の評価・完了条件

比較対象はその実験開始時のbaselineとする。独立した案を混ぜず、採用済み変更を明記する。

| 測るもの | 用途 |
|---|---|
| 出力の一致・実行結果 | 意味の保持 |
| phase別の実行時間 | 利用者が得る改善 |
| parse時間とexecute時間 | 巨大BFの読み込み費用を分離 |
| continuation/hidden dispatch/portal回数 | 往復削減の確認 |
| native operations | 現interpreterでの仕事量の手掛かり |
| raw/RLE換算命令数 | 生成BFそのものの費用の手掛かり |
| BF bytes・mapサイズ | コード展開の代償 |
| 最大pointer・RSS | ABI/layout変更の代償 |

native operation数やraw pointer距離は単独では実時間を予測できない。
現interpreterはscan・linear transfer・countdown等をまとめて実行するため、
同じ命令数削減でも効果が異なる。必要ならinterpreter自身のCPU profileも取り、
BF上の費用と実装上の管理費用を切り分ける。

小さい完走benchmarkを反復し、中央値などを記録する。
full selfhostは同じ入力・同じphase境界で比較し、完走時間未測定ならその旨を書く。
プロセス全体のwall timeを、parseを除いたexecute時間と混同しない。

適切な差分テスト・対象回帰を行い、共通lowering/ABIを変えた場合は `cargo test --workspace` を実行する。
PC/portal変更ではD=8/D=16、再帰、境界、profile mapの整合性も確認する。
採用理由、測定条件、限界を記録し、効果のない試作をproductionに残さない。

## 最初のセッションで目指す成果物

1. 同一artifactに対応するphase別実行・遷移集計と、function名への対応。
2. `arena_advance` を対象に、空Goto除去と局所if/while統合を分けた比較結果。
3. その結果に基づく次の判断：PC配置を先に試すか、portal往復削減を先に試すか。

PC4分割や全面的なABI変更は、上記の判断材料が得られる前に一括導入しない。
