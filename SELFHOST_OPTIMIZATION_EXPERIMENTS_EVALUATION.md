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
| BFCRLE v1テキスト圧縮 | 採用（オプトイン） | Rust版出力のSSD書込量を削減。通常BF互換を維持し、profileの展開後ordinal/identityとinline markerを保つ。セルフホスト版にもcompressedエントリを追加し、通常mainとbinary CIRを維持。 |
| interpreter RemoteTransfer | 採用 | BFの意味から経路不変性を実行時検証してScan往復を一括転送へ置換。source由来BFの複数入力で出力・論理counter一致とexecute短縮を確認。不成立時は通常実行。CIR/full selfhost時間比較は未実施。 |
| HIRの局所Frame lowering | 採用 | 単純なローカルwhileの非ゼロ条件・Output・定数更新を直接Frame命令にし、割当て前に不要な一時セルと条件の0/1化を除く。emit_repeat_256の反復は通常BFの最小形となり出力ベンチを短縮。複雑な制御は従来経路、多箇所inlineは未拡張。 |
| Frame Compareと非同期分岐による比較 | 採用 | source/binary CIRの比較をABI loweringまで保持し、反復内のoperand複製を省く。全8-bit入力・両ABI配置を検証。比較ベンチの時間短縮を確認したが、付随する既存inline/CFG簡約も含む。一部生成BFサイズは増加。full selfhost時間は未測定。 |
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

### 大小比較をFrame Compareとして保持

sourceのHIR loweringとbinary CIR adapterで、大小比較の意味を保持する
`FrameInstruction::Compare`を生成する。入力2値のsnapshotをunsigned比較し、
両operandを0にしてから指定した結果値をdstへ書く。operand/dstのaliasを許す。
CIR VM、validation、slot allocationのread/write集合とremappingを対応させた。

BF backendは既存ABI Scratch0..3の4連続セルを使う。比較反復の非ゼロ判定を、
入口と出口のpointer位置が異なる非同期分岐で実装し、各反復のoperand複製・復元を除く。
作業セルは出口ですべて0、context originへ復帰。D=8/D=16の両配置を扱う。
通常BFの意味だけを使用し、interpreterやBFCRLE形式は変更しない。
binary CIRでは逆向き比較のswapと、結果を反転する追加boolean化も省く。

Tape IR、汎用非破壊Branch、十進変換の定数特殊化、多箇所inlineは追加していない。
BF/mapは再生成が必要。IR artifact identityをv4へ更新し、phase設定も再生成する。

### 検証と測定

- `comparison_lowering.rs`: sourceの全256×256組の4種大小比較、元値保存、
  local CFG構造化ON/OFF、D=8/D=16でIR/BF結果を確認。
- binary CIRも全256×256組・4比較・D=8/D=16を確認。
- public Compareのframe/global/AbiValue間alias、任意の真偽結果値、
  operandのclearとdst書込み順を確認。
- `cargo test --release --workspace`成功（286件）。
  ログ: `tmp/compare-tests-final.log`。
- `scripts/verify-stage2-selfhost.sh`成功。
  ログ: `tmp/compare-stage2.log`。
- `scripts/verify-selfhost-compressed.py`成功。全18例の展開後命令列一致と、
  反復数・十進境界のIR/BF結果一致を確認。
  ログ: `tmp/compare-compressed-verification.log`。
- 比較ベンチ: `scripts/comparison-lowering/run.py`。
  変更前9884870のbinaryとcandidateを同じinterpreterでAB/BA交互5組測定。
  全ペアの4比較とoperand出力を含む。時間はstats有効のprocess wall（execute専用ではない）。

| 比較ベンチ | 変更前 | 変更後 |
|---|---:|---:|
| process wall中央値 | 2.579 s | 0.962 s |
| native operations | 567,770,227 | 284,373,322 |
| raw BF命令換算 | 55,662,509,210 | 12,779,247,956 |
| RLE命令換算 | 32,086,307,245 | 1,864,472,662 |
| 通常BF source bytes | 5,970 | 18,321 |
| max pointer | 93 | 59 |

出力393,216 bytesは期待値と一致。ログ: `tmp/compare-pairs-bench.log`。
比較loweringによるframe縮小で既存の単一使用void inlineが成立し、
このfixtureは2関数7continuationから1関数1continuationにも変化する。
したがって約2.68倍の速度は比較template単体の効果ではない。
同時にsource bytesは約3.07倍へ増えるため、サイズが必ず縮むとは主張しない。

full9と同じセルフホストsourceから実行用BFを生成し、
hello / stage8_aggregates入力を同じinterpreterでAB/BA交互4組比較した。
生成結果は両variantでbyte一致した。process wall中央値はhello 0.839→0.730 s、
aggregates 1.598→1.573 s。特に後者は小差であり強い速度改善の根拠にはしない。
実行用compressed BFは6,295,086→6,289,632 bytes。
ログ: `tmp/compare-bench.log`。比較以外の配置・continuation変化も含む。

full selfhostを再完走しての速度測定は未実施。
セルフホストBFC側のBF生成アルゴリズムは変えていないため、
full9の出力命令列が5.5 TB相当になる問題を解決した変更ではない。

次のコミットではこの作業結果節を置き換え、採否・理由・制約を保持する。
