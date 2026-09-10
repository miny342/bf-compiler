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
| HIRの局所Frame lowering | 採用 | 単純なローカルwhileの非ゼロ条件・Output・定数更新を直接Frame命令にし、割当て前に不要な一時セルと条件の0/1化を除く。emit_repeat_256の反復は通常BFの最小形となり出力ベンチを短縮。複雑な制御は従来経路、多箇所inlineは未拡張。 |
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

### HIRの局所Frame loweringを採用

単純なローカルscalar操作を仮想セル割当て前にFrame命令へ直接lowerする。
ローカル変数のOutputは元セルを読み、定数代入・定数加減算は一時セルを作らない。
whileの条件がx / x!=0 / 0!=xなら、x自身を非破壊のLoop条件にする。
本体は直接扱えるblock・宣言・代入・Output・nested whileに限定し、
call・return・abort・portal・一般条件やifは従来経路へ戻す。
試行は読み取り専用で、失敗時に部分的なIRやcontinuation IDを残さない。

値として使う比較結果の0/1化、入力の評価順、ローカル宣言のゼロ初期化は維持する。
多箇所inlineの拡張、interpreterの追加最適化、セルフホスト側の圧縮出力は行っていない。
`--disable-local-control-flow`の後段CFG復元とは独立したsource loweringである。

production stage2 sourceのemit_repeat_256を実際にlowerし、function 180 /
continuation 3760のまま、frame_slots=2と次の本体を確認した。

```text
Set(count, 0)
Output(character)
Set(count, 1)
Loop(count) {
    Output(character)
    AddConst(count, 1)
}
Return
```

ループ内にBranch / Copy / 継続遷移はない。回帰テストではD=8/D=16とも
最適化後BFに `[<.>+]` 相当のMove / Output / Move / Addだけのloopがあることを確認。
関数をinlineせず、関数外枠のReturnは残している。

### 反復出力の測定

基点は`d03e88c`。同じinterpreter（RemoteTransfer有効）で旧／新compilerの生成BFを比較。
`scripts/local-frame/repeat.bfc`は65,280回emit_repeat_256を呼び、
固定文字（A）を16,711,680 bytes出力する。
生成プログラムの保存にBFCRLEを使うが、実行時の出力は圧縮しない。

warm-up後AB/BA交互10ペア、計22実行。全出力hashが一致した。
execute中央値は3,129.624→297.937 ms（約10.5倍）。
paired median差の95% CIは-2,847.750〜-2,808.251 ms。
process_total中央値は3,141.913→310.546 ms。
native operationsは739,054,145→86,448,705、最大pointerは93で不変。
これは反復出力workloadの値であり、full selfhost全体の改善率ではない。

### production source BF比較

同じstage2 compilerソースから生成したBFで3入力を比較。各10ペア＋warm-upの計66実行。
全runで旧／新の出力hashが一致した。

| 入力 | execute中央値 baseline→candidate | execute差分95% CI |
|---|---:|---:|
| hello | 74.633→75.272 ms | -0.496〜+1.571 ms |
| stage5_functions | 560.262→568.728 ms | -18.560〜+16.416 ms |
| stage8_aggregates | 931.029→924.237 ms | -19.795〜-3.593 ms |

hello/functionsの差は識別できない。aggregatesではexecute短縮を支持する。
3入力の最大pointerは不変。CIR経路・full selfhost完走時間は比較していない。
区間はいずれもpaired median差のbootstrap（10,000 resamples、seed 20260910）。
process_totalはinterpreter内のread/parse/execute/output計測で、外部起動終了のwallとは異なる。

再現script：`scripts/local-frame/run.py`。新規rootへ、
`--interpreter`、`--baseline`、`--candidate`を指定する。
compiler BFでは`--inputs`に入力ソースを渡し、反復出力fixtureでは省略する。
出力データはメモリ上でhash照合し、大容量出力を保存しない。

測定ログ・BF/binary/input identity：
`tmp/direct-frame-repeat-measurement/`と`tmp/direct-frame-production-measurement/`の
`manifest.json`、`runs.jsonl`、`summary.json`。
production IRの確認結果は`tmp/direct-frame-production-ir.txt`。
生成BFとmapは`tmp/direct-frame-stage2-compiler.bf`と同名の`.bfmap.json`。

### 検証と設定互換

- `cargo test --workspace`：283件成功。
- 全256文字について各256回の出力、非破壊条件・反復後の値、wraparound、
  nested localの再初期化、入力による条件更新、call・early returnを検証。
- 値としての比較が0/1を返すこと、条件としての比較で元変数を消さないことを確認。
- `scripts/verify-stage2-selfhost.sh`成功（stage-12 self-host verification passed）。
- IR phase artifact identityを`bfc-ir-artifact-v3`へ更新し、helperとfixtureも同期。
  source/CIRとも旧v2設定は再生成が必要。BF mapのschemaと通常BF出力形式は不変。

最終検証ログ：`tmp/direct-frame-tests-final.log`、`tmp/direct-frame-shape-tests.log`、
`tmp/direct-frame-selfhost.log`。workspace/selfhost検証は反復性能測定の完了後に行った。
次のコミットではこの作業結果節を置き換え、採否・理由・制約を保持する。
