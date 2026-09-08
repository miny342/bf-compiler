# Selfhost 最適化実験の評価記録

## 2026-09-08: 実験2a 空Gotoのjump threading

比較対象は `e9ebac2` (`Optimize D16 portal offset nibble decomposition`) のcheckoutから生成したartifactと、同じsource・同じrelease interpreterで、Continuation IRの空`Goto`をthreadingしたartifactである。最適化はsource frontendのlowering後とselfhost CIR adapter後に適用し、空の`Goto`が指す先を終端まで解決した。関数entry、branch先、call return、portal resumeの参照を更新し、到達不能continuationを除去した。除去後にcontinuation IDを詰め直してdispatcherの密なcountdown配置を維持した。

production sourceは `scripts/concat-stage2-compiler.sh main` の出力で、入力は `selfhost/stage2/examples/hello.bfc` (201 bytes)。BF artifactは `--unlimited-tape` で生成した。実行はrelease版 `bf-interpreter` のfast IR、`--stats --no-progress --unlimited-tape`で各3回行い、wall timeは中央値を採用した。baseline artifactはgit archiveした`e9ebac2`から生成した。artifactとprofile mapは同時に生成し、optimized artifactのBF SHA-256は `a61c90d0a783957ca6b8505180dae7f0219f00cc291a9a8383518bdc5fe92aa6` である。

### CFGの変化

| 指標 | baseline | optimized | 変化 |
|---|---:|---:|---:|
| continuation数 | 6,068 | 5,012 | 1,056減 |
| 空Goto数 | 1,056 | 0 | 1,056減 |
| 書き換えたsuccessor参照 | — | 1,244 | — |
| 書き換えたfunction entry | — | 5 | — |
| continuation ID | — | compactionあり | — |

### production compilerでの比較

| 指標 | baseline | optimized | 削減率 |
|---|---:|---:|---:|
| BF source bytes | 5,949,379,055 | 5,949,228,509 | 0.0025% |
| helloコンパイル raw実行命令数 | 90,193,010,191 | 89,958,912,959 | 0.2596% |
| helloコンパイル RLE換算命令数 | 238,721,767 | 226,169,025 | 5.2583% |
| native operations | 29,370,131 | 25,861,380 | 11.9467% |
| process wall time（parse込み） | 23.05 s | 22.01 s | 4.5119% |
| 最大pointer | 1,119,023 | 1,119,023 | 変化なし |
| RSS | 約6.85 GiB | 約6.85 GiB | 大差なし |

3回のwall timeはbaselineが`24.35, 23.05, 22.40 s`、optimizedが`22.37, 22.01, 20.66 s`だった。compilerの出力BFはbaselineとoptimizedで異なるが、hello入力をコンパイルした出力BFはbyte単位で一致した。生成したBFを実行した結果も`41 21 0a` (`A!\n`)で一致した。

### 判断

空Gotoの除去は、native operation 11.9%、RLE換算命令 5.3%、hello compileのprocess wall time 4.5%の改善を示したため採用する。BF source bytesの削減は0.0025%と小さく、最適化の主な効果はdispatcher往復の削減である。

途中でIDを詰め直さずにcontinuationを除去した版は、BF sourceが5,949,834,560 bytesまで増加した。空きIDによってdispatcher pageが疎になり、countdownではなくequality scanが選ばれたためである。この結果を受け、continuation除去とID配置を分離せずに実装した。

今回は実験2の空Goto段階までを実施した。局所`if`/`while`の構造化、`arena_advance`の桁上がり付き加算、full selfhost完走時間の比較は未実施であり、次の実験候補として残す。helloは小さい完走入力なので、full selfhostの改善率を直接表さない。

### CIR adapter経路

同じCIR (`145,210 bytes`, decode前のflat continuation 5,737件)を `e9ebac2` のadapterとoptimized adapterへ入力した。adapter前のCIR生成は現行のstage2 CIR compilerで一度だけ行い、backend比較では同じCIRを使った。

