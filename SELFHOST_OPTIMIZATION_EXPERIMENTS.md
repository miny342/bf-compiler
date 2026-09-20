# Selfhost 最適化実験

> 現在のABIはD=16のみをサポートする。以下のD=8への言及は実験当時の記録である。

## 目的と記録方針

生成BFによるselfhostの実行時間を短縮する。
計画・現状は本書、採否と当該コミットの検証結果は
`SELFHOST_OPTIMIZATION_EXPERIMENTS_EVALUATION.md`に記す。
検証済みの採用・不採用案と理由・制約は残す。ログの場所、統計、artifact hash、
実行ごとの表は各コミットの作業結果だけに置き換え、過去の詳細はGit履歴で参照する。
完了したセッションへの作業指示は積み重ねない。NEXT/FIX文書は増やさない。

## 現在地と次の作業

- 空Goto threading・到達不能除去・ID compactionはsource/CIR共通で採用済み。
- HIRの単純なローカルwhileを、セル割当て前に直接Frame Loopへlowerする。
  非ゼロ条件は元セルを検査し、ローカルOutputの一時コピーと定数更新の一時セルを省く。
  複雑な条件・call/portal等は既存loweringへ戻す。多箇所inlineの拡張は未実施。
  IR phase設定のidentityはv4。以前の設定は再生成する。
- 大小比較をFrame Compareとして保持し、BF生成時にABI scratch上の非破壊判定へ展開する。
  source/binary CIR共通。比較反復内のoperand複製・復元を除く。
  Tape IR導入、十進変換の定数特殊化、多箇所inline拡張は未実施。
- BFCRLE v1はRust版の `--compressed-bf` で使用可能。通常BFとprofile互換を維持する。
- セルフホスト版も`concat-stage2-compiler.sh compressed`でBFCRLE出力を選べる。
  命令列の変更ではなく可逆な保存形式の変更。通常BFの`main`とbinary `cir`は維持する。
- interpreterのRemoteTransferを採用。Scan経路と更新先が独立な局所転送を実行時に
  一括化し、`--disable-remote-transfer`で比較可能。
  compiler側のportal batch/専用laneとは別の最適化として評価する。
- Rust backendのframe/global byte搬送はunaryを既定とし、
  `--enable-nibble-transfer`で従来のBF命令数削減方式を選択可能。
  offset分解・window移動・ABI配置は維持する。比較はRemoteTransfer ON/OFFそれぞれで行う。
- 同一関数の非空Branch後継inline化（2c）は採用済み。比較用に`--disable-2c`を保持。
- arena算術化は試作・検証後に撤回。単一入口の局所if/while構造化（2d）はsource/CIRの
  production BF比較に基づき通常経路へ採用。比較用に`--disable-local-control-flow`を保持。
- IRの継続・遷移・終端・phase別集計とportal要求集計は実装済み。
  phase設定は実入力byte列・lowering optionsのidentityに結び付ける。
  sourceは関数名、CIRは確認済みartifact限定の明示function IDを使用する。
- 設定入力上書き防止、phaseを跨ぐportal隣接切断、identity照合と
  固定CIR mapping検証は修正済み。開始chunk再訪はphase全体の履歴指標である。
- BF portalの詳細profileとコンパクト再現を追加済み。full phaseのhidden dispatch/navigation計測、
  実験3〜5のABI最適化、full selfhost時間比較は未完。
- 局所構造化のproduction比較とstage-12 selfhost機能検証は完了。
  次の候補は残るBF側費用の計測、PC配置、portal往復削減。代表phaseの実測で優先順位を決める。
  `scripts/local-structure/run.py`はdefault採用後もON/OFF比較を再現できる。
  `arena_advance`はphase別IR集計で熱い関数として確認された。
  portal再訪率だけからbatchの安全性・優位性を判断しない。

## 2026-09-20: full23 と BF シリアライザの手動特殊化

full19からfull23までのselfhost実験で、BF側の比較・継続・値コピーが主な費用として残った。
今回の修正と観測は次のとおり。

- `emit_move_to`では、ユーザーが追加した素朴な修正として現在位置をローカルcellへ一度コピーし、
  そのローカル値を比較・更新してから`target_position`へ戻すようにした。
  full23 profileではこの関数のexclusive shareが19.716%から12.140%へ下がったが、
  full19/full23は起動環境と入力条件が完全には同一でないため、時間の差をこの変更だけの効果とは断定しない。
- `emit_repeat_wide`は、セルのwrappingを利用した固定のgreedy展開へ変更した。
  10^7桁は最上位が0/1なので`if`、10^6〜10^1は比較順5,2,1,1の4段、10^0は残ったlowをそのまま出力する。
  1000〜10000桁の比較では残余の`high`も考慮し、単純化による桁落ちを避けた。
  これにより、動的な繰り返し回数と`sub_with_borrow`を減らすことを狙った。
- full23（`logs/full-selfhost-20260920-014052`）では、full19比で
  `abi.frame.sub_with_borrow`のshareが14.644%から8.497%へ、
  `emit_repeat_wide`のshareが21.279%から17.109%へ下がった。
  一方、`abi.frame.compare`は9.575%から12.961%へ増え、固定展開が比較とdispatcher countdownを
  別のボトルネックとして表面化させた。`emit_repeat_wide`のcontinuation数は50から166へ増えているため、
  継続を増やし過ぎない自動特殊化は将来候補だが、今回は実装しない。
