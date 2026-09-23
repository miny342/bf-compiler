# Brainfuck backend最適化計画

## 文書の位置づけ

この文書は、Rust版`bf-compiler`が生成するBrainfuckを高速化し、その結果を後から
BFC製self-host compilerへ移植するための実装計画を定める。

個別のBrainfuck template候補、既存peephole optimizationの測定値、参考アルゴリズムは
[BF_OPTIMIZATION_NOTES.md](BF_OPTIMIZATION_NOTES.md)に記録する。実時間profilingのartifact、
計測mode、provenanceの仕様は[BF_PROFILING_DESIGN.md](BF_PROFILING_DESIGN.md)に定める。

この計画の第一目的は、生成BFの命令数だけを減らすことではなく、repositoryのself-host testと
bootstrap verificationに要する実時間を短縮することである。BF source bytes、raw BF命令数、
RLE換算命令数、使用cell数は、実時間の変化を説明しregressionを検出するための副指標として扱う。

## 現状

compiler pipelineは次の形である。

```text
BFC source
  -> typed HIR
  -> Continuation IR
  -> ABI backend
  -> BF IR
  -> BF peephole optimization
  -> Brainfuck source
```

現行のBF peephole optimizationは次を実装済みである。

- 隣接するpointer移動の統合と相殺。
- 隣接する加減算の統合と相殺。
- zero move、zero addの除去。
- clear loopの認識と`[-]`への正規化。
- clearまたは加減算直後の`Input`による上書きの簡約。

さらに、profile map、counter/sample/exact profiler、provenance付きBF IR、およびhigh/low byteの
二段countdown dispatcherを実装済みである。dispatcherは密なID範囲に破壊的countdownを使い、
疎な範囲だけequality scanへfallbackする。測定値とtemplate契約は
[BF_OPTIMIZATION_NOTES.md](BF_OPTIMIZATION_NOTES.md)に記録する。

現行checkoutには独立したContinuation IR optimizer passはない。HIR lowering中の定数条件除去、
ABI backendでのdispatcher選択とaggregate clear融合、BF IR peepholeを区別する。各IRの正確な境界と、
追加IRを判断する条件は[IR.md](IR.md)に定める。

BF peepholeは生成BFのsource sizeへ大きく効く一方、RLE型target上の実行step削減は比較的小さい。
今後の主な対象は、最終BFから意味を推測するpeepholeではなく、Continuation CFGの縮約と、意味および
pointer条件を保持しているABI backendの改善とする。

2026-08-28のrepository headで、release buildによるRust compiler自身の生成時間は約0.12秒、
self-host production compilerの生成BFは約39,098,796 byte、内部test用生成BFは約53,238,909 byte
だった。内部test用BFは30秒以内に完了せず、full self-host verificationも120秒以内に完了しなかった。
この値は恒久的なbenchmark baselineではなく、profiling基盤を導入する理由を示す初期観測値である。

## 基本方針

### 実時間を採否の主指標にする

最適化の最終評価には、profiling情報を含まない通常のBF artifactを使用する。同じrelease build、
同じ入力、同じ実行条件で複数回測定し、原則としてwarm-up後3回以上の中央値を比較する。

詳細profilingはbottleneckの特定と変化の説明に使用する。profiling実行そのもののwall timeを、
最適化の合否値として使用しない。

### 最も意味の高い層で最適化する

- Continuation数や遷移数の問題はContinuation IRで解決する。
- dispatcher、call、return、portal、global navigationの問題はABI backendで解決する。
- localなBF命令の冗長性だけをBF optimizerで解決する。
- sourceの評価規則やlive/dead情報が必要な最適化はHIRまたはContinuation loweringで解決する。

この分類は、Continuationを入力にする最適化をすべてContinuation IR optimizerと呼ぶ、という意味ではない。
たとえばdispatcherの比較/countdown方式はABI fieldとpointer条件を使うためABI backendに置く。一方、
unreachable除去やjump threadingは`ContinuationProgram`自体を書き換える独立passに置く。

BF文字列から巨大なtemplateを再認識するpassは、他のpeepholeによるわずかな形の変化で壊れやすく、
self-host compilerへも移植しにくいため主方式にしない。

### ABIは比較対象であり固定条件ではない

初期のtemplate改善は現行ABIのframe field、portal layout、`D = 16`を保ったまま行う。ただし、
profilingによってABI固有の走査、往復、materializationが支配的と判明した場合は、現行ABIとの互換性を
絶対条件にしない。新方式は`AbiConfig`または明示的なABI versionとして旧方式と比較可能にする。

