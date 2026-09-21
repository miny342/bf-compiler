# CIR Inspection and Visualization Design

この文書は、Continuation IR（CIR）の構造を人間が確認するための、読み出し・疑似コード表示・
グラフ表示機能の仕様案である。実装済みの仕様ではなく、CIR inlineやcontinuation削減の判断材料を
得るための開発者向け機能を対象とする。

## 目的

CIRについて、少なくとも次の疑問に答えられるようにする。

- どの関数が存在し、どの関数を呼んでいるか
- 関数が何個のcontinuationに分割されているか
- 各continuationのbodyとterminatorは何か
- call、return、branch、loop、portalがどこでcontinuationを分割しているか
- どのcontinuationがhotか、どのedgeが頻繁に通るか
- どこがCIR inline候補で、inlineするとどのcontinuationがcallerへ入るか
- 関数名やsource位置を、IDではなく元のBFC sourceへ戻せるか

BrainfuckやABIの命令列を読むことが目的ではない。CIR上の関数・frame・continuation・制御フローを、
通常のプログラミング言語に近い形で読めることを第一にする。

## 採用方針: browser-firstのローカルviewer

主な表示手段は、`.cir`と対応するsidecarをブラウザへドラッグ&ドロップして調べる、ローカル完結の
viewerとする。DOTはCIやテキストレビュー用のexportとして残すが、最初のユーザー体験にはしない。

想定する構成は次のとおり。

- Vite + TypeScript + Reactの静的frontend
- call graphとfunction CFGの表示に`@xyflow/react`を使う
- `.cir`は`ArrayBuffer`/`DataView`でブラウザ内デコードする
- `bfmap.json`、IR metrics、source `.bfc`は`File.text()`で読み込む
- サーバー、compiler実行、BF実行、外部通信は不要とする
- `npm run build`で生成した静的ファイルをWSLからWindows側ブラウザで開けるようにする

これにより、普段はWSLで`npm run dev -- --host`を起動し、Windowsのブラウザからアクセスできる。
配布時は`dist/`を静的サーバーで開けばよく、後から必要になった場合だけ同じfrontendをTauriで包む。
Tauri固有のファイルアクセスやRust実装を最初から必須にしない。

DOT、JSON、疑似コードはviewerのdownload/exportおよびCI用の出力とする。canonicalなのはdecoderから
作る`InspectionProgram`であり、表示形式そのものではない。

### 依存とサプライチェーン

依存は少数に限定し、初期段階ではReact、Vite、`@xyflow/react`を中心にする。自動layoutのための
追加ライブラリや大きなUI frameworkは、必要性を確認するまで入れない。

- package versionは固定し、`package-lock.json`を必ず保存する
- CIと再現環境では`npm ci`を使い、install scriptは原則無効化する
- `npm audit --audit-level=high`と`npm ls --all`を依存追加時に確認する
- runtime CDN、remote font、telemetry、不要なpostinstallを使わない
- lockfileの差分と新規依存のライセンス・公開元をレビューする

## 入力形式と情報の境界

### `.cir`単体から取得できる情報

現行のself-host CIR形式は、少なくとも以下を保持している。

- programのstatic cell数とmain function
- function ID、entry continuation、frame cell数、戻り値型、parameter配置
- continuation IDと所有function
- frame instructionの列
- terminator
- call先、callの引数、return continuation
- branch/gotoの静的successor
- aggregate/global accessのstorage、base、offset、cell数

したがって、次の機能は`.cir`だけから実装できる。

- function call graph
- functionごとのcontinuation CFG
- continuation bodyの疑似コード表示
- static in-degree / out-degree
- recursive SCCの検出
- call siteとreturn edgeの表示
- portal・array operationの表示

### `.cir`単体では不足する情報

現行のself-host CIR binaryには、source-level function nameとsource spanが含まれていない。したがって、
現状は`function 12`、`continuation 348`のような表示までしか保証できない。

関数名やsource位置を表示する方法は、次の優先順位にする。

1. CIRへdebug metadataを埋め込む
2. CIRと同時に生成したsidecar manifestを読む
3. profile mapや別のIR metrics JSONを明示的に指定する
4. 情報がなければIDをそのまま表示する

名前を正しく表示するために、別runのIR対応表を暗黙に使ってはいけない。function IDはreachability、
inline、function remap、continuation optimizationで変わるため、異なるartifactの対応表を使うと
誤った関数名を表示する。

## 推奨する出力モデル

browser、DOT、textを個別に直接生成するのではなく、まずCIRを次のinspection modelへ変換する。

