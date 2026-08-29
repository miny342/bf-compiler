# Brainfuck backend最適化メモ

この文書は、source language semanticsと現行ABI contractを変更しないbackend最適化、および今後の
候補を記録する。
隣接する移動・加算の統合、zero命令の除去、clear loopの標準化など、意味を局所的に判定できる
BF IR peephole最適化とcontinuation dispatcherの二段countdownは採用済みである。その他の高度な
lowering手法は未採用であり、実装時には生成BFの長さ、実行step、追加cell数を現在のloweringと
比較してから選ぶ。

## Repository interpreterのfast IR

セルフホスト開発中はcompilerが生成するBF自体を変更せず、`bf-interpreter`側で別のfast IRへ
変換して実行する。現在は次をnative operationとして認識する。

- 連続する同種の`+`、`-`、`<`、`>`を1回の加算またはpointer移動へまとめる。
- `[-]`、`[+]`を含む、奇数を加える単一run loopをcellのclearへ変換する。
- `[>>>]`や`[<<<]`をzero cellまで進むnative scanへ変換する。
- `[->>>++>+<<<<]`のように元のcellを1ずつ消費し、pointerが元へ戻るlinear loopを
  複数targetへのwrapping transferへ変換する。

fast IRはraw BF命令数と1反復あたりのRLE group数をmetadataとして保持する。このため、実行は
まとめても`RunStats::executed_instructions`と`executed_rle_instructions`には従来VMと同じ値を
加算する。移動runには元source offsetも保持し、tape underflow/overflowの診断位置を変えない。
最適化できないnested loop、I/Oを含むloop、境界を越えうるlinear transferは通常実行へfallbackする。

2026-08-27時点のstage 2 production compiler BFでは、1,115,639個のBF命令に対しleaf loopは
28,905個あり、clear 22,897個、linear transfer 4,442個、scan 1,566個だった。これら3形で
leaf loopをすべて分類でき、全命令の約94%はRLE対象文字だった。

stage 2のBFC内部test BFをrelease buildで実行した結果は次のとおりである。

| interpreter | wall time | raw換算実行命令数 | RLE換算実行命令数 |
|---|---:|---:|---:|
| 逐次VM | 47.51 s | 35,607,963,239 | 20,162,353,151 |
| fast IR | 1.84 s | 35,607,963,239 | 20,162,353,151 |

同じ環境で約25.8倍高速になった。動的なnative hit数は次で確認できる。

```console
bf-interpreter --stats program.bf < input
```

これはセルフホスト検証を高速化するinterpreter実装であり、compiler backendのcost modelや生成BFを
変更するものではない。

## 現在のpeephole最適化と基準値

`compile`と`compile_continuations`は、未最適化BF IRへ`optimize_bf`を適用してから文字列化する。
未加工のIRは`lower`または`lower_continuations`で取得できる。次のコマンドは標準入力をprogramへ
渡し、最適化前後の出力が一致することを検査したうえで、静的・動的な指標を表示する。

```console
cargo run -p bf-compiler --example profile_bf_ir -- test.bfc
```

2026-08-24、default ABI (`D = 16`)、入力なしでの基準値は次のとおりである。

| program | 指標 | 最適化前 | 最適化後 | 削減率 |
|---|---:|---:|---:|---:|
| `test.bfc` | BF source bytes | 868,629 | 275,975 | 68.23% |
|  | 実行BF命令数 | 309,078,573 | 247,137,037 | 20.04% |
|  | RLE型推定命令数 | 162,438,386 | 157,037,145 | 3.33% |
| `fizzbuzz.bfc` | BF source bytes | 121,746 | 39,516 | 67.54% |
|  | 実行BF命令数 | 2,644,141,255 | 1,770,938,111 | 33.02% |
|  | RLE型推定命令数 | 1,515,754,326 | 1,456,578,134 | 3.90% |

両programとも最大tape位置は変化しない（`test.bfc`: 1,241、`fizzbuzz.bfc`: 203）。このpassは
生成サイズと1文字ずつ解釈する処理系には大きく効く一方、同種命令をまとめるRLE型targetへの
実行時効果は小さい。今後のtemplate最適化ではRLE型推定命令数も主要な判断材料にする。