ABIを変更する場合も、source language semanticsと外部から観測可能な入出力は維持する。異なるABIの
objectをlinkする仕組みは現在存在しないため、一つの生成BF内でABI versionが一貫していればよい。

### Rust版とself-host版でtemplate契約を共有する

最適化templateごとに次を言語非依存の契約として記録する。

- 入力と出力。
- 使用するABI field。
- 入力cellのlive/dead条件。
- 破壊するcell。
- 初期値0を要求するscratch cell。
- 終了時に0であるべきcell。
- entryとexitのBF data pointer位置。
- `D = 16`でのchunk境界の影響。
- source bytes、raw steps、RLE steps、wall time、追加cell数。

RustとBFCで同一のBF文字列を生成する必要はないが、同一の契約testを通し、同じABI上の状態遷移を
実現しなければならない。

## Milestone 0: 実時間profiling基盤（完了）

[BF_PROFILING_DESIGN.md](BF_PROFILING_DESIGN.md)に従い、次を実装した。

1. interpreterのload、parse、fast IR build、execute、output writeの区間計測。
2. compiler内のBF命令へprofile site provenanceを保持するannotated BF IR。
3. markerを含まないBFと対応するsidecar profile map。
4. ABI template、function、continuation、FrameInstruction別の実行counter。
5. 低overheadなsampling profilerとmicrobenchmark用exact profiler。
6. Rust compiler生成artifactで使用できる、BF命令以外の文字だけからなる埋め込みprofile marker。
   BFC製self-host compiler自身からのmarker生成は未実装である。
7. machine-readableなprofile reportと、inclusive/exclusive timeのtree表示。

初回profileでは少なくとも次を分離する。

- BFの読み込みとparse。
- fast IRへの変換。
- dispatcher。
- user continuation body。
- callとreturn。
- local frame operation。
- global/context navigation。
- aggregate portalのoffset計算、window移動、load/store、resume。
- structured branchとscalar comparison。

この測定結果で後続milestoneの順序を再確認する。以下は現行実装から推定した優先順位であり、profileが
異なる結果を示した場合は、実時間比率の高い項目を先にする。

## Milestone 1: DispatcherとContinuation CFG

ABI dispatcherの二段化とhigh/low byte countdownは完了した。Continuation CFGの縮約passは未実装である。

### 問題

初期dispatcherはdispatch cycleごとに全continuationおよびhidden portal continuationを列挙し、
各IDについてPC low/high byteをcopyして比較していた。この方式はcontinuation数を`C`とすると、
1遷移あたり概ね`O(C)`だった。

現行Rust backendはhigh byteでpageを選び、page内のlow byteを選ぶ。連続したpage/IDには破壊的countdown、
疎な集合にはequality scanを使う。self-host compilerのABI codegenへの移植と、profileに基づく
ID配置は残作業である。

### 検討したdispatcher variant

次を独立variantとして比較し、2と3を組み合わせた方式を採用した。

1. 旧ID equality scan。
2. 255件以下を対象とした1段countdown dispatcher。
3. high byteを選択してからlow byteを選ぶ2段countdown dispatcher。
4. function IDとfunction-local continuation IDを分ける階層dispatcher。
5. ABI上のmarker位置を遷移させるmoving-marker dispatcher。

4または5は未採用である。将来明確に有利と測定され、現行PC fieldやframe headerが妨げになる場合は
ABI versionを分ける。

### Continuation IR側の縮小

dispatcher変更と別passとして次を実装する。

- unreachable continuationの除去。
- 空の`Goto` continuationのjump threading。
- address-takenでないsingle-predecessor continuationの結合。
- constant branchの除去。
- portal resumeやcall return targetを誤って統合しないaddress-taken解析。
- 変換後のcontinuation IDの密な再採番。

profileが揃った後に、hot continuationへ小さいIDを割り当てる配置も比較する。最初からsource順を
変更すると再現性とdebuggabilityを損なうため、profileなしの頻度推測だけでは導入しない。

### 検証

- continuation ID境界の1、255、256、257、65535。
- user continuationとhidden portal continuationの混在。
- direct recursionとmutual recursion。
- scalar/aggregate callとreturn。
- invalid runtime PCをABI上の未定義状態とするか、停止させるかの明文化。
- 旧dispatcherと新dispatcherのdifferential execution。

## Milestone 2: Aggregate portal

### 連続操作を一つのtransactionにする

現行のaggregate accessは、複数cellの値についてもcellごとにportalへ入り、load/storeし、dispatcherを
経由してresumeする場合がある。struct、array、aggregate copyを次の単位へまとめる。

