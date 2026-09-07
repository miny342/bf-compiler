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

## 長時間self-host profileのrunbook

長時間実行で局所的な初期phaseだけを見て最適化しないため、production compiler全体を自己入力する。
最初のfull runでは、CIRから生成するBFに`continuation` granularityのsidecar mapを付け、interpreterは
2 ms samplingで動かす。これならABI site、function、continuation contextを追いつつ、`counters`や
`exact`よりprofile overheadを抑えられる。`source` granularityは現時点ではsource spanをCIRへ保持して
いないため`instruction`と同じ粒度になる。

以下はrepository rootで実行する。artifactは巨大になるため、`/tmp`ではなく空き容量に余裕がある
永続領域を使う。ここでは既存の計測用directoryである`logs/`の下へ時刻つきdirectoryを作る。

```bash
cargo build --release \
  -p bf-compiler --bin bfc \
  -p bf-interpreter --bin bf-interpreter

run_dir="logs/full-selfhost-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$run_dir"

scripts/concat-stage2-compiler.sh cir > "$run_dir/stage2-cir-compiler.bfc"
scripts/concat-stage2-compiler.sh main > "$run_dir/stage2-compiler.bfc"

target/release/bfc --run-ir --ir-progress-interval 15s \
  "$run_dir/stage2-cir-compiler.bfc" \
  < "$run_dir/stage2-compiler.bfc" \
  > "$run_dir/stage2-compiler.cir" \
  2> >(tee "$run_dir/cir-generation.metrics" >&2)

target/release/bfc --cir-input "$run_dir/stage2-compiler.cir" \
  --unlimited-tape \
  --profile-map-output "$run_dir/stage2-compiler.bfmap.json" \
  --profile-granularity continuation \
  > "$run_dir/stage2-compiler.bf" \
  2> >(tee "$run_dir/backend.metrics" >&2)
```

`--run-ir`はBFC製CIR frontendをRustのContinuation IR runnerで実行する。CIRはstdoutへ逐次出力され、
`--ir-progress-interval`のsnapshotには現在のcontinuation、実行instruction/loop/call、出力byte数、
RSS/HWM/VMが出る。次の`--cir-input`はcompact binary CIRを検証・decodeし、Rust ABI backendからBFを
streaming出力する。profile sidecarはBF本体の命令数とhashを含むため、別のBFと組み合わせると
interpreterが拒否する。BFを再生成したらsidecarも必ず同時に再生成する。

長時間実行の前に、小さいsourceでBFとsidecarの組が使えることを確認する。

```bash
target/release/bf-interpreter \
  --unlimited-tape \
  --progress-interval 5s \
  --profile-map "$run_dir/stage2-compiler.bfmap.json" \
  --profile-mode sample \
  --profile-sample-interval 2ms \
  --profile-output "$run_dir/hello.profile.json" \
  --profile-format json \
  "$run_dir/stage2-compiler.bf" \
  < selfhost/stage2/examples/hello.bfc \
  > "$run_dir/hello.bf"

target/release/bf-interpreter --unlimited-tape --no-progress \
  "$run_dir/hello.bf"
```

最後の出力が`A!`と改行になることを確認してから、full self-hostを実行する。次の例は12時間で
SIGINTを送り、その後30秒応答がなければ強制終了する。subshellのvirtual-memory上限6 GiBは、以前の
数十GiBへの暴走でWSL全体をOOMにすることを避けるための安全弁であり、正当な実行が上限へ達した場合は
`ulimit`値を上げる。profileを付けた現行artifactの想定使用量そのものを示す値ではない。

```bash
(
  ulimit -Sv 6291456
  exec timeout --foreground --signal=INT --kill-after=30s 12h \
    target/release/bf-interpreter \
      --unlimited-tape \
      --progress-interval 60s \
      --profile-map "$run_dir/stage2-compiler.bfmap.json" \
      --profile-mode sample \
      --profile-sample-interval 2ms \
      --profile-output "$run_dir/full.profile.json" \
      --profile-format json \
      "$run_dir/stage2-compiler.bf"
) < "$run_dir/stage2-compiler.bfc" \
  > "$run_dir/stage3-compiler.bf" \
  2> >(tee "$run_dir/full.metrics" >&2)
```

別terminalでは次を使える。`bf-progress`はelapsed time、現在site/key/context、入力・出力byte数、native
operation、RLE換算instruction、pointer、最大pointer、RSS/HWM/VMを表示し、続く`bf-progress-hot`は
その時点までの累積上位10 siteを表示する。

