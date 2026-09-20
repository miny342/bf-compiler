# Selfhost 最適化実験の評価

> 現在のABIはD=16のみをサポートする。以下のD=8への言及は実験当時の記録である。

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
| frame/global byte搬送のunary既定化 | 採用 | RemoteTransfer有効時のコンパクトケースでexecute短縮を確認。nibbleはBF命令数を抑える選択肢として`--enable-nibble-transfer`に保持。offset/window移動・ABI配置は維持。full selfhost速度は未測定。 |
| stage2 BF最適化器のcount読み書き削減 | 撤回（最適化器ごと撤去） | token kindに応じたlaneだけを読み書きし、相殺時の書き戻しを除去。最適化規則と生成出力は維持。小入力のbyte単位portal要求削減とIR/BF照合を確認。 |
| HIRの局所Frame lowering | 採用 | 単純なローカルwhileの非ゼロ条件・Output・定数更新を直接Frame命令にし、割当て前に不要な一時セルと条件の0/1化を除く。emit_repeat_256の反復は通常BFの最小形となり出力ベンチを短縮。複雑な制御は従来経路、多箇所inlineは未拡張。 |
| Frame Compareと非同期分岐による比較 | 採用 | source/binary CIRの比較をABI loweringまで保持し、反復内のoperand複製を省く。全8-bit入力・両ABI配置を検証。比較ベンチの時間短縮を確認したが、付随する既存inline/CFG簡約も含む。一部生成BFサイズは増加。full selfhost時間は未測定。 |
| 連続copyのrestore共有とzero destination | 採用 | aggregate、aggregate引数/戻り値、portalの連続copyではrestoreを列の先頭で一度だけclearする。新規callee frameの引数領域と、直前にclearしたportal protocol fieldではdestination clearも省く。32-cell引数/戻り値fixtureでnative 20.2%、RLE 21.7%、raw executed 23.0%減、出力一致。full selfhost全体の速度向上率は未測定。 |
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

## Portal詳細profile・コンパクト再現（2026-09-15）

**診断基盤を採用。ABI、nibble搬送、RemoteTransferの実装は変更していない。**
現状のproductionコードから短時間の再現を作れることと、搬送前の分解・要素選択・window交換を
別々に観測できることを確認した。profile付き生成BFは変更前のRust compilerとbyte単位で一致した。

### 測定条件

`tmp/portal-profile-evaluation`に12ケースの結果、`tmp/portal-profile-values`に搬送値を変えた
6ケースの結果を保存した。各directoryの`manifest.json`、`runner.py`、source/input、BF/map、
raw profile、`summary.json`と`report.md`が再現情報である。
通常測定はprofileなし、1回warmup後5組のON/OFF交互実行の中央値。前者は遅い設定で約2秒、
後者は約0.5秒を目安に校正した。各processは60秒timeout。
Rust/interpreterはrelease build、D=16、unlimited tape、同一BF・入力を使用。
sample/countersは別実行。compile・parse・RSSと全runの値も記録した。
長時間のcompiler自己入力は実行していない。

### 得られた内訳

| ケース | 圧縮BF bytes | RemoteTransfer ONの主なexclusive sample |
| --- | ---: | --- |
| 7byte要求搬送、padding 16セル | 1,263 | bit分解93.4%、搬送3.0% |
| 7byte要求搬送、padding 256セル | 1,368 | bit分解92.7%、搬送3.1% |
| 16要素×3セルのglobal配列 | 35,330 | 分解23.3%、要素選択20.2%、window交換15.0% |
| 16要素×3セルのframe配列 | 30,676 | 要素選択32.3%、window交換20.8%、offset12.3% |
| 大きいglobal配列・chunk境界 | 183,678 | window交換43.4%、分解23.3%、offset10.4% |
| production optimizerの抜粋 | 99,517 | dispatch38.5%、分解17.6%、要素選択9.6%、window交換4.7% |

共有祖先へ融合した命令の費用は親へ残した。表はsubsetであり、残余を子へ推測配賦していない。
また、異なるケースのsample割合は分母が違うため、速度比として比較しない。

7byteの搬送fixtureでは各要求がnibble 6本＋unary 4本の計10回のRemoteTransferとなり、
全256値・両paddingでnative site帰属をテストした。測定した全ケースでfallbackは0だった。
これは「RemoteTransferになっていないため遅い」という説明を支持しない。
分解loopは転送とは別に残り、準備が主な費用になる条件を約1.3KBのBFで再現できた。

要求byteを7本とも同じ値にした場合の、profileなしのwhole-fixture平均は次のとおり。
初期化・観測・後処理も含むため、routerだけのlatencyではない。