```text
enter portal once
  -> compute initial offset once
  -> visit contiguous cells while moving the window incrementally
  -> perform all load/store/copy operations
leave portal once
  -> resume once
```

最初に次をbulk operationとして扱う。

- contiguous clear。
- contiguous load/store。
- aggregate copy。
- aggregate argumentとreturn outboxの搬送。

途中のcellでsourceとdestinationがaliasしうる場合は、source semanticsで要求されるsnapshotを保持する。

### 固定除数divmod

logical offsetからchunkとremainderを求める処理では、除数`D`は8または16でcompile-time定数である。
汎用的な逐次除算ではなく、固定除数templateを比較する。

- canonicalな`(quotient, remainder)`を作る方式。
- page/slot countdownへ直接つなぎ、中間値をmaterializeしない方式。
- `D`が2の冪であることを利用する専用方式。

`D = 16`、low byte全256値、high byteを含む有効offset境界で検証する。

### Moving-index accessor

indexまたはremaining countを隣のchunkへ運びながら対象位置へ到達する方式を比較する。sentinelとして
利用できるのはABIがhead、flag、anchorと定義したcellだけであり、任意の0を含みうるpayloadを走査
条件に使用してはならない。

### Portal ABIの変更候補

profile結果によっては次をABI versionとして検討する。

- cell単位のportal protocolからrange transaction protocolへの変更。
- continuation resumeを介さないportal内loop。
- offset、length、operation kindを持つbulk request。
- global/localで同じprotocolを使いつつnavigation部分だけを分離する構成。
- portal scratchとframe headerの再配置。

## Milestone 3: Global navigationとframe境界

現行のglobal accessは、動的位置にあるframe contextからflag laneをanchorまで走査し、static globalへ
移動し、再びfrontier側へ戻る。走査量はactivation数だけでなくlive frame chunk数に依存する。

### 2026-09-24時点の判断

小さいglobal aggregateの近接配置だけを実装・検証し、以降のABI変更は保留する。
以下の番号はfull38調査時の候補番号であり、実装順序や着手予定を表さない。
候補と再検討条件はこの文書を正とし、一時artifactがなくても設計判断を参照できるようにする。

候補1は`ee4fa7e`で実装済み。16セル以下のaggregateを大きいaggregateの後、scalar globalsの
直前へ移し、各群の宣言逆順、portal prefix、anchor位置、総容量を維持した。full38との比較で
圧縮前BFは9,169,772,770から780,510,141 bytes（91.5%減）、実行raw命令数は51.5%減となった。
RLE/native命令数は一致し、生成結果もSHA-256まで一致した。定数moveをまとめる現在のinterpreterでは
実行速度改善とは扱わない。測定の詳細は[BF_OPTIMIZATION_NOTES.md](BF_OPTIMIZATION_NOTES.md)の
「2026-09-24: 小さいglobal aggregateをanchor近傍へ配置」に記録する。

巨大arenaの向こうに小structが置かれることで生じた長距離移動は、この配置変更で大きく減った。
以降の候補は変更後のprofileで必要性が示された場合に再検討し、現時点では実装しない。

### 候補2: Portal不要aggregateのpacking（低優先度・保留）

現行layoutは非空aggregateすべてにportal prefixを与える。D=16では3セルのstructでもpayloadと
prefixで計2 chunk、34物理セルを使う。動的AggregateLoad/Storeを受けないregionを通常のdata slotへ
詰める、またはscalar replacementする余地はある。

ただし、prefixを持つ変数が大量にあることは今回の巨大BFの原因ではなかった。近接配置後に
prefixだけを削る効果は小さいと見込み、積極的な実装候補からは外す。allocatorの寿命再利用後にも
残るregion数と削減可能chunk数が、frame容量・stack scan・Call/Returnを圧迫すると測定された場合のみ
再検討する。globalのstatic距離短縮とframe縮小の効果は分けて評価する。

再検討時には型サイズだけでなくregionの使用方法を解析する。動的portalが必要なregionは従来layoutを
保ち、定数field access、AggregateCopy、値渡し、aggregate subrange、parameterとelementのalias、
outbox配送の意味を保持する。backendのregionからelement位置へのmappingも対応が必要であり、
allocation後にprefixだけを一律削除する方式にはしない。

### 候補3: Global側での処理とframe復帰の削減（保留）

各helperがframe contextから出発して毎回frameへ戻る規約は、言語仕様ではない。
次の変更をそれぞれ独立した候補として残す。