```bash
tail -f "$run_dir/full.metrics"
watch -n 10 'ps -C bf-interpreter -o pid,etime,%cpu,rss,vsz,cmd'
```

interpreterは入力と実行中の出力をmemoryへ保持し、正常終了時にstdoutをまとめて書く。そのため
`stage3-compiler.bf`のfile sizeが実行中に増えなくても停止とは限らず、`output_bytes`を見る。最初の
Ctrl+C、または上記timeoutのSIGINTではsafe observation pointで`reason=interrupt`の最終snapshotを
stderrへ出してexit code 130で終了する。2回目のCtrl+Cは即座にexitする。

`full.profile.json`は正常終了してstdoutを書き終えた後にだけ生成される。中断時に残るprofile情報は
`full.metrics`の累積snapshotだけであり、checkpoint/resume機能はない。このため、途中snapshotはphaseの
診断には使えるが、globalな最適化判断には完走したJSONを使う。

完走後はartifactを記録し、ABI stable keyを全context横断で合計する。単一siteの上位だけでなく、特に
`abi.navigation.global`、`abi.dispatch.page.countdown`、`abi.portal.offset`などの合計を比較することで、
同じglobal処理が多数のcontinuationへ分散した場合も見落とさない。

```bash
sha256sum "$run_dir/stage2-compiler.bf" "$run_dir/stage3-compiler.bf"

jq '.phase_timings_ns, .run_stats, .profile.total_samples' \
  "$run_dir/full.profile.json"

jq -r '
  .profile.sites
  | group_by(.stable_key)
  | map({key: .[0].stable_key, samples: (map(.samples) | add)})
  | sort_by(.samples) | reverse | .[:30][]
  | [.samples, .key] | @tsv
' "$run_dir/full.profile.json"
```

2 ms sampleで20 sample未満のsiteはJSON上で`low_confidence: true`になる。候補をfull runで絞った後だけ、
小さい再現入力または絞った比較で1 ms sampleや`exact`を使う。`counters`は全profile blockのcounter更新、
`exact`はsite境界ごとのclock readを行うため、最初のfull self-host比較には使わない。

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

### 低番号global aggregateのanchor近傍配置

self-host CIR版full compilerを2 ms samplingしたところ、`abi.navigation.global`が最大siteで、そのうち
`abi.portal.router.global.2`が全sampleのおよそ38%を占めた。CIR adapterはbump allocatorが使う低い
logical rangeからGlobalId 0, 1, 2, ...を割り当てる一方、static layoutも同じ順にlow address側から置いていた。
そのため最も頻繁に使う低番号regionのportal headがanchorから遠くなっていた。

aggregateの物理配置だけをGlobalId逆順にし、scalar globals、論理GlobalId、portal protocolは変更しない
A/Bを行った。full compilerでの結果は次のとおりである。

| 指標 | GlobalId昇順配置 | GlobalId逆順配置 | 変化 |
| --- | ---: | ---: | ---: |
| 生成BF bytes | 916,810,945 | 727,308,936 | -20.67% |
| interpreter RSS（sample実行） | 1,211,320 KiB | 1,026,232 KiB | -15.28% |
| BF parse（3回平均） | 2.737 s | 2.167 s | -20.82% |
| `hello.bfc` compile execute（3回平均） | 0.634 s | 0.635 s | ほぼ同じ |

repository interpreterは連続pointer moveを一つのnative operationへ畳むため、静的距離の短縮はexecute時間に
ほぼ現れなかった。4秒時点のself-host進捗も両配置とも入力1369 bytes、約588M native operationsだった。
一方、生成量、load/parse、RSSにはそのまま効く。素朴なBF interpreterでは長距離move自体も短くなるため、
実行時間改善も期待できる。mirror portalや有限stackを導入する前の低riskなlayout改善として採用する。

### global portal routeのnibble搬送

逆順配置後のfull compiler sampleでは、`abi.portal.router.global.2`内の`abi.navigation.global`がなお
全sampleの約42%を占めた。global portal routerはframe上にstageしたindex、payload、accessor PC、resume PCの
7 bytesをportalへ移す。従来templateは各byteを1ずつ減らし、1 unitごとにlive stackを越えてglobalへ往復するため、
low byteが200台なら同じstack scanを200回以上行っていた。