## 二段dispatcher

2026-08-29、Rust backendのdispatcherをcontinuation IDのhigh byteでpage分けした。dispatch cycleでは
まず存在するhigh-byte pageを選び、一致したpageのlow-byte caseだけを選ぶ。user continuation、
portal accessor、portal resumeは同じpage表へ入る。これによりcase数を`C`、存在するpage数を`P`と
すると、各caseでlow/highを比較する`O(C)`の全件走査を避けられる。連続するhigh-byte pageとpage内の
連続するlow byteには破壊的countdownを使い、疎な集合だけequality scanへfallbackする。profile mapには
`abi.dispatch.page.*` siteも出力する。

case bodyはcall、return、portal処理によって別contextへdata pointerを移せる。移動先contextの`PcLow`
はdispatch cycle末まで0なので、low byteが0のcaseをpageの先頭へ置き、移動後に`xx00`を誤dispatch
しないことをtemplateの不変条件とする。`0x0100`を含む複数page、callによるcontext移動、D=8/16を
backend testで検査する。

`fizzbuzz.bfc`をrelease buildし、repository fast interpreterでwarm-up後3回の中央値を測った結果は
次のとおりだった。入力なし、出力は最適化前後で一致した。

| 指標 | 変更前 | 二段dispatcher | 削減率 |
|---|---:|---:|---:|
| BF source bytes | 39,516 | 36,867 | 6.70% |
| 実行BF命令数 | 1,770,938,111 | 1,660,011,088 | 6.26% |
| RLE型推定命令数 | 1,456,578,134 | 1,329,168,036 | 8.75% |
| fast IR native operations | 103,829,946 | 73,382,252 | 29.33% |
| execute wall time | 375.76 ms | 300.57 ms | 20.01% |

最大tape位置は203のままだった。`logs/tmp.bfc`から生成したBF sourceは、文書化済みの変更前
53,238,909 byteに対して53,033,032 byte（0.39%減）だった。約3000秒かかるfull profile実行は
この変更の開発時検証では再実行していない。

### 二段の破壊的countdown

同日、まずpage内のlow-byte equality scanを破壊的な多段switchへ置換した。page内の最小low byteを
`PcLow`から一度引いて0始まりへ正規化し、nested loopを1段進むごとに`PcLow`を1減らす。0へ到達した
段のcaseだけが共通`Branch` flagを消費して実行される。比較用cellへのcopy/restoreとcaseごとの定数比較は
行わない。範囲外の値は最深部で`PcLow`とflagをclearするため、別caseへaliasしない。

countdownはpage内のlow byteが連続している場合だけ選択する。公開Continuation IRは任意のu16 IDを許すため、
たとえばlow byteが0と255だけの疎なpageへ256段のloopを生成すると退行する。この場合は従来のequality
scanへfallbackし、`abi.dispatch.page.compare`としてprofileする。通常のfrontendとhidden portal ID allocatorが
作る密なID列はcountdownを選択する。

case bodyがcall、return、portalによってcontextを移動した場合、移動先では`PcLow`と`Branch`が0である。
このため残りのnested loopとcaseは実行されず、従来dispatcherと同じくdispatch cycle末に`NextPc`を
新しい`Pc`へ移す。entryでは`PcHigh`が選択済み、exitでは`PcLow=0`、`PcHigh=0`、`Branch=0`、pointerは
current context originというtemplate契約になる。

直前の二段equality dispatcherと同条件で`fizzbuzz.bfc`を比較した結果は次のとおりだった。wall timeは
同一release binaryを交互に実行し、各artifactのwarm-up後3回の中央値を使用した。

| 指標 | 二段equality | countdown | 削減率 |
|---|---:|---:|---:|
| BF source bytes | 36,867 | 33,034 | 10.40% |
| 実行BF命令数 | 1,660,011,088 | 108,371,016 | 93.47% |
| RLE型推定命令数 | 1,329,168,036 | 25,072,890 | 98.11% |
| fast IR native operations | 73,382,252 | 7,881,310 | 89.26% |
| execute wall time | 300.25 ms | 18.25 ms | 93.92% |