- 圧縮BFの`emit_repeat_character`は、full25で100/10のwhileから
  200,100,50,20,10,10の固定6比較へ一度変更した。しかしfull25 profileでは
  関数時間が344.6秒から482.0秒、内部compareが211.5秒から373.8秒へ増えたため、
  full25取得後に元のwhile実装へ戻した。固定展開版のfull25 profileは、この撤回後の現ソースの
  性能結果としては扱わない。

full23の生成物を使った`hello.bfc`（`A!`）と`arithmetic.bfc`（出力byte列255,2,0）は、
生成BFを実行した結果とRust IR実行結果が一致した。固定展開版のstage2 compilerでも同じ2例と
count 0〜255を含むrepeat出力を検証し、結果一致を確認した。その後、固定展開版を撤回して
元のwhile実装へ戻している。

今後の候補として、比較命令の復元、global/static cellのアドレス解決メモ化、
`arena_read`/`arena_advance`の特殊化、continuationをまたぐ制御構造の再配置がある。
これらはfull23だけでは効果と正しさを断定できないため、今回のコミットでは保留する。

以下は各実験の設計と検証条件。実験1のIR部分と実験2の空Goto・2c部分は完了しており、
既存実装を足場に未完項目へ進む。実装済み部分の再作成は不要。

## 作業上の注意

`AGENTS.md`に従い主agentで作業し、一時ファイル・build・生ログは`./tmp`に置く。
既存metrics・artifact・ユーザー変更・実行中selfhostを保持する。
大容量BFを生成・実行する前にCPU・メモリ・空き容量を確認し、同時実行を抑える。
BFとmapは同時生成する。再現scriptは`scripts/`で追跡し、生成物は追跡しない。
自分の変更を検証してcommitを続け、push/mergeは行わない。
既存設計は`ABI.md`、`BF_OPTIMIZATION_PLAN.md`、`BF_OPTIMIZATION_NOTES.md`、
`BF_PROFILING_DESIGN.md`を参照し、古い未実装表記より現コードを優先する。

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

interpreter側のRemoteTransferでScan付き転送を一括化できるようになった。
今後のABI/nibble比較ではinterpreter設定を固定し、必要ならON/OFFの両方を測る。
既存BFの実行高速化と、BF自体の往復・搬送量削減は区別する。

既存profileでwindow左右・global navigationが重かったことから、offsetだけでなく往復数を減らす。
現 `jump_portal_window` は、16セルのprotocolと移動先payloadの値を交換する。
単一cellのload/storeでも複数セルの搬送が発生する。

計測したアクセス列を使い、次を順に比較する。

1. 同じnodeの複数field、連続cell、aggregate copyを一度のportal滞在にまとめる。
2. 初回offset計算後、近接offsetへ増分移動して複数操作を実行する。
3. より大きな変更として、payloadを固定し、専用制御領域・laneだけを移動するABI。

同じregionへのアクセスでも、間のcall・alias・store・I/Oを越えて安全にまとめられるとは限らない。
値のsnapshot、重複copy、再帰、global/frame境界を保つ。
専用lane案ではtape増加とnavigation距離の変化も費用に含める。

### 実験4のコンパクト再現と詳細profile

`scripts/portal-profile/run.py`を追加。productionのstreaming optimizerを抜き出したケース、
1/3セル・global/frame・chunk境界・stack条件・ゼロ/非ゼロpayloadのケース、
production routerの7byte搬送をそのまま呼ぶfixtureを分けて測る。
CLI/profile形式を変えず、要求フィールド別の分解・搬送、window交換、要素選択、resumeを細分化した。
生成BFを変更する最適化はこの計測変更に含めない。

手順と計測上の制限は[`scripts/portal-profile/README.md`](scripts/portal-profile/README.md)。
`run.py`は搬送生成方式を固定し、RemoteTransferのON/OFFを同一BF・入力で比較する。
`compare-transfer.py`はunary/nibbleの生成BFを同じ入力で比較し、
RemoteTransfer ON/OFFを別々に測定する。global scalar copyと長いlive stackのfixtureも追加した。
コンパクトケースはfull selfhostのphase比率の代わりにはならず、次の変更候補を絞るために用いる。

当時のfull15でhotなglobal 69/70は、保存sourceのloweringからBF最適化器のkind/count配列
（16/48セル）と対応づけた。count読み書き削減を試した後、full16の観測を受け、
最適化器自体を撤去して素朴なBFCRLE出力へ戻した。以下のportal案は未実装の候補である。
次は小配列の専用accessorで、要素選択と既知位置のcopyを行い、window交換・PC搬送を省く案を比較する。
有効indexだけを扱えばよく、範囲外は言語仕様上UB。
大配列では256セルごとの独立portal prefixと、相対位置で共用する内側accessorを候補とする。
二段選択なら全要素のcaseを列挙する必要はない。ただしprefix/markerの追加配置、
呼出し後の復元、論理offsetから物理位置への変換、および外側の長距離Moveを含む
通常BF長とBFCRLE長をそれぞれ評価する。行選択でscan guardを書き換えるloopは、
現RemoteTransferの経路不変条件を満たさないため、自動的に一括化できるとは仮定しない。

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

## 次の判断に必要な成果物

1. 同一artifactに対応するphase別実行・遷移集計と、function名への対応。
2. `arena_advance` を対象に、空Goto除去と局所if/while統合を分けた比較結果。
3. その結果に基づく次の判断：PC配置を先に試すか、portal往復削減を先に試すか。

PC4分割や全面的なABI変更は、上記の判断材料が得られる前に一括導入しない。