| 指標 | baseline | optimized | 削減率 |
|---|---:|---:|---:|
| adapter後continuation数 | 5,854 | 5,197 | 11.2231% |
| BF source bytes | 872,442,809 | 872,309,321 | 0.0153% |
| helloコンパイル raw実行命令数 | 17,810,788,702 | 17,769,955,700 | 0.2293% |
| helloコンパイル RLE換算命令数 | 388,928,100 | 398,866,281 | 2.5553%増 |
| native operations | 25,562,312 | 27,881,746 | 9.0736%増 |
| process wall time（parse込み） | 3.64 s | 3.15 s | 13.4615% |
| 最大pointer | 1,117,330 | 1,117,330 | 変化なし |

出力BFはbyte単位で一致し、生成したBFの出力も`41 21 0a` (`A!\n`)で一致した。CIR経路ではnative operationとRLE換算命令が増えた一方、wall timeは短縮したため、これらの副指標だけで採否を決めない。source経路とCIR経路でContinuation IDの構造が異なることも確認できたので、次回以降は両経路を別artifactとして記録する。

## 2026-09-08: 実験1 IR遷移集計の追加

Continuation IR runnerに、実行時の`from -> to`遷移を集計するオプトイン計測を追加した。`bfc --run-ir`ではhot continuationに加えてhot transitionをstderrへ出力する。通常のIR実行では遷移mapを作らず、計測追加の影響を通常backendの比較値へ混ぜない。phase境界やportal要求元の分類はまだ実装していないため、これは実験1の基礎計測である。

## 2026-09-08: 実験2b `arena_advance`の桁上がり付き加算

実験2aのoptimized artifactをbaselineにして、selfhost compilerの`arena_advance`を1スロットずつ進めるループから、slotの加算と桁上がり判定へ置き換えた。`cell`が8 bitなので`slot + amount`は最大1回だけpageをまたぐ。最終bank/pageでのoverflow、page内のwrap、最終pageから次bankへの移動、`amount == 0`をstage2のarena testで確認した。

比較対象は同じbranch・同じrelease compiler/interpreterで、変更前の実験2a optimized artifact `/tmp/bfcompiler-selfhost-opt.BOEWDb/stage2-compiler.bf`と、変更後に生成した`/tmp/bfcompiler-arena-advance.4HB0cV/stage2-compiler.bf`である。入力は`selfhost/stage2/examples/hello.bfc` (201 bytes)。stage-12 selfhost verificationは完走した。artifact生成時間は変更後10.57 s、最大RSSは6,242,664 kBだった。

| 指標 | 実験2a optimized | arena_advance変更後 | 変化率 |
|---|---:|---:|---:|
| stage2 compiler source bytes | 184,330 | 184,305 | 0.0136%減 |
| compiler BF source bytes | 5,949,228,509 | 5,949,224,358 | 0.0001%減 |
| helloコンパイル raw実行命令数 | 89,958,912,959 | 90,073,042,346 | 0.1269%増 |
| helloコンパイル RLE換算命令数 | 226,169,025 | 333,366,860 | 47.3972%増 |
| native operations | 25,861,380 | 21,790,580 | 15.7408%減 |
| RLE operations | 41,639,381 | 37,185,609 | 10.6961%減 |
| process wall time（parse込み、中央値） | 22.01 s | 22.57 s | 2.5443%増 |
| 最大pointer | 1,119,023 | 1,119,023 | 変化なし |
| RSS（中央値付近） | 6,848,756 kB | 6,848,664 kB | 大差なし |

変更後のwall timeは`24.28, 22.42, 22.57 s`、baselineは`22.37, 22.01, 20.66 s`だった。生成されたhello用BFはbaselineとbyte単位で一致し、実行結果も`41 21 0a` (`A!\n`)で一致した。artifactのSHA-256は変更後が`4a3c15df32a06da73bf439835c0a462df6e17cd79ff9c08810fd159985a4e62c`、hello出力はbaselineと同じ`acb48860c1ae3a117e0bee07af290638de1e4a7006794e43152670bfa90875ff`だった。