wall timeは16.45倍高速化し、最大tape位置は203のままだった。

続いて同じ方式を`PcHigh`にも適用した。存在するhigh-byte pageが連続している場合は最小high byteで
正規化してpageを破壊的にcountdownし、疎なpage集合には従来のequality scanを残す。選択したpageのbodyが
contextを移動すると、移動先の`PcHigh`と`Branch`は0なので残りのpage gateは実行されない。連続する
`0x01xx`、`0x02xx`、`0x03xx`と、疎な`0x01xx`、`0xffxx`のprofile siteをbackend testで検査する。

low-byte countdownだけの版と同条件で`fizzbuzz.bfc`を比較した結果は次のとおりだった。

| 指標 | lowのみ | high + low | 削減率 |
|---|---:|---:|---:|
| BF source bytes | 33,034 | 33,001 | 0.10% |
| 実行BF命令数 | 108,371,016 | 107,808,472 | 0.52% |
| RLE型推定命令数 | 25,072,890 | 24,299,392 | 3.08% |
| fast IR native operations | 7,881,310 | 7,107,812 | 9.81% |
| execute wall time | 18.490 ms | 16.745 ms | 9.44% |

最大tape位置は203のままで、wall timeはさらに1.10倍高速化した。`logs/tmp.bfc`の生成BF sourceは、
二段equality版の53,033,032 byteから最終的に52,633,798 byteへ0.75%縮小した。

profilingではhigh-byte countdownを`abi.dispatch.pages.countdown`、疎なhigh-byte集合のfallbackを
`abi.dispatch.pages.compare`、その中のhigh-byte照合を`abi.dispatch.page.select`、low-byte countdownを
`abi.dispatch.page.countdown`、疎なpage内のfallbackを`abi.dispatch.page.compare`、case gateとbodyを
従来どおり`abi.dispatch.case.*`へ分離した。
exact modeの`percent`はclock readを含むexecute wall time基準なので、帰属できた時間だけを分母にする
`attributed_percent`もtext/JSON reportへ追加した。

2026-08-30、BFC製第5段階compilerのABI backendにもhigh/low二段countdownを移植した。selfhost IRの
`NodeId`はAST・命令と同じarena上の疎なaddressなので、continuation recordへ1始まりの密な16-bit
dispatch IDを追加し、PCにはこのIDだけを使う。call先は移動先contextの`NextPc`へ設定し、dispatch
cycle末までは`Pc=0`を保つ。BFC compiler自身のglobal navigationを増やさないよう、frame幅とarenaから
読んだID・list cursorはloopの外またはlocalへcacheする。

### full self-host exact profile

`logs/tmp.bfc`をcontinuation granularityでcompileし、生成した52,633,798 byteのBFをexact modeで1回
full self-host実行した。出力は`ok\n`、最大tape位置は26,390、executeは135.41 s、process全体は
137.20 sだった。reportはrepository rootの`tmp.log`へ保存した。exact modeでは5,749,199,982回の
clock readを行うため、execute wall timeのうちsiteへ帰属した時間は78.76 sである。

`abi.dispatcher`のinclusive timeは78.76 s（attributed 100%）だが、これは関数本体をdispatcher siteの
子として記録するprofile treeの構造による。dispatcher直下の関数実行までdispatch overheadと数えては
ならない。dispatcher本体、high/page/case gateのexclusive timeを合計すると11.85 s、帰属時間の
15.04%だった。内訳ではhigh-byte countdownは0.45 s（0.58%）、low-byte countdownは9.13 s
（11.59%）であり、PcHighは主要な残存bottleneckではない。

最大の単一workloadは`function.67.continuation.1177`のinclusive 29.03 s（36.85%）で、その中の
inner `abi.frame.branch`だけでexclusive 14.47 s（21.65%）を占めた。次はhigh-byte dispatchではなく、
low-byte ID配置/countdownと、このloop/branch loweringを優先して調べる。

### dynamic projectionのcarry除去

