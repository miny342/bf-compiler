# Selfhost 最適化実験の評価

## 検証済みの採否

| 案 | 判断 | 根拠と制約 |
|---|---|---|
| 空Goto threading・到達不能除去・ID compaction | 採用 | source/CIRで出力一致、helloのprocess wall短縮。主効果はdispatcher往復削減。full selfhost改善は未確認。 |
| continuation除去のみでIDを詰めない案 | 不採用 | 疎なpageがequality scanを選び、生成BFが増大した。密なID配置を維持する。 |
| arena_advanceの桁上がり付き加算 | 不採用・試作撤回 | 境界とselfhost検証は通過したが、helloのraw/RLE換算命令とprocess wallが悪化。native operation削減だけでは採用しない。 |
| 同一関数の非空Branch後継inline化（2c） | 再評価後に採用 | 当初の少数wall測定による不採用をpaired比較で見直した。source/CIR複数ケースでBF execute短縮を確認。生成BF増加を伴う。全入力で差を識別できたわけではなく、end-to-end/full selfhost改善は未確認。 |
| 局所CFG構造化（2d） | 採用 | source/CIRの複数入力でBF execute短縮と出力一致を確認。call・portal等の境界を維持し、通常loweringで有効化。frame guard追加とID配置変化を伴うため、削減全体をloop化だけの効果とは解釈しない。full selfhost時間の改善は未確認。 |
| IR継続・遷移・終端・phase・portal集計 | 採用 | 出力・通常counter一致とaccounting、source/CIR測定を確認。計測費用があるためオプトイン。BF hidden dispatch/navigation費用は測れていない。 |
| phase/portal計測のレビュー修正 | 採用 | 設定入力上書きを拒否し、portalなしの別phaseを挟む隣接も切断。実入力・options identityを照合し、未知CIRへ固定ID mappingを流用しない。 |
| 高い再訪率を根拠にportal batchを優先する判断 | 撤回 | 開始chunk再訪はphase全体の履歴指標。直前要求の近さやcall/I/O/aliasを跨ぐbatch安全性を示さない。候補を調べる根拠までに限定する。 |

sourceのphase別IR集計では`arena_advance`が熱い関数として確認され、局所構造化の候補に残る。
CIRの関数名は推測しない。frame/global regionと再帰activationを区別し、
同phaseラベルの再帰はphase境界として扱わない。

## 未検証の案

頻度に基づくPC配置、portal連続処理・専用lane、PC多分割は未採用。
局所構造化は単一入口の直列・分岐合流・then側back-edgeを扱う。
else側back-edge、複雑な非構造CFG、call/portal/abortを含む外側while全体の構造化は未対応。
BF hidden dispatch/navigation計測とfull selfhost時間比較も未完。
後続実験は`SELFHOST_OPTIMIZATION_EXPERIMENTS.md`を参照する。

## このコミットの作業結果

### 局所CFG構造化の通常経路への採用

実験用driverと同じ順序（既存cleanup・2c後に構造化、最後に2cなしのcleanup）で
source/CIR共通optimizerへ組み込んだ。
`--enable-local-control-flow` / `--disable-local-control-flow`で切り替えられ、
defaultは有効。2cも引き続き有効で、両optionは独立に設定できる。

専用guard slotで条件cellの分岐後の再利用を保護し、call・portal・return・abort境界と
外部からの入口を維持する。既存allocation後に変換し、追加slotを含むlayoutをbackendで計算する。
static metricsにはlocal構造化による継続削減・直列結合・分岐・loop・scratch追加数を別項目で保存する。

artifact identityを`bfc-ir-artifact-v2`へ更新し、順序・境界付きraw入力に加えて
2cと局所構造化の設定をhashへ含めた。旧identity設定は再生成が必要。
Python helper・固定fixtureも更新し、設定IDだけでなくversionとoptionsの不一致も拒否する。
固定CIRの関数ID mappingは従来どおり確認済みraw hashに限定する。

### production BF比較

比較開始commitは`c14a053`。固定production source/CIRに対し、2c有効のbaselineと
局所構造化を追加したcandidateを同じrelease driver/interpreterで比較した。
各経路・各入力でwarm-up後AB/BA交互10ペア。全132実行（warm-up込み）の出力が
対応する直接IRの出力と一致した。

以下のexecuteは生成されたcompiler BFの実行時間であり、直接IR時間ではない。
差分区間はpaired median bootstrap 95% CI。process wallはparseとプロセス起動等も含む。