```text
InspectionProgram
  metadata
  functions[]
    id, name, entry, frame, parameters, return_type
    continuations[]
      id, body[], terminator
      source_span?
      static_edges[]
      execution_count?
  call_edges[]
  continuation_edges[]
  sccs[]
```

この中間モデルから、browser用JSON、text、DOTを生成する。出力順はID順または明示した安定順に固定し、
同じCIRから生成した結果が毎回大きく変わらないようにする。

## Call graph

### ノード

functionごとに1ノードを作る。ノードには最低限、次を表示する。

```text
fn#12 read_number
entry: c348
frame: 41 cells
return: cell
continuations: 17
```

名前がなければ`fn#12`、source spanがあれば代表的なsource位置を併記する。nodeの色や太さは、
metricsが指定された場合だけ実行回数またはexclusive durationを反映する。

### エッジ

call siteごとにfunctionからcalleeへのエッジを作る。エッジには以下を付ける。

- caller continuation ID
- callee function ID/name
- callerのreturn continuation ID
- static call site数
- metricsがあればcall回数

同じcaller/calleeへの複数call siteは、既定では一本にまとめ、ラベルに`sites=...`を表示する。詳細表示
ではcall siteごとに分離できるようにする。

### SCCと再帰

再帰や相互再帰はSCCとして検出する。DOT/browserではSCCをclusterまたは折りたたみノードとして表示し、
次を区別する。

- self recursion
- mutual recursion
- SCC外からSCCへのentry
- SCCから外へのcall

CIR inline候補の検討では、recursive SCCは既定でinline不可として表示する。

## Function CFG

call graphとは別に、functionを選択すると、そのfunctionに属するcontinuationだけでCFGを表示する。

### ノード

continuationごとに一つのノードを作る。

```text
c348 [entry]
  f[2] = global[17]
  f[3] = compare(f[2], 0)
  branch f[3] ? c350 : c351
```

ノードには最低限、次を表示する。

- continuation ID
- entryかどうか
- body instruction数
- terminator種別
- static in/out-degree
- metricsがあれば実行回数、exclusive/inclusive duration
- source spanまたはsource line

### エッジ

- `Goto`: 通常エッジ
- `Branch`: then/elseを明示した二本のエッジ
- `Call`: callee entryへのcallエッジと、caller内のreturn continuationへのresumeエッジ
- `ArrayLoad` / `ArrayStore` / aggregate portal: portal/accessor/resumeの関係を注記
- `Return` / `Halt` / `Abort`: terminal edgeまたは端子として表示

call graphとCFGを混ぜると巨大なグラフになるため、既定ではfunction境界を越えるcallは点線にし、
callee内部は展開しない。`--expand-calls N`で最大N段だけ展開できるようにする。

## Bodyの疑似コード表示

bodyはBF命令ではなく、frame-relativeな命令として表示する。表示はBFCの完全な再構成ではなく、
「何がcontinuationを跨がない命令か」を確認するための正規化された疑似コードとする。

例:

```text
fn#12 read_number() -> cell {
  c348:
    f[2] = 0
    f[3] = input()
    while (f[3] != 0) {
      f[2] += f[3]
    }
    call fn#7 times_ten() -> c349

  c349:
    f[4] = abi.value
    if (f[4] != 0) goto c351 else goto c350

  c350:
    return f[2]
}
```

表記規則:

- frame slotは`f[n]`
- globalは`g[n]`または名前付きmetadataがあれば`g[name]`
- `AbiValue`は`abi.value`
- array/aggregateは`array(storage, base, offset, cells)`として表示
- `Copy`、`Transfer`、`Compare`、`SubWithBorrow`は意味を失わない範囲で一行にまとめる
- nested `Loop` / `Branch`はインデントして表示する
- terminatorのcallとbody内のframe instructionを混同しない

必要であれば、同じfunctionについて次の二つを切り替えられるようにする。

1. `--body=summary`: instruction種別と個数だけ
2. `--body=expanded`: 全instructionとoperandを表示

## Browser viewer

ブラウザviewerは単一artifactを一度に読むのではなく、同じ調査セッションへ複数ファイルを追加する。
最初から全function CFGを描画せず、summaryから選択したfunctionだけを詳細表示する。

### 入力とD&D

drop zoneは次のファイルを受け付ける。

- `.cir`: 必須のCIR本体
- `*.bfmap.json`: backend profile map。source spanやBF siteの補助情報
- `*.ir-metrics.json`: function/continuation名、実行回数、遷移数などの補助情報
- `.bfc`、またはsource directoryのzip: source表示用。初期版では任意
- 将来のdebug metadata manifest: CIRとartifact identityを検証するsidecar