上記の`function.67`はstage 2 compilerの`allocate_cells`であり、最大のbranchは
`arena_cells[arena_next.page][arena_next.slot]`のflat offset計算から発生していた。従来はdynamic indexを
1ずつ消費してstrideをlow/high byteへ加え、low byteの加算ごとにwrapをbranchで判定していた。
`[16][256]`ではpage indexのstrideが256、slot indexのstrideが1なので、validなindexについてはどちらも
low byteのcarry判定が不要であるにもかかわらず、汎用templateを使っていた。

projection loweringでは、HIRが保持するarray lengthとelement strideから、その時点のlow offset byteの
最大値を追跡するようにした。source-levelで範囲外indexはundefined behaviorなので、valid indexの上限は
`length - 1`と仮定できる。`maximum_low + (length - 1) * (stride & 0xff) <= 255`を証明できる場合は、
index cellを1回の`Transfer`でlow/high byteへ直接分配する。証明できない場合は従来のcarry付きtemplateへ
fallbackする。これにより`[16][256]`ではpageをhigh byteへ、slotをlow byteへ直接加算できる。

併せて、else bodyが空のContinuation IR `Branch`はbranch flagを確保せず、condition cell自身をthen armの
gateとして使うABI templateへ変更した。then bodyがconditionを書き戻しても破壊的branch契約を守るため、
bodyの前後でconditionをclearする。これは単独では実時間差が測定noiseの範囲だったが、生成量とbranch用
frame slotを減らし、carry付きfallbackにも適用できる。

同じrelease binaryとrepository fast interpreterで`logs/tmp.bfc`を入力なしで実行した結果は次のとおり。
出力は変更前後とも`ok\n`で、wall timeはwarm実行の中央値を使用した。

| 指標 | 変更前 | carry除去後 | 削減率 |
|---|---:|---:|---:|
| BF source bytes | 52,633,798 | 52,542,234 | 0.17% |
| 実行BF命令数 | 478,746,533,739 | 253,696,567,820 | 47.01% |
| RLE型推定命令数 | 197,613,590,147 | 19,385,331,743 | 90.19% |
| fast IR native operations | 2,611,618,596 | 1,023,292,188 | 60.82% |
| execute wall time | 29.045 s | 22.226 s | 23.48% |

最適化後にinstruction granularityの1 ms sample profileを取ると、`abi.frame.branch`のexclusive推定時間は
0.381 s（executeの1.45%）まで下がった。次の最大項目は`abi.navigation.global`の19.779 s（75.41%）である。
特に`function.161.continuation.3068`内の2個のglobal navigationが合わせて約9.96 sを占めるため、次の
backend最適化ではglobal location間transferの移動距離と配置を調べる。

### scalar global copyのnibble搬送

最適化後profileの`function.161`はtest harnessの`capture_compiler_output`であり、global scalar
`test_capture_length`を保存しながらframe temporaryへ読む2個の`Transfer`が熱点だった。従来の
non-destructive copyはsourceをdestinationとrestoreへ破壊的transferし、restoreからsourceへ戻す。
sourceまたはdestinationがglobalの場合、byteを1減らす各反復でcurrent contextからanchorまでのstackを
往復するため、値255のcopyはstack scanを数百回実行する。

sourceを保存する操作を`FrameInstruction::Copy`としてContinuation IRに残し、ABI loweringまで
`Transfer`列へ分解しないようにした。D=16のstatic layoutはscalar globalsとglobal aggregatesの間に
9個の共有scratch cellを確保する。scalar globalからframeへのcopyではsourceをstatic側で8 bitへ分解し、
それをlow/high nibble counterへ畳む。各counterを消費しながらsourceを復元し、同じ1または16をframe側へ
加える。これによりstack往復回数はsource値に比例する二重transferから、2 nibbleの和（最大30）に制限
される。scratchはcopyのentry/exitでzero、sourceは保存、destinationは上書きされる。D=8は9 cellを
確保できないため従来templateへfallbackする。