| 経路/input | BF execute中央値 baseline→candidate | execute差分95% CI | process wall中央値 baseline→candidate |
|---|---:|---:|---:|
| source/hello | 106.559→84.834 ms（-20.39%） | -22.765〜-20.659 ms | 22.942→22.908 s |
| source/stage5_functions | 824.095→642.328 ms（-22.06%） | -188.186〜-174.407 ms | 23.532→23.261 s |
| source/stage8_aggregates | 1,351.888→1,044.073 ms（-22.77%） | -310.463〜-297.150 ms | 23.891→23.581 s |
| CIR/hello | 118.355→102.573 ms（-13.33%） | -16.280〜-14.556 ms | 3.487→3.491 s |
| CIR/stage5_functions | 976.680→839.633 ms（-14.03%） | -142.780〜-134.572 ms | 4.377→4.201 s |
| CIR/stage8_aggregates | 1,596.141→1,362.362 ms（-14.65%） | -235.567〜-230.400 ms | 4.980→4.719 s |

全6件のexecute差分区間は負。process wallはstage5/stage8の両経路で短縮を支持し、
helloの両経路は0を含むため改善を識別できない。
sourceのparse中央値は約20秒、CIRは約3秒で、executeの改善率をprocess wallへ流用しない。

| 経路 | BF bytes baseline→candidate | map bytes baseline→candidate |
|---|---:|---:|
| source | 5,949,341,515→5,949,341,072 | 10,983,719→10,336,419 |
| CIR | 873,058,539→873,066,794 | 12,497,651→12,443,868 |

最大pointerは全6件で変化なし。RSS差は小さい。
sourceのraw実行命令はわずかに増加した一方、RLE換算命令・native operations・実行時間は減少。
CIRはこれらが減少した。副指標を速度の代用にせず、実時間を採用根拠とする。
CIRでは継続訪問の削減率よりBF時間の短縮率が大きく、ID compaction後のdispatcher配置などの
寄与は分離していない。full selfhost完走時間や代表phaseのBF時間改善は未確認。

生ログ、入力・binary identity、全統計とBF/map hash：
`tmp/local-structure/production-run/{manifest,artifacts,ir,summary}.json`、
`runs.jsonl`および同directoryの各runログ。元の測定物は上書きしていない。

### 通常経路の検証

- `cargo test --workspace`は270件成功。
  D=8/D=16、条件slot再利用、ネスト、CIR境界、call・再帰・portal・abortに加え、
  CLI設定切替、source/CIR identity差、古いversionと偽のoptionsの拒否を確認した。
  source/CIRとも、統合APIのIRが独立実験APIと完全一致するテストを追加した。
- CLI defaultと明示OFFでproduction BF/mapを再生成し、source/CIRの計4組すべてが
  測定に使用した実験driverのBF/mapとbyte単位で一致した。
- default ONのsource/CIR×3入力でphase/portal設定を再生成し、accountingと出力一致を確認した。
  source identityは`0a68efe1fe20a3e5312e4a8d99cf58b0f8eb1ee19d75e929caafd2f0728153d3`、
  CIR identityは`eb5f20d2eb75d8c20b1ee99df4187ea639c1ec1bb8784daa90e2832f91ac088a`。
- 2c専用の既存比較scriptは局所構造化を明示OFFに固定し、別の最適化を混ぜない。
  phase/portal runnerは第3引数でONも選べる。新旧のphase集計を同じartifactと取り違えない。
  切替後のrunnerもON/OFF各6件のphase集計・出力一致と、overhead fixtureの設定照合を確認した。
- `scripts/verify-stage2-selfhost.sh`も正常終了し、`stage-12 self-host verification passed`を確認。
  compiler自身のテスト、無効入力、global・aggregate・snapshot・型・macro・大きいASTの
  生成と実行をdefault ONで検証した。これは機能検証であり、full selfhostの時間比較ではない。

検証ログは`tmp/local-structure/integration-tests-final.log`、
再生成BF/mapとphase結果は`tmp/local-structure/integration/`、
切替scriptの検証結果は`tmp/local-structure/phase-cli-on/`と`phase-cli-off/`、
追加selfhost検証ログは`tmp/local-structure/integration-selfhost.log`。
通常経路を検証するbuild・テストは反復性能測定の完了後に行った。

### 再現

`scripts/local-structure/run.py`のbaselineは局所構造化OFFを明示するため、default採用後も
実験を再現できる。新しいrun rootと同一内容のsource/CIR、3入力を指定する。
`--driver`にはrelease example `local_structure`、`--interpreter`には同じrelease interpreterを使う。

phase集計は`scripts/selfhost-2c/run_ir_phase_portal.sh RUN_ROOT REPO_ROOT --enable-local-control-flow`。
既存2c artifactに対応させる場合は第3引数を省略（OFF）する。
selfhost検証はリポジトリ内の`TMPDIR`と`CARGO_TARGET_DIR`を指定して
`bash scripts/verify-stage2-selfhost.sh`を実行する。
次のコミットではこの作業結果節を置き換え、採否と制約だけを継続して保持する。