native operationsとRLE operationsは減ったが、raw実行命令数とRLE換算命令数、wall timeは悪化した。したがって、hello compileのend-to-end指標ではこの変更を採用せず、実験2a optimizedをbaselineとして残す。`arena_advance`の局所的な命令削減効果は確認できたため、別の実行形態またはfull selfhost入力で再評価する候補とする。

## 2026-09-08: 実験2c branch successorのinline化

同一関数内の`Goto`が、非空の`Branch` continuationへ向かう場合、その後継のframe instructionを`Goto`元へ複製し、後継の`Branch` terminatorを元のcontinuationへ移した。後継continuation自体は別incoming edgeのため残す。これにより、whileのback-edgeから条件continuationへ戻るdispatcher往復を削減できる。call・portal・returnを含むcontinuationは対象にせず、同一関数のBranch successorだけを対象にした。

arena変更の影響を分離するため、baselineは実験2a optimized（`arena_advance`変更前）、optimizedは同じsourceにbranch successor inline化だけを加えたartifactとした。入力は`selfhost/stage2/examples/hello.bfc` (201 bytes)。stage-12 selfhost verificationは完走した。inline化後のartifact生成時間は10.46 s、最大RSSは6,243,880 kBだった。

| 指標 | 実験2a optimized | branch inline化 | 変化率 |
|---|---:|---:|---:|
| stage2 compiler source bytes | 184,330 | 184,330 | 変化なし |
| compiler BF source bytes | 5,949,228,509 | 5,949,341,515 | 0.0019%増 |
| helloコンパイル raw実行命令数 | 89,958,912,959 | 89,948,431,759 | 0.0117%減 |
| helloコンパイル RLE換算命令数 | 226,169,025 | 222,176,579 | 1.7652%減 |
| native operations | 25,861,380 | 24,657,913 | 4.6535%減 |
| RLE operations | 41,639,381 | 40,512,404 | 2.7065%減 |
| process wall time（parse込み、中央値） | 22.01 s | 22.15 s | 0.6361%増 |
| 最大pointer | 1,119,023 | 1,119,023 | 変化なし |
| RSS | 約6.85 GiB | 約6.85 GiB | 大差なし |

wall timeはbaselineが`22.37, 22.01, 20.66 s`、inline化後が`23.97, 22.15, 20.11 s`だった。生成されたhello用BFはbyte単位でbaselineと一致し、実行結果も`41 21 0a` (`A!\n`)で一致した。inline化後artifactのSHA-256は`73f95665ae1b04f3b43fdb45b4bd5b451400ba0bafee1f48a4dfb332690556f8`だった。

native operations、RLE operations、raw/RLE命令は減ったが、BF sourceが113,006 bytes（約110.4 KiB）増え、3回の中央値wall timeは0.64%悪化した。したがって、この変換は現在のhello compileのend-to-end基準では採用せず、実験2a optimizedを本命baselineとして保持する。loop専用のinlining範囲をさらに絞るか、dispatcher移動費用を直接測る別benchmarkで再評価する候補とする。

## 2026-09-08: 実験2c再評価とIR計測

前回のhello 3回測定にあった0.64%の中央値増加は、別時点の少数測定であり、今回の不採用根拠としては確度が不足していた。baselineを`bd3560470b9f7c5c8f06f413d1a8cd6224878c34`に固定し、同一入力・同一release fast IR interpreterで、source/CIRを分けてAB/BA交互10ペア（各10回）再測定した。

2cはslot allocation後の共通Continuation CFGで、同一関数内のGoto元に非空Branch後継のbodyを順序どおり複製し、Branch terminatorをGoto元へ移す。後継continuationは他のincoming edgeのため残す。candidateは`--enable-2c`、baselineは`--disable-2c`で生成した。再評価でsource/CIRの両方に適用できることを確認し、結果を採用してdefaultを有効化した。`--disable-2c`は再現比較用に残した。