8個のbitを個別に搬送する版は往復を最大8回にでき、executeは約6.38 sまで短縮したが、navigation
templateを8組展開するためBF sourceが92,693,059 byteへ76.42%増加した。nibble版は約4%遅い一方、
生成量をほぼ維持できるためこちらを採用した。carry除去後のartifactとの比較は次のとおり。

| 指標 | carry除去後 | nibble copy | 増減 |
|---|---:|---:|---:|
| BF source bytes | 52,542,234 | 53,170,123 | +1.19% |
| 実行BF命令数 | 253,696,567,820 | 102,715,459,925 | -59.51% |
| RLE型推定命令数 | 19,385,331,743 | 14,398,370,711 | -25.72% |
| RLE operations | 5,180,773,955 | 2,673,971,876 | -48.39% |
| scan steps | 4,576,378,022 | 2,076,876,082 | -54.62% |
| execute wall time | 22.226 s | 10.629 s | -52.18% |

最大tape位置は26,339から26,229へ減った。`Copy`がlowering時のrestore temporaryを不要にし、複数の
function frameが縮小した効果も含む。global accessを無条件に高速化するものではなく、scalar globalから
frameへの明示的copyだけを選択する。global aggregate portalやframeからglobalへのcopyは従来templateを
使うため、生成量とのtrade-offを個別に測定してから適用範囲を広げる。
最適化後の1 ms sample profileでは`abi.navigation.global`は8.272 s相当まで減ったが、execute推定時間の
59.27%で依然最大だった。次はglobal portalのfield設定batch化、逆方向copy、frame compactionを比較する。

### hot global aggregateのローカル保持

残ったglobal navigationの大部分は、stage 2 compilerの`allocate_cells`が2 cellのglobal
`arena_next`をloop内で繰り返し読んで更新する箇所だった。`arena_advance`は渡された値だけを更新し、
`fail`は復帰せず、loop中に`arena_next`を観測する別の呼び出しもない。このため関数入口で
`arena_next`をローカル`position`へ読み、loopをローカルだけで実行し、正常終了時に一度だけ書き戻すようにした。

同じRust compilerと同じstage 2 self-test入力で、変更前後を生成し直した結果は次の通りである。
実行時間はwarm-up後3回の中央値を使った。

| 指標 | loop内global read/write | local保持 | 変化 |
| --- | ---: | ---: | ---: |
| 生成BF bytes | 53,170,123 | 53,101,153 | -0.13% |
| 実行BF命令数 | 102,715,459,925 | 72,185,235,147 | -29.72% |
| RLE型推定命令数 | 14,398,370,711 | 12,133,620,872 | -15.73% |
| RLE operations | 2,673,971,876 | 1,525,890,459 | -42.94% |
| scan steps | 2,076,876,082 | 948,925,708 | -54.31% |
| execute wall time | 10.580 s | 6.102 s | -42.32% |

これはglobal aggregate copy全般をbackendで特殊化するより、値が変わらない範囲をsourceで明示してglobal access
自体をloopから外す方が大きく効く例である。一般の関数呼び出しをまたぐglobal cacheはalias/effect解析が必要だが、
このように呼び出し先が対象globalを観測しないと確認できるhot loopでは、まずローカル保持を優先する。