D=16では7 protocol cellsが1 chunkを占有し、同じchunk内の残り9 lanesは未使用だった。この9 cellsをcarryと
8 binary digitsに使い、route byteをbit分解してlow/high nibble counterへ畳み、1または16ずつportalへ加える。
stack往復はbyte値（最大255）から2 nibbleの和（最大30）へ制限される。scratchは各搬送のentry/exitでzero、
route sourceは破壊され、zeroであるportal destinationへ値が移る。D=8はscratchを追加せず、従来の7-cell routeと
unary templateを維持する。

nibble搬送は長いnavigation loop bodyを2組生成するので、7 fieldsすべてに適用するとBF sourceが46.0%増えた。
full compilerで適用範囲をA/Bし、動的なlow byteであるindex、accessor PC、resume PCの3本だけをnibble化した。
payloadはload時に0であり、high bytesは通常小さいため、これら4本は生成量に対する効果が小さかった。

| variant | BF source bytes | `hello.bfc` execute | full入力bytes（6秒） | sample RSS |
| --- | ---: | ---: | ---: | ---: |
| unary 7本 | 727,308,936 | 630 ms | 1,585 | 1,026,192 KiB |
| nibble 7本 | 1,062,162,707 | 399 ms | 1,991 | 1,360,104 KiB |
| nibble 4本（low + payload） | 918,655,064 | 410 ms | 1,955 | 1,216,888 KiB |
| nibble 3本（low controlのみ） | 870,818,191 | 415 ms | 1,944 | 1,169,256 KiB |

採用した3本版は生成量を19.73%増やす一方、短いself-host compileのexecuteを34.21%短縮し、full compilerの
同一実行時間での入力処理を22.65%増やした。`global.2` navigationのsample比率も約42%から約12%へ低下し、
最大siteは`abi.dispatch.page.countdown`へ移った。生成量、parse、RSSを含む一回限りの短い実行では不利になり得るため、
全field版へ広げずこのPareto点を採用する。次はnested stack scanを含むtransferのinterpreter native化、または
low-byte continuation IDのprofile-guided配置を独立に比較する。

### portal hidden PCのpage内反転

3本nibble版のsampleでは、頻繁なportal accessor/resumeが論理ID `5856`, `5861`, `5862`にあり、low byteが
`224`, `229`, `230`だった。hidden dispatch entryはuser continuationの後ろへ割り当てられるため、最後の
部分pageでcountdown距離とroute PC搬送値がともに大きくなる。

論理ContinuationIdとprofile keyは維持し、ABIのPC fieldへ保存する物理low byteだけをpage内で符号化する。
hidden entryのascending countdown距離の合計よりdescending距離の合計が小さいpageだけ、occupied rangeを
反転する。dispatcher table、main/call/goto/branch target、portal accessor/resumeはすべて同じ全単射を使う。
hidden entryを持たないpageとhigh byteは変えない。

full compilerをsamplingなしで各10秒に制限し、execute開始後6秒のsnapshotを比較した。生成BFの差は
物理PC定数の符号化だけで、RSSも変わらなかった。

| 指標 | ascending PC | hidden優先反転 | 変化 |
| --- | ---: | ---: | ---: |
| BF source bytes | 870,818,191 | 870,818,118 | -73 bytes |
| `hello.bfc` compile execute | 433 ms | 393 ms | -9.11% |
| full入力bytes（6秒、samplingなし） | 1,999 | 2,079 | +4.00% |
| interpreter RSS | 1,134,056 KiB | 1,134,108 KiB | 実質同じ |

2 ms samplingでも6秒時点の入力は1,918から1,999 bytesへ4.22%増えた。BF sourceやmemoryを増やさず
改善が再現したため採用する。ただし最大siteは引き続き`abi.dispatch.page.countdown`であり、page全体の反転は
同じpage内のuser continuationを後方へ動かす。次にID配置を広げる場合は、Rust direct VMのcontinuation countを
用いた全entryの重み付きpermutationとして別途比較する。

### 固定chunk幅の16-bit portal divmod

portal accessorは16-bit offsetをchunk displacementとpayload remainderへ分ける。従来templateはlow byteを
1回ずつ、high byteを1 unitごとにさらに256回tickし、各tickでremainder counterの比較と16-bit quotientの
carry処理を行っていた。offset `H:L`の実行量は最大65,535 ticksになる。

ABIが許すchunk幅はD=8またはD=16だけなので、`D = 2^shift`として次を直接構成する。

```text
remainder    = L & (D - 1)
quotient_low = (L >> shift) | (H << (8 - shift))
quotient_high = H >> shift
```