拡張子だけでなく、CIR magicとJSONのshapeも確認してファイル種別を判定する。複数の候補がある場合は
自動で黙って結び付けず、session manifestに一覧を出し、ユーザーが対応関係を選べるようにする。
別runの`bfmap.json`やmetricsを読み込んだ場合は、identityが確認できなければ「参考情報」として表示し、
function IDを名前へ自動変換しない。

解析はすべてブラウザ内で行う。読み込んだファイルをuploadせず、compilerやinterpreterも起動しない。
parse error、version mismatch、metadata不一致は画面上部に警告として残し、静的CIRの表示自体は可能なら
継続する。

### 画面

初期画面は次の三領域とする。

1. 入力ファイル、artifact identity、警告、function検索のsession pane
2. function call graphを表示するgraph pane
3. 選択したfunctionのCFG、body、source、metricsを切り替えるdetail pane

必要な操作は次のとおり。

- call graph / function CFGの切り替え
- pan、zoom、node選択、edge選択
- function名、ID、source行、continuation IDで検索
- SCC、callee、unreachable nodeの折りたたみ
- call edgeからcallee、return continuation、call siteへ移動
- hotnessによるnode/edgeの色付け。metricsなしでは静的表示
- summaryとexpanded bodyの切り替え
- `InspectionProgram`、DOT、疑似コード、警告一覧のdownload

React Flowにはfunction/continuation用のcustom nodeを渡し、node positionは初期版では安定した簡易layoutで
決める。巨大なlayout用依存を増やす前に、SCCをまとめた階層表示と選択functionの遅延描画で対応する。

### 表示の粒度

call graphではfunctionをノードにし、function境界を越えるedgeは点線にする。CFGではcontinuationを
ノードにし、`Goto`、`Branch`、`Call`、`Return`をedgeの種類として表示する。callee内部は既定で展開せず、
「calleeを開く」操作でdetail paneを切り替える。`expand-calls`相当の多段展開は、画面を破壊しない上限を
設けてから追加する。

bodyはBF命令ではなくframe-relativeな疑似コードで表示し、frame slot、global、array、terminatorを
確認できるようにする。sourceが対応しない場合でも、`fn#ID`と`c#ID`で必ず表示できる。

## DOT / Graphviz

自動化とCIで扱いやすいように、browserとは別にDOT出力を提供する。

```text
cir-inspect program.cir --graph call --format dot > callgraph.dot
cir-inspect program.cir --graph function --function 12 --format dot > fn12.dot
```

DOTでは次をサポートする。

- `--graph call`: function call graph
- `--graph cfg`: 一つのfunctionのcontinuation CFG
- `--graph combined`: call graph上で選択functionのCFGを展開
- `--metrics report.json`: dynamic countとhotnessをedge/nodeへ付加
- `--hide-unreachable`: entryから静的に到達しない要素を隠す

Draw.io XMLは、DOTで表現できないbodyやsource metadataを失いやすいので、必要になった場合に
InspectionProgramから追加生成する。最初からDraw.ioをcanonical formatにはしない。

## Metricsとの統合

静的CIRと実行metricsは別入力として扱う。

```text
cir-inspect program.cir \
  --metrics program.ir-metrics.json \
  --profile-map program.bfmap.json \
  --format json > inspection.json
```

IR metricsには、可能なら以下を含める。

- function ID/name/entry
- continuation IDとfunction ID
- static in/out-degree
- executed continuation count
- dynamic transition count `from -> to`
- call/return/portal terminal count
- optimizer前後のcontinuation数
- duplicated frame instruction数

runtime profile mapのABI siteとCIR continuationを直接同一視してはいけない。profile mapはBF backendの
siteであり、CIR metricsはCIR interpreterのsiteである。対応づける場合は、明示的なstable keyまたは
source spanを使う。

## Metadataの仕様案

function名とsource位置を確実に表示するため、CIR v2以降ではdebug metadataをoptional recordとして
追加する。

最低限:

```text
FunctionMetadata {
  function_id
  name
  source_file_id?
  source_start?
  source_end?
}

ContinuationMetadata {
  continuation_id
  function_id
  source_file_id?
  source_start?
  source_end?
}
```

debug metadataが無効なproduction用CIRでは、既存のbinary payloadを増やさない。代わりに同じartifact
identityを持つmanifest sidecarを使えるようにする。

