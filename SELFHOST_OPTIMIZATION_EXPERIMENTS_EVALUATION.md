# Selfhost 最適化実験の評価

## 検証済みの採否

| 案 | 判断 | 根拠と制約 |
|---|---|---|
| 空Goto threading・到達不能除去・ID compaction | 採用 | source/CIRで出力一致、helloのprocess wall短縮。主効果はdispatcher往復削減。full selfhost改善は未確認。 |
| continuation除去のみでIDを詰めない案 | 不採用 | 疎なpageがequality scanを選び、生成BFが増大した。密なID配置を維持する。 |
| arena_advanceの桁上がり付き加算 | 不採用・試作撤回 | 境界とselfhost検証は通過したが、helloのraw/RLE換算命令とprocess wallが悪化。native operation削減だけでは採用しない。 |
| 同一関数の非空Branch後継inline化（2c） | 再評価後に採用 | 当初の少数wall測定による不採用をpaired比較で見直した。source/CIR複数ケースでBF execute短縮を確認。生成BF増加を伴う。全入力で差を識別できたわけではなく、end-to-end/full selfhost改善は未確認。 |
| IR継続・遷移・終端・phase・portal集計 | 採用 | 出力・通常counter一致とaccounting、source/CIR測定を確認。計測費用があるためオプトイン。BF hidden dispatch/navigation費用は測れていない。 |
| phase/portal計測のレビュー修正 | 採用 | 設定入力上書きを拒否し、portalなしの別phaseを挟む隣接も切断。実入力・options identityを照合し、未知CIRへ固定ID mappingを流用しない。 |
| 高い再訪率を根拠にportal batchを優先する判断 | 撤回 | 開始chunk再訪はphase全体の履歴指標。直前要求の近さやcall/I/O/aliasを跨ぐbatch安全性を示さない。候補を調べる根拠までに限定する。 |

sourceのphase別IR集計では`arena_advance`が熱い関数として確認され、局所構造化の候補に残る。
CIRの関数名は推測しない。frame/global regionと再帰activationを区別し、
同phaseラベルの再帰はphase境界として扱わない。

## 未検証の案

局所if/while構造化は下記の試作・小規模検証まで完了し、通常コンパイルへの採用は保留。
頻度に基づくPC配置、portal連続処理・専用lane、PC多分割は未採用。
BF hidden dispatch/navigation計測とfull selfhost時間比較も未完。
優先順位と必要な検証は`SELFHOST_OPTIMIZATION_EXPERIMENTS.md`を参照する。

## このコミットの作業結果

### 実験2d：局所CFG構造化の試作

開始baselineは`ca33380`。source/CIR共通のallocated Continuation IRに対して、
単一入口の直列block、合流する分岐、then側が戻る局所loopを反復してまとめる。
2c適用済みbaselineの後に追加し、最後のcleanupは2cを再適用せずIDを詰め直す。
call・portal・return・abort境界は維持し、関数entryとcall/portal resumeもincomingとして数える。
外部参照のある内部blockは吸収しない。else側back-edgeや複雑な非構造CFGは対象外。

終端Branchは条件を分岐前だけ消費するが、FrameInstruction::Branchは分岐後にも消す。
allocated slotの再利用を壊さないよう、分岐をまとめる関数に専用guard slotを1つ追加する。
ネスト時もguardを再利用でき、backendのelse flagは別の既存scratchで確保される。
Loopではback-edge前の条件消費と条件本体の再評価を明示する。
frame allocationのやり直しは行わず、追加slotを含むframe layoutはbackendが計算する。

`structure_local_control_flow`と`examples/local_structure.rs`は実験用入口であり、
通常のCLI・default loweringは変更していない。採用時にはoptionsとartifact identityにも
反映し、実験用driverの結果と通常経路の一致を確認する必要がある。

### 今回の検証

`cargo test --workspace`は269件成功。追加4テストは条件slotを両armで上書きする例、
ネストしたif/whileとI/O、call・再帰・portal・abort、CIRの0/1/255境界を扱う。
2c有効/無効のloop形状、D=8/D=16生成BFと直接IRの出力一致を確認した。
CIR fixtureの初版は破壊的Binaryのsource operandを再設定せず停止しなかったため、
そのテスト実行を中断し、各反復で再設定するfixtureに修正してworkspace全体を再実行した。
最終ログは`tmp/local-structure/workspace-tests-final.log`。