- global→global copy、連続Set、global同士の比較をstatic座標とstatic scratchで実行する。
  比較結果だけをframeへ返すなど、運ぶ値そのものを減らせる場合も対象にする。
- global→frame copyのsource保存・復元をstatic側scratchで行う。既存9セルscratchの予約と
  非再入性を確認し、scratchまでの距離によって逆効果になる場合も計上する。
- backendでFrame/Staticの現在位置を追跡し、依存するload/compute/storeをまとめ、必要な境界で
  frameへ戻る。同じaggregateへの連続portal処理や、frame復帰不要の終端処理との融合も候補にする。

単に区間をまとめても複数cellの値が一度のpointer往復で運べるわけではない。不要な復元・frame復帰・
要求metadata・搬送する値の数を区別する。CIRのeffectsに従い、対象globalを観測・更新するCall/portalや
依存する操作を越えず、snapshotとI/O順序を維持する。local templateの変更とyieldを伴う変更も分ける。
B1のframe-relative branch/loopを閉じてからcontextを移す条件は保持する。

### 候補4: 閾値で選ぶ共有globalサービス（保留）

全globalアクセスをdispatcher経由にはしない。inline/B1による複製後の静的なアクセス命令数×移動距離が
閾値を超える候補に限り、global/operationごとの移動列を共有する案を検討する。閾値は未決定である。
コードサイズの判断には共有helperと各request/resume stubの費用を含め、実行速度には動的な頻度、
要求・結果の搬送、追加dispatchの費用を別に計上する。共有化だけでは動的な移動命令は減らない。

候補のprotocolは、callerで引数・store値をsnapshotして共通mailboxとresume PCを設定し、通常calleeの
frameを作らずhelperで処理し、frame復帰を終えてからcallerへ結果を配送する形とする。
長い往路と復路の両方を共有する必要がある。現在のportal routerを流用するだけでは、siteごとのresumeに
長い復路が残る。constant offset専用の要求や複数field処理ならmetadataを減らせる可能性がある。

global operandはFrameInstructionのBranch/Loop内にもあるため、その途中からdispatcherへ飛ばさない。
選択したアクセスだけをallocation/local reconstruction前の内部CFGでrequest/resumeへ分割するか、
同等のbackend出力計画を持つ。live temporary、破壊的condition、snapshot、評価順序を保持し、
helperのresume PCを通常関数のReturn PCと混同しない。近いアクセスは直接実行する選択肢を残す。

### その他のABI候補（保留）

| 候補 | 狙い | 再検討時の条件・費用 |
|---|---|---|
| global側の計算・frame-local cache | 同じ値の再搬送を減らす | callee effects、再帰、動的storeとのaliasを追跡し、call・abort・I/O等の観測点で同期する |
| transport専用lane / moving mailbox | 遠距離の反復搬送を隣接chunk間の搬送へ変える | tape増、stack flagとの共存、page移動、scratch初期化、pointer復帰を計上する |
| global側の固定dispatcher/context | global同士の操作を近くする | local・parameter・resultへのアクセスが遠くなる費用も計上する |
| runtime control metadataの分離 | globalへの走査量とlive data chunk数の依存を減らす | 元frameを特定してlocalへアクセスするlocator・搬送費用を含める |
| Return時のframe全域clearの削減 | dead storageのclearを省く | 次のCallのzero前提を見直し、entryでread-before-writeになるcellだけを初期化する解析が必要 |

専用laneとcode共有は独立した変更であり、共有helper化だけでstack scanが短くなるわけではない。
clearをReturnからCallへ移すだけでも削減にはならない。sourceの初期値0、共通gateのzero条件、
portal cleanup、再帰時の値分離を維持する。

### 再検討する場合の測定

同じCIR・入力・interpreter設定で、出力、入力消費、終了状態、global更新順序を比較する。
圧縮前/圧縮後BF bytes、raw/RLE/native命令数、実dispatcher訪問、frame chunks、stack scan steps、
parse/RSS/executeを分ける。実dispatcher訪問はcounters付きfixtureで確認する。
0/1/255、浅い/深いstack、再帰、aggregateのoverlap、複数field、early return/abortを含める。
単回sample時間の差だけを採否の根拠にせず、命令カウンタと反復測定を使う。

## Milestone 4: Branch、comparison、scalar template

### Branch

- condition値をbackupしない非同期分岐。
- branch結果をpointer位置で次のtemplateへ渡す方式。
- short-circuit `&&`と`||`でのboolean materialization削減。
- else flagやzero scratchの再利用。

