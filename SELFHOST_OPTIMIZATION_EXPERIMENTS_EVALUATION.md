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
| BFCRLE v1テキスト圧縮 | 採用（オプトイン） | Rust版出力のSSD書込量を削減。通常BF互換を維持し、profileの展開後ordinal/identityとinline markerを保つ。セルフホスト版の出力は未変更。 |
| interpreter RemoteTransfer | 採用 | BFの意味から経路不変性を実行時検証してScan往復を一括転送へ置換。source由来BFの複数入力で出力・論理counter一致とexecute短縮を確認。不成立時は通常実行。CIR/full selfhost時間比較は未実施。 |
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

### RemoteTransferの採用

Scanを含むMove/Add loopをFastIRのRemoteTransferへ落とす。コンパイラの出力・ABIは変更しない。
通常BFとBFCRLE v1の双方でdefault ON、`--disable-remote-transfer`で比較できる。

32命令以下の候補を実行時に読み取り専用で探索し、元のpointerへ戻ること、
Scanの全判定セル（ゼロ終端を含む）と更新先の非重複、元セルの正味delta ±1を確認する。
複数更新先・係数付き転送に対応し、不成立・未割当領域への移動では元のloopへ戻す。
既存のClear/Scan/固定offset Transferは維持する。nibble生成側は変更していない。

raw/RLE換算命令・最大pointerと出力は不変。profileの論理移動距離・loop回数も保持し、
融合したnative operationはsiteのLCAへ帰属する。
remote_transfer_loops / iterations / fallbacksをstatsとprofileに追加した。
独立したScanのcounterは減るが、経路探索もあるためその削減率を実メモリアクセス削減率とはしない。

### production source BF比較

圧縮対応commit `d441d20` を基点とし、同じrelease interpreterと同じstage2 compiler BFを使用。
BFは圧縮＋inline metadata付きだが、測定はprofileなし（markerはコメントとして扱う）。
各入力warm-up後AB/BA交互10ペア、計66実行。別のBFを生成して比較したものではない。
全runで出力hash、raw/RLE換算命令数、最大pointerが一致した。

| 入力 | execute中央値 OFF→ON | execute差分95% CI | process_total中央値 OFF→ON |
|---|---:|---:|---:|
| hello | 83.674→76.625 ms（-8.43%） | -9.593〜-5.603 ms | 661.798→650.568 ms |
| stage5_functions | 631.653→560.285 ms（-11.30%） | -78.970〜-60.413 ms | 1,189.184→1,154.853 ms |
| stage8_aggregates | 1,034.468→932.057 ms（-9.90%） | -105.339〜-92.296 ms | 1,617.495→1,515.461 ms |

区間はpaired median差のbootstrap（10,000 resamples、seed 20260910）。
execute差は3入力とも負。process_totalはstage5/stage8で短縮を支持し、helloは0を含む。
parse差は3入力とも0を含み、この比較では改善・悪化を識別しない。
process_totalはinterpreter内計測（read/parse/execute/outputを含む）であり、
外部プロセス起動から終了までのwall時間とは区別する。
CIR由来BF・full self-compilation時間の比較は未実施。

stage8ではRemoteTransfer適用277,503回、まとめた反復1,633,821回、fallback 5回。
native operationsは233,257,625→219,214,685。
実行時間改善を採用根拠とし、命令counterだけで判断していない。

生データ・入力/BF/binary hash：
`tmp/remote-transfer-measurement/{manifest,summary}.json`、
`runs.jsonl`、後処理の`paired-intervals.json`。
比較scriptは`scripts/remote-transfer/run.py`。新規rootと
`--interpreter`、`--program`、`--inputs`、`--pairs 10`を指定する。

### full selfhost途中の観測（性能比較ではない）

ユーザーの `full7.metrics` はsample modeで、取得した末尾はelapsed 3,300秒、
入力184,330 bytes、出力蓄積7,340,721,984 bytes。ソースhashを既存のfunction mappingと
照合し、function.180/continuation.3760はemit_repeat_256、function.29/continuation.299は
emit_move_toと確認した。最後の60秒ではportal左右の累積samplesは変わらず、
emit_repeat_256のbranch/copyが増加しており、当該区間はcodegenの反復出力が主な候補。
累積上位siteを現在phaseの順位と誤解しない。

現interpreter CLIは出力をVecへ蓄積し完走後にstdoutへ書くため、output_bytesは
ディスク書込済み量ではない。出力蓄積とともにRSSも増えている。
この観測は同じ進捗・計測modeのOFF比較ではなく、full selfhostの改善率・完走を示さない。
セルフホスト側RLE生成や出力streamingは今回実装していない。

### 検証

- `cargo test --workspace`：280件成功。
- 全256初期値、左右方向、Scan長0を含む複数距離、加減算係数、複数転送先を
  従来FastIR実行と比較し、tape全体・pointer・raw/RLE換算回数を確認。
- Scan判定セルとのalias、元のpointerへ戻らない経路、境界エラー、
  動的テープ拡張、失敗探索の無副作用、探索中のinterruptを確認。
- 通常BF・圧縮BF・inline marker付きBFで出力とraw reference counterを照合。
  counters/exact/sampleの各modeとsite融合後の集計を確認。
- `scripts/verify-stage2-selfhost.sh`成功（stage-12 self-host verification passed）。
  Rust版生成物は圧縮し、セルフホスト版の通常BF出力も実行した。

ログ：`tmp/remote-transfer-tests-final.log`、`tmp/remote-transfer-unit.log`、
`tmp/remote-transfer-selfhost.log`。最終workspace/selfhost検証は反復性能測定後に実行した。
次のコミットではこの作業結果節を置き換え、採否・理由・制約を保持する。