固定production sourceは現在のconcat出力とSHA-256が一致した。
source=`9fdab34bc55be1329256fb93c360d8bfb7ce756e4888ce8d9eb972ac731838e6`、
CIR=`c7ff5b09e53714b4e9a2278ee4aee36c8fc88d868f49138c5c1a632fcdb5c2bb`。
直接IRで3入力を比較し、baseline/candidateおよびsource/CIRの出力が一致した。
各経路内でcall・return・portal・I/O回数と正常終了も一致。
生成された3入力分のBFも実行し、各入力をRust frontendの直接IRで実行した結果と一致した
（stage5の入力はEOF条件）。

| 経路/input | baseline continuation実行 | candidate continuation実行 |
|---|---:|---:|
| source/hello | 64,284 | 27,936 |
| source/stage5_functions | 532,821 | 222,851 |
| source/stage8_aggregates | 826,243 | 341,362 |
| CIR/hello | 52,690 | 51,594 |
| CIR/stage5_functions | 435,945 | 420,732 |
| CIR/stage8_aggregates | 681,695 | 637,340 |

静的継続数はsourceで4,980→4,300、CIRで5,155→5,064。
sourceでは直列223・分岐240・loop 8、CIRでは直列29・分岐46・loop 8を変換した。
これはIR計測であり、BF時間改善率ではない。ログは`tmp/local-structure/ir/`。

### arena境界microbenchmarkのBF比較

既存arena_micro sourceと同じ内容の固定CIRを使用した。amount=0/1/255、slot末尾、
page/bank境界を32反復する。入力ファイル名はrunner上でhelloだが、このmainはstdinを読まない。
2cは両variantで有効、同じrelease driver/interpreter、warm-up後AB/BA交互10ペア。
各BF/mapは同時生成し、unprofiled時間測定と別にcandidateのsample profileを取得して
両経路のmap受理・出力一致を確認した。profile sample数は意味上の実行回数に換算しない。

| 経路 | BF execute中央値 baseline→candidate | paired差分95% CI | process wall中央値 | BF bytes baseline→candidate |
|---|---:|---:|---:|---:|
| source | 47.664→27.062 ms（-43.22%） | -21.782〜-20.154 ms | 887.327→816.247 ms | 107,768,541→107,768,153 |
| CIR | 101.350→68.558 ms（-32.36%） | -34.707〜-31.803 ms | 3,512.080→3,489.586 ms | 873,106,720→873,114,661 |

sourceのprocess wall差分区間は負、CIRは0を含む。最大pointerは両経路とも変化なし。
全実行の出力checksumは`ba9e12b580adf8cf5e53fbaf81f09ef7ba814b4a5e88bf4bce77353c32684e11`。
全raw/RLE命令数・native operations・parse/execute・RSS・BF/map hashは
`tmp/local-structure/arena-run/{summary,artifacts,manifest}.json`と`runs.jsonl`に保存した。

arena sourceは内部分岐の統合が効き、fail callを含む外側whileは局所loopになっていない。
CIR microのIR訪問削減は小さい一方、BF時間は短縮した。ID compactionに伴うdispatcher配置の
変化などの寄与は分離できておらず、短縮全体を局所loop化の効果と解釈しない。

### 判断と再現

小規模では有望だが、production BF・複数入力・full selfhost代表phaseの時間は未測定。
通常経路への採用を保留し、次にproduction source/CIRのBF比較を行う。
今回の実験コードと回帰テスト・runnerをcommitし、次のコミットではこの作業結果節を置き換える。

```sh
mkdir -p ./tmp/local-structure/cargo-tmp
TMPDIR="$PWD/tmp/local-structure/cargo-tmp" CARGO_TARGET_DIR="$PWD/tmp/local-structure/build" \
  cargo build --release -p bf-compiler --example local_structure -p bf-interpreter --bin bf-interpreter
python3 scripts/local-structure/run.py ./tmp/local-structure/NEW-RUN \
  --driver ./tmp/local-structure/build/release/examples/local_structure \
  --interpreter ./tmp/local-structure/build/release/bf-interpreter \
  --source SOURCE.bfc --cir SAME_PROGRAM.cir --inputs INPUT.bfc
```

runnerは新しいrootを要求し、identity・IR出力と通常counter・BF出力一致を確認してから
集計する。production比較では`--inputs`にhello、stage5_functions、stage8_aggregatesを指定する。
sourceは`concat-stage2-compiler.sh main`、対応CIRは同じsourceをstage2 CIR compilerへ
入力して生成する。既存固定CIRを使う場合もmanifestに実byte列のhashを記録する。