artifact identityは、CIR bytes、format version、lowering options、source file order/optionsから計算し、
異なるrunのmetadataを誤って適用した場合は警告またはエラーにする。

## CLI案

decoderと`InspectionProgram`はRust側の共有ライブラリに置き、browser viewerとはJSON境界で接続する。
browserを基本UIとするが、CIや差分確認のためにread-onlyなCLIも用意する。既存の`bfc`へ統合するか、
独立した`cir-inspect` binaryにするかは実装時に決める。

想定コマンド:

```text
bfc --inspect-cir program.cir --format text
bfc --inspect-cir program.cir --format dot --graph call -o callgraph.dot
bfc --inspect-cir program.cir --function 12 --body expanded
bfc --inspect-cir program.cir --metrics program.ir-metrics.json --format json > inspection.json
```

viewer側は次のように起動する。

```text
npm ci
npm run dev -- --host
npm run build
```

`dist/`は静的ファイルとして扱う。WSLでdev serverを起動してWindows側ブラウザから開けること、
ネットワークなしでbuild済みviewerが表示できることを受け入れ条件にする。Tauriは`dist/`を再利用する
任意のWindows配布手段として後段で追加する。

必要な入力がない場合の表示:

- function nameなし: `fn#12`
- source spanなし: `file:? bytes:?`
- metricsなし: static graphとして表示
- metadata identity不一致: 明示的にエラー

inspectはCIRを実行しない。実行が必要な場合は既存の`--run-ir`を使い、inspectは常にread-onlyである。

## 大規模CIRへの対策

full self-host級では、全function CFGを一枚の画像にすると読めない。既定の表示は次のようにする。

- call graphは全体表示可能だが、SCCを折りたたむ
- function一覧はhotnessまたはcontinuation数でsort
- CFGはfunction単位で遅延表示
- bodyはsummaryを既定にし、選択時だけexpanded
- `--top N`、`--function NAME/ID`、`--source FILE:LINE`で絞り込む
- `--path fnA,fnB,...`でcall pathを限定する
- `--diff old.cir new.cir`は将来拡張とし、まずは単一artifactを確実に表示する

## 実装フェーズ

### Phase 1: 静的text dump

- `.cir` decoder
- function一覧
- functionごとのcontinuation一覧
- body疑似コード
- call graphのedge一覧
- IDだけでも動作すること

### Phase 2: browser MVP

- Vite/TypeScriptの静的アプリ
- `.cir`のD&Dとブラウザ内decoder
- function一覧とcall graph
- function選択時のCFG、body、ID fallback
- parse errorとsidecar不一致の表示

### Phase 3: metadata、metrics、source対応

- `bfmap.json`、IR metrics、sourceのsession入力
- function/continuation metadata
- sidecar identity検証
- source name/line/column表示
- hot continuationとhot edgeの強調

### Phase 4: exportと配布

- InspectionProgram JSON、DOT、疑似コードのdownload
- CIから使えるtext/DOT/JSON CLI
- `npm run build`とWindowsブラウザ利用の検証
- 必要性が出た場合のみTauri package

### Phase 5: CIR inline支援

- inline候補表示
- call siteからcallee CFGをプレビュー
- inline後のcontinuation数、frame slot、call edgeの推定
- 実際のinline passとは分離し、まずinspect-onlyで導入する

## 検証項目

- 小さなscalar callのcall graphが正しい
- direct recursion / mutual recursionがSCCとして表示される
- if/whileのthen/else/back edgeが正しい
- callのreturn continuationがcaller側に表示される
- array/global portalがcallと混同されない
- function name metadataがないCIRでもID fallbackで壊れない
- malformed `.cir`を実行せずに診断できる
- 同じ入力からdeterministicなtext/DOT/JSONが生成される
- profile/metricsのartifact identity不一致を検出できる
- full self-host規模でも全CFGを一度に展開せず表示できる

## 最初の実装範囲

browser MVPでは次だけで十分である。

1. `.cir`からInspectionProgramを構築してJSONで渡す
2. `.cir`をD&Dし、function ID、entry、continuation、call edgeを表示する
3. functionごとのCFGを選択表示する
4. bodyを疑似コードで表示する
5. function nameが存在する入力では名前を使い、なければIDへfallbackする
6. `bfmap.json`やmetricsの不一致を警告し、黙って誤結合しない

この段階で、CIR inlineを実装する前に「どの関数がどこでcontinuationを跨いでいるか」を確認できる。
DOTやTauriは後からInspectionProgramへ追加できるため、最初から可視化全体を大きく作り込まない。