| byte値 | ON ns/要求 | OFF ns/要求 |
| --- | ---: | ---: |
| 0 | 639 | 613 |
| 1 | 1,154 | 933 |
| 15 | 2,995 | 5,742 |
| 16 | 3,369 | 4,916 |
| 127 | 19,350 | 34,887 |
| 255 | 37,715 | 69,707 |

大きい値ではRemoteTransferの改善後も分解費用が残る。小さい値ではprobeの固定費用もあり、
必ずONが速いとは限らない。この表は当該runの中央値であり、実際の要求値分布を代替しない。

### 判断と限界

- この時点では次の独立実験としてrouterのbit分解を省く搬送のA/Bを優先した。
  単純転送のnative化との優劣はこの測定だけでは確定せず、2026-09-16の独立比較で判断した。
- 小配列では16通りの要素選択も費用が大きい。小配列専用の選択やaggregate一括転送を
  次の候補として維持する。大配列ではwindow交換の比重が高く、別の条件で評価する。
- frame配列にはglobal routerがない。stackに置いても同じ費用になる、という説明は不正確。
  一方で要素選択・window交換は残るため、関数の固定領域化だけで配列の費用が消えるわけでもない。
- optimizerの抜粋はfull14とdispatch規模・PC値・call stack・仕事の割合が異なる。
  primitive fixtureの制御値はPCとしてdispatchされない。full14と同じ比率や改善率は主張しない。
- 全source fixtureをIR出力と照合し、配列のIR load/store数、全初期化payloadとcaller sentinelの保存を確認。
  ON/OFFの出力・logical BF/RLE count・max pointerも一致した。
- `cargo test --workspace`、全targetのClippy、profile追加前とのBF一致が通過した。

## Byte搬送をunary既定へ変更（2026-09-16）

**採用。Rust backendのnibble搬送を`--enable-nibble-transfer`で選択する方式にした。**
global scalarからframeへのcopy、およびglobal routerのoffset/accessor/resume low byteが対象。
source/CIR、通常BF/圧縮BF、profile付き出力で共通に指定する。
offset分解、base-16 window移動、frame/static配置とscratch予約は維持した。
stage2のBFC製backendは変更していない。

### full15の観測と判断

以下は変更前のfull15を読んだ時点の記録。後の実験で`full15.metrics`が上書きされたため、
現在の同名ファイルとは一致しない。独立A/Bのartifactは別directoryに保存している。

`full15.metrics`の210〜220分の差分では、routerの複数のnibble/unary siteが
それぞれ約2.7〜3.0万sample増え、global navigationも約1.7万増えている。
序盤のdecompose/exchangeだけを改善して終わる説明では不十分である。
ただし保存されているのは累積top10表示で、最終profile JSONはない。
ここから全siteの割合やnibble有無の実行時間差を確定してはいけない。

RemoteTransferはunary loopも一括実行できる。一方、nibble化は分解処理を追加し、
対象byteの転送を二つのloopに分ける。global要求7byteでは計10本から7本へ減らせる。
各loopに経路探索が必要なため、分解だけでなく長いstackでの搬送も比較した。
RemoteTransferのない実行では値による往復回数の削減が有利になる条件が残る。

### 独立したA/B測定

次のコマンドで測定した。

```sh
python3 scripts/portal-profile/compare-transfer.py tmp/nibble-transfer-evaluation \
  --baseline-compiler tmp/nibble-transfer-baseline/bfc
```

release build、unlimited tape、profileなし、warmup後5組のAB/BA交互実行。
RemoteTransfer ON/OFFを別々に校正し、それぞれの遅いvariantを約1秒にする。
同じ組の入力・反復数は同一。表は実行時間の中央値を入力record数で割った値であり、
初期化・観測・後処理も含む。長いstackは有効なframeに4,064個のlive caller chunkを追加したfixture。

| ケース | ON: nibble ns/record | ON: unary ns/record | ON時間変化 | OFF時間変化 |
| --- | ---: | ---: | ---: | ---: |
| global scalar snapshot | 13,744 | 874 | -93.6% | +16.7% |
| production optimizer抜粋 | 14,307,067 | 10,839,705 | -24.2% | -8.7% |
| global 16要素×3セル | 45,419 | 31,799 | -30.0% | -8.4% |
| 同配列・再帰8段 | 49,100 | 35,412 | -27.9% | -3.2% |
| 大きいglobal配列・chunk境界 | 42,125 | 30,885 | -26.7% | -9.1% |
| 搬送単体・padding 16 | 20,453 | 987 | -95.2% | -20.1% |
| 搬送単体・長いlive stack | 77,129 | 47,762 | -38.1% | +52.1% |