### Comparison

- 両operandがdeadな場合の破壊的大小比較。
- operandがliveな場合のcopy variant。
- compare結果をboolean cellへ置かずbranchへ直接つなぐvariant。
- difference、`min`、`max`を後続処理と融合するvariant。

大小比較はunsigned 8-bitの全`256 * 256`入力について検証する。equal、0、255、wraparound付近を
少数の代表値だけで済ませない。

### Constant、I/O

- clearとconstant addの融合。
- 連続定数出力の共通base利用。
- 入力直前のdead write除去。
- lexerで頻出するdelimiter比較や文字分類との融合。

これらはfull self-host profileで十分な実時間比率が観測されたものから実装する。

## Milestone 5: Frame layoutとtemporary

profileとlivenessを用い、次を比較する。

- 頻繁に相互参照するframe slotの近接配置。
- parameter、return value、call temporaryの距離短縮。
- branch temporaryの生存期間に基づく再利用。
- portal temporaryの再利用。
- aggregate snapshotの必要最小range化。
- function固有frameとuniform frameの比較。

物理slot配置はsourceから観測できないが、portal region、outbox、ABI headerと重複してはならない。
再帰時に異なるactivation間でtemporaryがaliasしないことを検証する。

## Milestone 6: BF IR peephole

上位層の改善後に残る局所冗長性を対象にする。

- 既知zero cellへのclear除去。
- clearとAddのSet相当への統合。
- canonical linear transferの保持。
- loop前後の不要なpointer returnの除去。
- profile siteを越えて統合された命令のprovenance処理。
- 複数passが必要な場合のfixed-point条件とiteration上限。

peephole optimizationはtape boundaryのerror semanticsを不用意に変えない。現行と同様に、compilerが
生成する未最適化BFが境界を越えないという前提を使う場合は、その前提をAPI documentとtestに残す。

## Benchmark suite

### Microbenchmark

`bf-compiler`の専用exampleに、現行backendを使う次の測定を置く。

- dispatcher sizeとID分布。
- scalar call/returnと再帰depth。
- portal load/store、aggregate size、index分布。
- global accessとstack depth。
- branch body距離、conditionの0/nonzero分布。
- comparison全入力。
- `D = 16`。

### Repository workload

- 小さなscalar program。
- `test.bfc`。
- `fizzbuzz.bfc`。
- stage2 self-host internal test。
- stage2 production compilerでのinvalid input検査。
- stage2 compilerが生成したprogramの実行。
- full `scripts/verify-stage2-selfhost.sh`。

開発中は短いmicrobenchmarkとself-host smoke testを使い、merge前または節目で長いfull verificationを
実行する。

## 採用基準

すべての変更について次を満たす。

### Correctness

- old/new backendでprogram outputが一致する。
- expected error markerと停止条件が一致する。
- `D = 16`を検証する。
- bounded/unbounded tapeの両方を検証する。
- direct/mutual recursionとaggregate call/returnを含む。
- profile有無で実行する8種類のBF命令列が完全一致する。

### Performance

- markerなしrelease実行のwall time中央値を主指標にする。
- parse、fast IR build、executeの変化を別々に報告する。
- BF source bytes、raw/RLE steps、最大pointer、peak RSSも併記する。
- 一つの指標だけを改善して主要workloadのwall timeを悪化させる変更はdefaultにしない。
- target依存のtrade-offはbackend optionとしてPareto frontierを残す。

Milestone 1と2を終えた時点の最初の目標は、self-host internal testのend-to-end wall timeを少なくとも
2倍改善することである。達しない場合は、小さなpeepholeを積み重ねる前にprofile分類、interpreterの
fast IR認識、またはABI構造を再検討する。

### Portability

- template契約をRust固有のclosureや型だけで表現しない。
- self-host compilerで必要になるscratch数と状態遷移を明文化する。
- Rust版で安定したtemplateにはBFC側の対応予定または非対応理由を記録する。
- ABI versionを変更した場合は`ABI.md`とBFC側の定数・layoutを同じ変更で更新する。

## 実装単位

一つの変更で複数の主要templateを同時に置換しない。原則として次の単位で進める。

1. benchmarkまたはprofile site追加。
2. 独立probeとtemplate contract。
3. Rust backendの旧/new切替可能な実装。
4. exhaustiveまたはdifferential test。
5. repository workloadの測定。
6. default variantの決定。
7. 文書更新。
8. BFC実装への移植。

旧variantは新variantがfull self-host verificationを安定して通り、比較用途が不要になるまで残す。