成果物と生ログはすべて`tmp/next-2c-reeval/`に保存した。baselineはgit archiveで展開した専用checkoutからrelease buildし、candidateは作業branchのrelease buildを固定した。source compiler BFCは184,330 bytes、固定CIRは145,210 bytes（flat continuations 5,737件）である。

| 経路 | artifact | BF bytes | map bytes | BF SHA-256 |
|---|---|---:|---:|---|
| source | baseline | 5,949,228,509 | 10,829,995 | `a61c90d0a783957ca6b8505180dae7f0219f00cc291a9a8383518bdc5fe92aa6` |
| source | candidate | 5,949,341,515 | 10,983,719 | `73f95665ae1b04f3b43fdb45b4bd5b451400ba0bafee1f48a4dfb332690556f8` |
| CIR | baseline | 872,309,321 | 12,031,934 | `b6e175877113fbe7cb8fba72f78593e2dd5ee290475fd20aaf4091c464a107f4` |
| CIR | candidate | 873,058,539 | 12,497,651 | `daadb3f4a47983501e5baba295a53cf4ed738ff2bbeb2793da1a3fad08705db9` |

mapは各BFと同じprofile compileで生成し、source helloのsample profileでsidecar identity検証も通した。3入力の出力はsource/CIRの全artifact間で一致した。hello output SHA-256は`acb48860c1ae3a117e0bee07af290638de1e4a7006794e43152670bfa90875ff`、stage5_functionsは`c49f795f133d8f4ba2a02d7d316e175ee0e08d7e8dc6439d46b86fc1a19567f8`、stage8_aggregatesは`59ace01f0ba73410072f2f0d80846a774be7061ab16e3cce093568e1041b26da`である。

### 実測値

process wallは`/usr/bin/time -v`のelapsed、executeはBF interpreterの`execute_ns`で、単位はms。ここでのexecuteは生成BFを`bf-interpreter`で実行した時間であり、`bfc --run-ir`の直接IR runner時間ではない。changeはcandidate対baselineの中央値比。括弧内は同じペア差分のmedian bootstrap 95% intervalで、seedは`20260908`、10,000 resamplesである。全回のraw値は`tmp/next-2c-reeval/measurements/summary.json`と`runs/runs.tsv`に保存した。

| 経路/input | process wall baseline→candidate | execute baseline→candidate | raw instructions change | RLE instructions change |
|---|---:|---:|---:|---:|
| source/hello | 22,845→22,910 ms (+0.2845%, CI -2720→+2785 ms) | 97.558→94.037 ms (-3.6090%, CI -7.264→+0.135 ms) | -0.0117% | -1.7652% |
| source/stage5_functions | 23,425→23,480 ms (+0.2348%, CI -2620→+2750 ms) | 762.428→726.104 ms (-4.7643%, CI -43.096→-29.935 ms) | -0.0159% | -1.8122% |
| source/stage8_aggregates | 24,055→23,965 ms (-0.3741%, CI -3080→+2675 ms) | 1,258.752→1,190.851 ms (-5.3944%, CI -73.719→-60.287 ms) | -0.0171% | -1.7056% |
| CIR/hello | 3,075→3,075 ms (0.0000%, CI -1490→+90 ms) | 105.058→104.907 ms (-0.1438%, CI -1.467→+1.543 ms) | -0.3270% | +0.0869% |
| CIR/stage5_functions | 3,810→3,855 ms (+1.1811%, CI -30→+1455 ms) | 863.884→861.616 ms (-0.2625%, CI -10.934→+21.895 ms) | -0.2331% | +0.1150% |
| CIR/stage8_aggregates | 4,415→4,410 ms (-0.1133%, CI -110→+1365 ms) | 1,435.401→1,415.189 ms (-1.4081%, CI -36.104→-4.209 ms) | -0.0858% | -0.1031% |