主な調査元は、angel_p_57氏の
[Brainf**k記事一覧](https://zenn.dev/angel_p_57/articles/40838978dcaf7b)である。記事中の
記号付きBFは説明用のコメントを含むため、そのままcompilerへ埋め込まず、entry/exit条件を
持つlowering templateとして実装する。

## 最優先で試す候補

### 非破壊的な非同期分岐

[非破壊的条件分岐](https://zenn.dev/angel_p_57/articles/afcae170f7311a)では、次の2方式が
整理されている。

- 条件値を評価用cellと保存用cellへ複製し、評価後に保存値を戻す。
- `[`に入ったcellとは別のcellで`]`を評価する非同期的な分岐と、必ず存在するelse側の
  経路を組み合わせ、条件値を消費せずに最終pointer位置を揃える。

後者は条件値のbackup cellの代わりに、経路を合流させるためのzero cellを要求する。
現在のcontinuation dispatcherとarray accessorもpointer位置の移動を積極的に利用するため、
次の箇所で候補になる。

- `Branch`と短絡評価のlowering。
- dispatcherのcase判定と、array portalからresume continuationへ戻る判定。
- 条件値が後続でもliveな場合の、copy/restoreの省略。

ただし、これはCell IRの`Branch`の意味を変える機能ではない。BF lowering内で両経路の
終了pointer位置を静的に証明できる場合だけ選択する。通常のbackup方式もfallbackとして残す。

### 破壊的な大小比較と差分の同時計算

[効率のよいアルゴリズム](https://zenn.dev/angel_p_57/articles/2d6f2f36eb235a)の大小比較は、
2値を同時に1ずつ減らし、一方が先に0になった時の終了cellの違いで大小を表す。同時に
絶対差に相当する値が残るため、比較後に差、`min`、`max`を使う場合は処理を融合できる。

source-levelの比較演算へは次の規則で使える。

- 両operandが比較後にdeadなら、operandを直接消費する最短形を選べる。
- operandがliveなら、一時cellへcopyした値を消費する。元値を壊してはならない。
- 比較と差、`min`、`max`の複数結果が直後に必要なら、保存cellを追加するvariantと後続処理を
  融合し、別々に再計算しない。
- 終了pointer位置自体が比較結果を表すので、直ちにbranchへつなぐtemplateとして扱う。
  booleanの0/1へ一度materializeする形と、branchへ直接loweringする形を別々に測る。

この方式はunsigned 8-bit値の大小比較に適する。wrapping subtractionの符号だけを見て
比較する方式ではないため、`0`, `255`, equalを含む全`256 * 256`組で検証する。

### 固定除数用divmod

[divmodアルゴリズムと数値出力](https://zenn.dev/angel_p_57/articles/d5bd4cf2d32168)は、被除数を
1ずつ消費しつつ、除数counterと余りを周期的に回して商と余りを同時に得る実装を示している。
さらに[効率のよいアルゴリズム](https://zenn.dev/angel_p_57/articles/2d6f2f36eb235a)には、除数を
codeへ埋め込み、除数cellを省く固定除数版がある。

array accessorの`index / D`と`index % D`では`D`がcompile-time定数であり、defaultは16、
互換構成は8である。そのため固定除数版は特に相性がよい。記事の固定除数版は通常の商・余りと
値の向きや更新時点が異なるので、canonicalな`(q, r)`へ補正してからdispatchする案だけでなく、
その表現のままpage/slot countdownへ融合する案も比較する。

除数1への対応や任意除数版は、一般の`/`、`%`演算を実装するときの候補として分離する。
array用templateは`D = 8`と`D = 16`の全入力についてのみ保証すればよい。

### スライド型loopとmoving-index accessor

[スライド型ループ](https://zenn.dev/angel_p_57/articles/d0c8c51b244cc8)は、対になる`[`と`]`が
同じ物理cellを使う必要はないことを利用し、loopの継続判定位置を反復ごとに移動する。
記事ではsentinelまでの走査、marker jump、指定距離の移動を例示している。

これは次の最適化候補になる。

- 二段静的array dispatchを、indexを運びながらchunkまたはelement間を移動する
  moving-index accessorへ置換する。
- stack flag列、global anchor、array境界のsentinel scanを短縮する。
- dispatcherの現在位置とcounterを一緒に隣のslotへ送る。

任意の0値要素をsentinelと誤認しないよう、走査対象はABIがmarker/flagと定めたcellに限る。
配列payloadの内容には依存させない。

### 多段分岐とdispatcher

[条件分岐の多段化](https://zenn.dev/angel_p_57/articles/b409d3e760d99b)は、判定値を1ずつ減らし、
共通のelse用flagを使い回してswitch相当を構成する。continuation IDやarrayのpage/slotは
密な非負整数なので比較した結果、high byteでpageを選んでからlow byteを破壊的に減らす二段countdownを
Rust backendへ採用した。詳細と測定値は「二段dispatcher」の節に記録している。残る比較対象は次になる。

- page内の小さなgapまでcountdownを許すcost model。
- 頻度の高いcontinuationへ小さいIDを割り当てる配置。

一般のswitch loweringにも使えるが、疎な値では減算回数が増える。密度とprofileを見て
照合型、countdown型を選択する。

## 次段階の候補

### 二進多cell値

同じ[効率のよいアルゴリズム](https://zenn.dev/angel_p_57/articles/2d6f2f36eb235a)には、carry/borrowを
「最初に見つかる0または1までのslide」として処理する二進increment/decrementがある。
16-bitを固定したPCには現在の2-cell表現で十分だが、次の場合に再検討する。

- 256を超える配列indexや、より広いstack pointerをruntime値にする。
- 任意長整数を言語へ追加する。
- increment/decrement主体のcounterで、基数256のcarry処理より二進cell列が有利になる。

[BCD演算の記事](https://zenn.dev/angel_p_57/articles/0dabf335693675)は、任意長decimal値や
decimal I/Oを言語機能へ追加した場合の候補とする。version 0の8-bit `cell`には導入しない。

### 入出力と定数生成の融合

[AtCoderでの頻出処理](https://zenn.dev/angel_p_57/articles/7c8c9b9127fb29)には、複数の出力候補を
互い違いに配置して分岐結果のpointer位置から直接走査する方法、複数定数の共通部分をloopで
一括生成する方法、delimiter判定と十進入力の蓄積を融合する方法がある。

これらは標準libraryまたはpeephole optimizationの候補にする。特に定数文字列出力は、文字ごとに
独立生成する方式、共通baseから差分生成する方式、分岐候補をinterleaveする方式をcode sizeと
step数の両方で比較する。

### BF上のruntime VMを作る場合

[セルフインタプリタ](https://zenn.dev/angel_p_57/articles/ba6f70caa7ce41)では、命令列を0から7の
数値として1cellおきに置き、IPを命令領域とdata領域の間で運び、括弧探索用の可変長counterを
静的領域と反対方向へ伸ばしている。

現在のABIは全continuationをcompile-timeに生成するので、この構成を直接採用しない。将来、生成BFの
code sizeを抑えるためにbytecode interpreter型backendを追加する場合だけ、次を参考にする。

- code/dataを伸長方向の異なる領域へ置くlayout。
- IP位置を継続flagとして兼用する走査。
- 命令cellの間のzero cellをscratchとして再利用する表現。
- 対応する括弧を事前対応表なしで探す場合の可変長nest counter。

## cost model上の注意

[AtCoderでの頻出処理](https://zenn.dev/angel_p_57/articles/7c8c9b9127fb29)と
[処理限界の記事](https://zenn.dev/angel_p_57/articles/d7e2f3529c801d)では、対象処理系が連続する
同種の`+ - < >`をまとめて1 stepとして数えることを利用し、code sizeを増やして長距離移動の
step数を減らしている。この性質はBrainfuck全体の保証ではなく、target依存である。

最適化の評価値は混同せず、少なくとも次を別々に記録する。

- 生成BFのsource bytes。
- repositoryのinterpreterによる実行命令数。
- 連続する同種命令を1命令とするRLE型targetの推定step数。
- 使用した最大tape位置と追加scratch cell数。
- entry/exitのpointer位置と、終了時にzeroであることを要求するcell集合。

code sizeとstep数のPareto frontierを残し、特定処理系にだけ速いvariantはtarget optionで選ぶ。

## 実装・benchmark順序

1. 比較templateを独立probeにし、全8-bit入力で結果、元値の保存、終了pointer位置を検証する。
2. `Branch`のbackup版と非同期版を、条件0/nonzeroおよびbodyの距離を変えて比較する。
3. 固定除数divmodをarray probeへ追加し、`D = 8/16`、配列長16/100/256で測定する。
4. moving-index accessorと追加のpage countdown改善を個別に導入し、組合せ爆発を避ける。
5. 実programのprofileが得られてからcontinuation ID配置とtarget別cost modelを追加する。

最適化templateは、入力cellのlive/dead、必要な初期zero cell、破壊されるcell、各経路の終了pointer
位置を型またはmetadataとして宣言する。文字列断片だけを登録する方式にはしない。
