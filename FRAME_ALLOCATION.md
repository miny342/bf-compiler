# 生存解析によるframe領域の再利用

Rust版compilerのsource frontendでは、HIRをContinuation IRへloweringした後、関数ごとに
`frame_allocation::allocate`を実行する。HIRのlocal名・型はこの時点では必要ない。
`FrameSlot`をvirtual register、frame aggregateを分割しないvirtual regionとして扱う。

## 解析と割り当て

- 通常のcontinuation successorに加えて、call/portalのresume edgeをCFGへ含める。
  calleeは別frameなので、callerでreturn後に必要な値はresume edgeを通して生存する。
- 構造化`Loop`にもback edgeを作り、`Branch`は両枝を解析する。
  structured branchのconditionはbackendがbranch終了まで使用するため、その期間は再利用しない。
- worklistで `live_in = uses ∪ (live_out − defs)` の固定点を求める。
- definitionとlive-out、同じ命令のoperand間に干渉edgeを作る。破壊的`Transfer`、copy、
  portalの入出力を新たにaliasさせない。parameter同士とentryで生存する初期値も干渉させる。
- 干渉数の多い領域から決定的なgreedy coloringを行い、scalarとaggregateを分けて配置する。
  aggregateは同じpayloadサイズのものだけを共有する。protocol、head、paddingも領域と一緒に共有される。
- parameter、命令、terminatorの参照とfunction descriptorを更新し、最終IRをconstructorで検証する。

aggregateの部分書き込みは領域全体のread/writeとして保守的に扱う。全体copy、全体load、
loweringが生成する連続した全要素`Set`による初期化では古い値をkillできる。
そのため、別scopeのarrayやstructと、その評価用temporaryも再利用できる。
regionをcellごとに解析しないので、大きなarrayでもlivenessの変数数はpayload長に比例しない。

これは最小値を保証する最適彩色ではない。異なるサイズのaggregateの重ね合わせ、aggregate内の
個別fieldへの分割、copy coalescing、dead store除去は行わない。`--cir-input`のflat frameや、
公開APIで手動構築したIRにも自動適用しない。source loweringが作る独立したlocal領域が対象である。

## インライン化の採否

単一の静的call siteから呼ばれるvoid関数で、再帰せず、末尾以外にreturnがないものを候補にする。
parameterとlocalをfresh IDへrenameし、引数の左から右への評価順序を保ってbodyを挿入する。
到達可能性とcall siteの収集は同じHIR walkerを使用する。

候補を試験的にCIRへloweringして領域を再利用し、callerのframe chunk数を変更前と比較する。
D=8とD=16の両方で増加しない場合だけ採用する。見積もりにはbranch temporary、portal temporary、
outbox、aggregate protocolとalignmentを含める。program全体で共通のroute領域は比較から省く。
採用したcallerの見積もりはcacheする。

これは「call時の最大stackが小さければよい」という判定より保守的である。calleeをinline化すると、
その領域はcallerの全実行期間に常駐する。calleeを呼んでいない時間もglobal navigationが余分な
frame chunkを走査するため、caller自身のframe増加を抑える。

## 回帰テスト

`frame_allocation`のテストは、同じsourceを再利用前・再利用後、inline化前・後のCIRへloweringする。
CIR VMの結果を比較し、割り当て後はD=8/16のBrainfuckでも期待する出力を確認する。
loop back edge、再帰callをまたぐ値、短絡評価、aggregateの部分更新、argument snapshot、
portal offset、繰り返す初期化、inline化によるframe膨張の抑止を含む。
100個の独立したscalar scopeでは、300以上のvirtual slotが4以下のphysical slotへ収まることも確認する。

## selfhost測定

入力は `logs/full-selfhost-20260907-013902/stage2-compiler-test.bfc`。
元の2行はユーザー提供のJSON profile、残りは同じソースをこの変更でcompileして実行した結果。
`abi.navigation.global`は同じstable keyを持つsiteのexclusive durationの合計。
サンプリング値であり、単回測定の実行時間には測定環境による揺れがある。

| 版 | 実行時間(s) | global navigation(s) | scan steps | maximum pointer |
|---|---:|---:|---:|---:|
| 元・inlineなし | 51.75 | 41.61 | 11,367,337,178 | 1,166,226 |
| 元・inlineあり | 138.62 | 125.69 | 34,282,738,796 | 1,192,287 |
| scalar再利用のみ | 137.32 | 122.90 | 32,304,112,410 | 1,189,669 |
| scalar＋aggregate再利用 | 27.63 | 14.78 | 3,945,939,130 | 1,126,973 |
| 最終版（再利用＋frame増加を避けるinline化） | 27.04 | 13.76 | 3,617,018,080 | 1,126,888 |

scalarだけの再利用では改善が小さく、aggregateの再利用がglobal navigationの削減に大きく寄与した。
新規測定の3版はいずれもselfhostテストを完走し、出力は `ok\n` だった。

測定ファイルは `logs/frame-allocation-20260907/` に保存した。
`scalar-inline` はscalarのみ再利用、`allocated-inline` はaggregateも再利用、
`guarded-inline` はさらにcaller frameの増加を避けた最終版を指す。
この入力では元の298関数のうち36関数をinline化し、残る262関数のscalar領域合計は965セル
（元は8,054セル）、最大scalar領域は19セル（元は257セル）になった。
合計は全function descriptorの和であり、実行時の最大stackではない。

再実行する場合は、新しい出力directoryを用意して以下を実行する。

```sh
cargo build --release -p bf-compiler -p bf-interpreter
mkdir -p logs/frame-allocation-rerun
target/release/bfc --unlimited-tape \
  --profile-map-output logs/frame-allocation-rerun/test.bfmap.json \
  --profile-granularity abi \
  logs/full-selfhost-20260907-013902/stage2-compiler-test.bfc \
  > logs/frame-allocation-rerun/test.bf
target/release/bf-interpreter --unlimited-tape \
  --profile-map logs/frame-allocation-rerun/test.bfmap.json \
  --profile-mode sample --profile-format json \
  --profile-output logs/frame-allocation-rerun/test.profile.json \
  logs/frame-allocation-rerun/test.bf \
  > logs/frame-allocation-rerun/test.output
```

selfhostテストの期待する出力は `ok\n`。BF fileは約15GBあり、生成時のRust compilerの
peak RSSは約30GBになる。表の実行時間はinterpreterの`execute` phaseで、BFの生成・読込・parse時間を含まない。