source executeは3入力すべて短縮し、CIR executeも3入力すべて短縮した。source/stage5・source/stage8・CIR/stage8はexecute差分の区間が0を含まず短縮を支持する一方、source/hello・CIR/hello・CIR/stage5は0を含むため、全入力で効果を識別できたとは言えない。process wallの区間は全て0を含み、幅も大きい。今回の測定で退行を検出しなかったとは言えるが、退行しないことやend-to-end/full selfhost改善を確認したとは言えない。candidateのstatic変化はsourceでcontinuation 5,012→4,980、inline 334件、複製1,305 instruction、CIRで5,197→5,155、inline 599件、複製3,098 instructionだった。frame instruction、call/return、aggregate accessの通常カウンタは各入力でbaselineとcandidateが一致し、減少はdispatcher continuation訪問に対応する。

### IR計測

`bfc --run-ir --ir-metrics PATH`でcontinuation実行数、全from→to遷移、terminator分類、terminal Halt/Abort、static/dynamic in/out degree、function IDをJSONへ出力した。source frontendのfunction名は保存し、CIR adapterは名前を推測せず`name: null`のstable function IDだけを保存する。全12件で`accounting.ok=true`、transition total = executed continuation - 1、各nodeのdynamic入出力 = execution数を確認した。

| 経路/input | baseline executed cont. | candidate executed cont. | arena_advance実行数 |
|---|---:|---:|---:|
| source/hello | 70,656 | 64,284 | 47,452→42,460 |
| source/stage5_functions | 591,914 | 532,821 | 413,324→369,543 |
| source/stage8_aggregates | 936,521 | 826,243 | 619,913→554,350 |
| CIR/hello | 60,111 | 52,690 | function names unavailable |
| CIR/stage5_functions | 500,301 | 435,945 | function names unavailable |
| CIR/stage8_aggregates | 800,078 | 681,695 | function names unavailable |

IR metricsの遷移収集ON/OFFでは、同じ出力（1 byte）と通常カウンタ（2,097,186 continuation dispatch、14,688,493 frame instruction、1 input、1 output）が一致し、ONだけがhot transitionを出力した。従来の19 continuation程度のfixtureは`/usr/bin/time`の0.00秒表示で識別できなかったため、今回の追加計測では同じ小さいBFC program内の32 × 255 × 255回の内側ループを反復し、`bfc-ir phase=execute ... elapsed_ns`を実行区間だけの高分解能値として取得した。AB/BA交互10ペアのexecute中央値はOFF 86.503 ms、ON 108.725 ms（+25.69%、ペア差分95% bootstrap +19.364〜+23.921 ms）だった。process wallは粗い10 ms分解能でOFF 80 ms、ON 110 msとなるため、overheadの主値には使わず、loweringとprocess起動を含まないexecute値を採用した。出力SHA-256は`6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d`で、raw runsと集計は`tmp/review-fixes-ir-overhead/ir-overhead/`に保存した。この測定はBF benchmarkの比較値へ混ぜていない。

### arena_advance microbenchmark

`06_arena.bfc`の`arena_advance`本体は変更せず、slot=255、page末尾、bank末尾、amount=0/1/255を32反復する専用mainを作った。出力checksumは両経路・両artifactで`ba9e12b580adf8cf5e53fbaf81f09ef7ba814b4a5e88bf4bce77353c32684e11`で一致した。sourceのprocess wallは760→775 ms（+1.97%、CI -20→+40 ms）、executeは43.230→41.399 ms（-4.24%）。CIRは3,055→3,060 ms（+0.16%、CI -40→+50 ms）、executeは89.866→88.904 ms（-1.07%）だった。microbenchmarkは短く、process wallの差は識別できない。

### sample profileと判断

source helloの同じartifact/mapで2ms samplingをbaseline/candidate各1回取得した。sample countはいずれも54であり、continuation実行回数として解釈しない。profile reportにはdispatcher/page countdown、navigation、portal、frame操作のsite別観測を保存したが、これはprofile overheadを含む別測定であり、上のunprofiled process/execute値へ混ぜていない。