low/high byteをそれぞれframe内の8 binary cellsへ分解し、必要なbitを重み付きtransferで既存の
`Index`, `Scratch1`, `Scratch2`へ畳む。dispatch中はzeroである`Pc*`, `NextPc*`と既存scratch fieldsを
再利用するため、frameやportal layoutへcellを追加しない。offset `0x1fff`を使う回帰をD=8/16双方へ追加し、
quotient high byteまで検証した。

hidden PC反転版をbaselineとして、full compilerをsamplingなしで10秒に制限した結果は次のとおり。

| 指標 | tick divmod | bit divmod | 変化 |
| --- | ---: | ---: | ---: |
| BF source bytes | 870,818,118 | 870,818,392 | +274 bytes |
| `hello.bfc` compile execute | 386 ms | 377 ms | -2.40% |
| full入力bytes（6秒、samplingなし） | 2,079 | 2,252 | +8.32% |
| interpreter RSS | 1,134,108 KiB | 1,133,884 KiB | 実質同じ |

2 ms samplingでは6秒時点の入力が1,999から2,144 bytesへ7.25%増え、従来約17%を占めた2つの
`abi.portal.offset`はtop 10から消えた。生成量とmemoryを実質維持して改善したため採用する。次のportal候補は、
別々に実行しているwindow right/access/window leftの移動回数削減である。

### D=16 portal windowのzero-lane swap省略

D=16では16-cell portalが1 chunkに収まり、window移動はportal chunkと隣接payload chunkのlaneごとのswapになる。
従来は全laneについて`payload -> temporary`, `portal -> payload`, `temporary -> portal`の3 transfersを行っていた。
offset計算後も`PcLow`, `PcHigh`, `NextPcLow`, `NextPcHigh`, `Branch`, `Scratch0`, `Scratch3`は必ずzeroである。
これらのlaneでは隣接payloadをzero portal laneへ直接moveすれば、移動先の新portal laneもzeroになるため1 transferで済む。
windowを戻す時点ではquotientを消費済みの`Scratch1`, `Scratch2`も同じ扱いにする。D=8の2-chunk portalは
一般のrotationを維持する。

固定D divmod版をbaselineとして、full compilerをsamplingなしで各10秒に制限した結果は次のとおり。

| 指標 | 全lane swap | zero-lane省略 | 変化 |
| --- | ---: | ---: | ---: |
| BF source bytes | 870,818,392 | 870,811,840 | -6,552 bytes |
| `hello.bfc` compile execute | 389 ms | 370 ms | -4.87% |
| full入力bytes（6秒） | 2,242 | 2,283 | +1.83% |
| native operations（6秒） | 1,241,180,675 | 1,196,466,300 | -3.60% |
| RLE換算命令（6秒） | 19,977,492,486 | 17,661,462,712 | -11.59% |
| interpreter RSS | 1,133,936 KiB | 1,133,624 KiB | 実質同じ |

D=8/16の全回帰、selfhostによる`hello.bfc`生成物のbyte一致、生成BFの`A!\n`出力を確認した。
生成量と実行量がともに改善するため採用する。

### D=16 portal windowの固定距離nibble jump

184,330-byteのproduction compilerをfull self-host samplingしたところ、1,980秒で入力42,552 byteまで
進んだ時点でもBF出力は始まらず、`abi.portal.window.right/left`が約99万sample中約64万、約65%を
占めた。従来のwindowは16-bit offsetから求めたchunk quotientを1ずつ消費し、D=16 portal chunkと
隣接payload chunkを右へswapする。access後は同じ回数だけ左へswapするため、最大4,095 chunkの距離が
そのまま実行回数になる。arenaの使用位置が増えるほど入力処理が遅くなった原因とも一致する。

D=16ではchunk quotientの12 bitを3個のnibbleとして保持し、1、16、256 chunk離れたpayload chunkとの
固定距離swapを各nibble値の回数だけ行うようにした。途中のpayload chunkは動かさず、portalが通った
位置だけが一時的に入れ替わる。payload access後に256、16、1の逆順で同じswapを行うと全配置が復元する。
移動回数は片道最大4,095回から`15 + 15 + 15 = 45`回になる。nibbleと帰路用copyは既存protocol fieldへ
置き、portal/frame layoutは増やさない。D=8は従来の隣接rotationを互換経路として残す。

offset `0x1fff`の回帰では、D=8の隣接walkが390,769 native operations、D=16の固定距離jumpが
20,989 operationsだった。さらに8,192-cell global aggregateの遠端へstore/loadし、途中の複数payload
sentinelが往復後も元の位置に残ることをD=8/16双方で確認した。