変化はnibbleを基準とし、負がunaryの短縮。全7ケースのONでpaired差のbootstrap 95%区間も
負になった。全source fixtureのnibble有効BFは変更前7367a75のcompilerとbyte単位で一致した。
通常のportal単体は1要求7本/10本のRemoteTransferとなることを全256値で検証。
長いfixtureは両方式ともrunあたり1回のfallbackを含み、それも時間・counterへ含めた。
生成BFサイズはoptimizerが99,517→94,952 bytes、小さい搬送単体が1,263→436 bytes。

### 検証と制約

- `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`が通過。
- source/CIRのglobal snapshot・動的配列アクセスを両方式・両RemoteTransfer設定で照合。
  通常/圧縮BF、profile/embedded mapのBF identity一致と、flagで生成BFが変わることを検証。
- 搬送単体の全256値、短い/長めのframe、native site帰属を両方式で検証。
- 測定は毎runの出力をIRまたは制御byte列と照合した。同じBFのRemoteTransfer ON/OFFで
  pilotのlogical BF/RLE命令数と最大pointerも一致した。
- `tmp/nibble-transfer-evaluation`に全pair、BF/map、入力/期待値、binary/source hash、script snapshot、
  bootstrap区間を保存。sample/countersの別診断は`tmp/nibble-transfer-diagnostics-unary`と
  `tmp/nibble-transfer-diagnostics-nibble`に保存した。診断の短い測定時間は速度判断に使わない。
- RemoteTransferがなくても、小さい値・短い距離では分解費用が勝つ場合があるため、
  「なしならnibbleが常に速い」とはしない。BF命令数とnative実行時間も区別する。
- full15のphase分布とfull selfhost改善率は未検証。長時間のcompiler自己入力は実行していない。
  navigationやwindow交換自体は残り、portal往復削減・専用laneは引き続き別の課題である。

## stage2 BF最適化器の不要なcountアクセス削減（2026-09-16、撤去前の測定）

`09_bf_optimizer.bfc`で、合併する分岐に入ってから必要なcountを読むよう変更した。
加算・奇偶判定はlow byteだけ、pointer移動は3byte、括弧等はcountを使わない。
書込みも同じkind契約に限定し、相殺されたtokenと変化しないkindへの書き戻しを省く。
最適化規則、16 tokenの容量、生成BFの形式は維持する。slotの未使用laneには古い値が残りうる。

`tmp/portal-next-review`に変更前/後のoptimizer抜粋sourceと生成BF、入力・出力、IR metricsを保存した。
production fixtureの4 blockで、実行されたIR load/storeのcellsを合計すると、
byte単位portal要求はload 2,008→1,020、store 1,324→764、合計3,332→1,784（46.5%減）。
この入力の最適化済み出力184 bytesは一致した。
80 blockを同じRemoteTransfer有効のinterpreterで、profileなし・warmup後5組AB/BA比較した参考値は、
execute中央値840→688 ms（約18%短縮）。全runの出力一致を確認した。
ログは`io-benchmark.*.log`、全測定値は`io-benchmark.json`、source/BF/binary/input hashは`io-manifest.json`。
長時間のcompiler自己入力やfull phaseの速度比較は実行していない。

`verify-stage2-bf-optimizer.py`は580ケースのIR/BF・通常/圧縮の照合と50プログラムの意味比較が通過。
全count laneを使った後にring slotを再利用する3ケースも、期待する命令列と照合する。
logは`tmp/portal-next-review/verification.log`。
二段portal配置・小配列accessor・interpreterの変更は本変更に含めない。

## 素朴なBFCRLE出力への復帰（2026-09-16）

full16について、ユーザーから生成コードの削減量に対してコンパイル時の負担が大きいとの報告。
移動run圧縮換算で約300→225 MiBという削減と、約26 MBの出力進行を根拠に、
最適化導入前の即時出力へ戻す。完走時間が約10倍という予想は実測で確認した値ではない。
kind/count配列とflush/resetを撤去し、反復回数の可逆な圧縮のみを行う。
整数幅の修正・診断・Rust側の最適化は維持する。full selfhostの再実行は行わない。
`portal-profile`の`optimizer`というcase名は既存コマンドとの互換用に残すが、
現在は即時serializerを実行するため、旧測定とは処理内容・出力が異なる。

検証結果:
- `verify-selfhost-compressed.py`: 全サンプルの通常/圧縮命令列一致、全byte反復数・24-bit境界のIR/BF一致。
- `verify-stage2-selfhost.sh`: 小入力のコンパイル・生成BF実行を含むstage2回帰検証成功。
- `verify-stage2-limits.py`: 301関数、255/256境界、frame上限、overflow拒否、16-bit offset加算の検証成功。
- `portal-profile`の出力fixture: 4 blockのIR/BF出力一致（420 bytes）。
- serializer本体は`0400bc4`の実装との一致を確認。helloは通常848 bytes、圧縮276 bytes。
ログは`tmp/simple-output-{compressed,stage2,limits}.log`。