正しさ、D=8/D=16を含むworkspace 163 tests、default 2cでの`verify-stage2-selfhost.sh`、source/CIRの複数入力でのBF interpreter execute短縮を確認できた。採用根拠は、意味を保持したうえで複数ケースのBF executeを短縮したことに置く。process wallは退行を検出しなかったが区間が広く、end-to-endやfull selfhostの改善は未確認であるため、そこを採用根拠として過大に扱わない。前回の0.64%中央値増加だけで不採用とする判断は、今回のpaired再測定では支持されなかった。BF source bytesはsourceで+113,006（約110.4 KiB）、CIRで+749,218であり、生成サイズとBF interpreter executeを併記したうえでdefault採用を維持した。

今回完了したのは2c復元・common source/CIR適用・IR基本計測・3入力のsource/CIR比較・arena microbenchmark・profile sample・評価記録である。phase別集計、portal offset列、BF hidden dispatch eventの新規計測、arena算術化、局所if/while構造化、PC配置、portal ABI、PC多分割、full selfhost長時間比較は未完了。次は局所構造化へ進む前に、CIR側function名/source対応を持たない制約を補うphase/portal計測を優先する。今回のCIRでは名前付きarena_advanceの判断をしていないため、未計測のPC/portal削減効果は断定しない。

再実行用コマンドは次のとおり。全成果物は指定root配下に作られる。

```sh
scripts/selfhost-2c/build_artifacts.sh \
  ./tmp/next-2c-review-fixes-$(date +%Y%m%d-%H%M%S) /home/a/git/bfcompiler
scripts/selfhost-2c/run_measurements.sh \
  ./tmp/next-2c-review-fixes-RUN /home/a/git/bfcompiler
python3 scripts/selfhost-2c/summarize_measurements.py \
  ./tmp/next-2c-review-fixes-RUN
scripts/selfhost-2c/run_ir_metrics.sh \
  ./tmp/next-2c-review-fixes-RUN /home/a/git/bfcompiler
scripts/selfhost-2c/run_ir_overhead.sh \
  ./tmp/next-2c-review-fixes-RUN /home/a/git/bfcompiler
python3 scripts/selfhost-2c/summarize_ir_overhead.py \
  ./tmp/next-2c-review-fixes-RUN
scripts/selfhost-2c/run_arena_measurements.sh \
  ./tmp/next-2c-review-fixes-RUN /home/a/git/bfcompiler
python3 scripts/selfhost-2c/summarize_arena.py \
  ./tmp/next-2c-review-fixes-RUN
scripts/selfhost-2c/run_hello_profiles.sh \
  ./tmp/next-2c-review-fixes-RUN /home/a/git/bfcompiler
python3 scripts/selfhost-2c/write_manifest.py \
  ./tmp/next-2c-review-fixes-RUN /home/a/git/bfcompiler
```

source生成は`bfc --unlimited-tape --profile-map-output MAP --profile-granularity continuation stage2-compiler.bfc > ARTIFACT`、CIR生成は`bfc-baseline --run-ir stage2-cir-compiler.bfc < stage2-compiler.bfc > stage2-compiler.cir`、CIR backendは`bfc --cir-input stage2-compiler.cir --unlimited-tape --profile-map-output MAP --profile-granularity continuation > ARTIFACT`である。時間測定は各artifact/inputにwarm-upを1回置き、`bf-interpreter --unlimited-tape --no-progress --stats --timings ARTIFACT < INPUT`をAB/BA交互10ペアで`/usr/bin/time -v`に包んだ。

今回のレビュー修正で、評価文書と再実行scriptを追跡対象へ移した。scriptは`/home/a/git/bfcompiler/scripts/selfhost-2c/`にあり、artifact生成、baseline checkout、固定CIR生成、source/CIR計測、IR metrics、overhead、arena集計、profile、manifestを順に再現できる。各scriptは新しいrun rootを要求し、既存のruns.tsv・metrics・profileを既定では上書きしない。巨大BF/map、build、生ログはcommitせず、実行時の出力は指定した`./tmp`配下へ置く。既存の`tmp/next-2c-reeval/`に保存した元測定値は保持し、今回のprocess wall区間の転記だけを訂正した。