停止したfull runと同じ145,210-byte CIRからcompiler BFを再生成し、`stage7_globals.bfc`をコンパイルした。
executeはwarm実行3回の中央値、sample比率は別の2 ms sampling 1回である。

| 指標 | 隣接window walk | 固定距離nibble jump | 変化 |
| --- | ---: | ---: | ---: |
| full compiler BF bytes | 870,811,840 | 872,412,134 | +0.18% |
| fast IR native operations | 1,576,874,455 | 1,250,982,489 | -20.67% |
| execute wall time | 7.055 s | 5.919 s | -16.10% |
| window sample比率 | 17.49% | 2.80% | -14.69 points |

生成したtarget BFは変更前後でbyte一致した。full self-hostは再実行していないが、生成量を約1.6 MBだけ
増やして、停止runの最大bottleneckだったoffset比例のwindow往復をnibble値比例へ制限できるため採用する。

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


## 2026-09-07: dense continuationのdispatchページ幅を均衡化

`logs/full-selfhost-20260902-233738/` と `logs/full-selfhost-20260907-184903/` の
`stage2-compiler.cir`、`stage2-compiler.bf`、自己入力の `stage2-compiler.bfc` は
それぞれSHA-256が一致した。
BFのhashは `fd2f2b68854f8b50e5eae622a2dc6ddc27aec71af671eeeed735a928ead24619`。
runbookのBFC frontend → CIR入力の経路には、Rust HIRのinline化・frame再利用が適用されない。
この2本の時間差は生成コードの退行を示さない。

今回の変更は共通Rust ABI backendの `DispatchEncoding` に入れた。
hidden portal entryを含むIDが `1..=N` に密で、N > 256なら、portal entryを先に、
通常の継続を後に、それぞれ論理ID順に並べる。そのrankをページ幅
`ceil(sqrt(N + 1))` で2 byteへ分ける。論理ID・CIR形式は変更しない。
既存のhidden entryを優先する低位byte反転も、新しいページ単位で適用する。
疎な公開IRと256 entry以下のprogramは元の配置を使う。

countdownは高位・低位とも線形なので、従来の約 `N/256 + 256` 段を
約 `2*sqrt(N)` 段へ均衡化できる。これは実行頻度に依存しない探索段数の改善であり、
各継続の頻度を使った最適配置ではない。高位ページのcountdownが増える継続もある。
production compilerのページ幅は256から77になる。frame layoutや追加scratchは変更しない。
portal PCの高位byteはglobal/frame間を値の回数だけ移動して転送する（低位は既存の
nibble転送を使う）。dispatch距離だけでなくPCのbyte値もcostになるため、portal entryを
先に置き、とくに高位byteを小さくして転送costを抑える。

分岐統合も調査した。元CIRをadapterへ通した5,854継続には1,108分岐があり、
両枝が直ちに同じGoto先へ戻り、枝へのincoming edgeが各1本の形は22個だった。
単一predecessorのGoto先は188個。これだけで構造化可能な分岐の総数は判断できないが、
単純なdiamond統合だけで大幅な継続削減ができるという根拠は得られなかった。
この変更では分岐統合は行わない。

測定は `logs/dispatch-balance-20260907/` に保存。
短いselfhost入力は同じCIRから生成した変更前後のcompiler BFを使い、
同じinterpreterでbaseline → balancedを3回交互に実行した。
時間はJSONのexecute phaseのみ（BFの読込・parseを含まない）の中央値。

`balanced` はページ幅のみ、`portal-first` はさらにportal entryを先に配置した版。
`portal-first` も同じ3入力を各3回実行し、baselineの出力とbyte一致した。

| selfhost入力 | 変更前(s) | balanced(s) | portal-first(s) | 最終版の短縮 |
|---|---:|---:|---:|---:|
| hello | 0.4458 | 0.3296 | 0.1952 | 56.2% |
| stage8_aggregates | 6.0712 | 4.5475 | 2.7685 | 54.4% |
| stage11_dynamic_projection | 4.8932 | 3.5993 | 2.2502 | 54.0% |

いずれも生成BFが変更前後でbyte一致した。3入力の生成BFも実行し、既存の期待出力を確認した。
集計と全runの時間は `short-comparison.json`、BF/mapのhashとsizeは `artifacts.json`。

追加テストはD=8/16で全1,024 entryをページ横断して実行する場合、再帰call/return、
global portalとhidden resumeのPCを確認する。ページ幅の切替境界と65,535 IDまでの
encodingの一意性も検証する。`cargo test --workspace` は全件成功。

`balanced` はhelloでnative operationsが82,745,550 → 37,452,998へ減った一方、
scan stepsが25,499,264 → 42,741,284へ増えた。production compilerの自己入力を
各310秒（読込・parse込み）走らせた300秒snapshotでは入力27,556 → 28,122 byte、
両方output 0 byteで、進捗差は約2%に留まった。この中間版のBFは872,432,553 byte。
`full-baseline.metrics` と `full-balanced.metrics` に途中経過を保存している。
これは序盤の診断であり、full selfhost完走のspeedupではない。

`portal-first` のproduction compiler BFは872,430,369 byte（元より+18,235 byte、+0.00209%）。
helloのnative operationsは31,443,036、scan stepsは16,036,804となり、両方とも元より減った。
最大pointerは3版とも1,117,330。aggregateと動的projectionでも最大pointerは変わらない。

最終版のfull自己入力も同じ310秒枠で実行した。300秒時点の入力byte数は以下。

| 版 | input bytes | output bytes |
|---|---:|---:|
| baseline | 27,556 | 0 |
| balanced | 28,122 | 0 |
| portal-first | 36,267 | 0 |

最終版は同じ時間で入力処理が31.6%多く進んだ。最大pointerは3版とも1,121,308。
`full-portal-first.metrics` の最後はSIGINTによるsnapshotで、execute 305.4秒時点。
正常完走ではないためfullのJSON profileやstage3生成物は得られていない。
後半の出力生成phaseへの効果は、この比較からは判断できない。


selfhostテスト全体も同じsource
`logs/full-selfhost-20260907-013902/stage2-compiler-test.bfc` で比較した。
これはRust frontend → BFの経路。baseline artifactは
`logs/frame-allocation-20260907/guarded-inline.bf` を使い、interpreterを今回再実行した。
各版1回のexecute時間で、すべて出力は `ok\n`。

| 版 | execute(s) | native operations | scan steps | 最大pointer |
|---|---:|---:|---:|---:|
| baseline | 25.8004 | 3,101,218,536 | 3,617,018,080 | 1,126,888 |
| balanced | 27.0606 | 1,541,334,069 | 5,696,692,218 | 1,126,888 |
| portal-first | 13.8374 | 1,601,221,450 | 2,090,427,578 | 1,126,888 |

最終版の短縮は46.4%。RLE換算instructionも23,130,084,079 → 16,463,384,797へ減った。
ページ幅のみではnative operationsが減っても実行時間が悪化するため、portal PCの配置を
含む版を採用した。短いselfhostとtest全体の両経路で改善し、frame膨張もない。

helloだけを追加で `--profile-mode counters` でも実行し、stable keyを全context横断で合算した。
`abi.navigation.global` のscan stepsは25,499,264 → 42,741,284 → 16,036,804で、
上記のscan増減がglobal navigationによるものと確認した。counters測定の時間は表に使わない。
全数値は `comparison.json`、集計処理は `summarize.py`、実行手順は
`run-long-comparison.py` と `run-portal-first.py` に残した。workspaceは245テスト成功。
さらに、再帰fixtureを別々のページの低位byte=0へ配置してcall/returnを確認した。

再測定では新しいdirectoryを作り、runbookのBF生成で同じ保存済みCIRを入力する。
例えば最終版のhello比較は以下。baselineには上記full-selfhost directoryのBF/mapを使う。

```sh
mkdir -p logs/dispatch-rerun
target/release/bfc --cir-input logs/full-selfhost-20260907-184903/stage2-compiler.cir \
  --unlimited-tape --profile-granularity continuation \
  --profile-map-output logs/dispatch-rerun/compiler.bfmap.json \
  > logs/dispatch-rerun/compiler.bf
target/release/bf-interpreter --unlimited-tape \
  --profile-map logs/dispatch-rerun/compiler.bfmap.json \
  --profile-mode sample --profile-format json \
  --profile-output logs/dispatch-rerun/hello.profile.json \
  logs/dispatch-rerun/compiler.bf < selfhost/stage2/examples/hello.bfc \
  > logs/dispatch-rerun/hello.bf
```

selfhostテスト全体の生成・実行は `FRAME_ALLOCATION.md` の再実行手順と同じ。
測定前に `cargo build --release -p bf-compiler -p bf-interpreter` を実行する。
