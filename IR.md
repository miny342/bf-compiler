# 中間表現の設計

この文書は、Rust版`bf-compiler`でソース言語からBrainfuckを生成するまでの実装、
各中間表現（IR）の責務、最適化を置く層、今後IRを追加する条件を記録する。

以下では、実装に存在するものを「現行」、まだ存在しないpassやIRを「提案」と明記する。
型名と公開範囲は現在のRust実装を基準とする。設計候補を、実装済みであるかのようには記述しない。

既定有効の CFG region emission と実 BF dispatcher 訪問数の比較は
[CIR_REGION_EMISSION.md](CIR_REGION_EMISSION.md) を参照。yielding control は通常 CFG に統一し、
旧 `BranchWithBodies` は削除した。
allocation 前の CIR inline と virtual result の実装・比較は [CIR_INLINE.md](CIR_INLINE.md) を参照。

## 全体構成

コンパイラは、構文木から直接Brainfuck文字列を生成しない。source frontendと低水準APIは、
それぞれ用途の異なるpipelineを持つ。

```text
source/CLIの本線:
  BFC source text
    → tokens
    → AST
    → macro expansion
    → typed HIR
    → 全 reachable function の unallocated Continuation IR
    → virtual inbox/outbox normalization / frame-aware CIR graph inline / virtual cleanup
    → CFG cleanup / local reconstruction
    → function ごとの frame fusion / liveness-based frame allocation
    → allocation 後の conservative CFG cleanup
    → soft-region emission plan / ABI layout / portal planning / BF template selection
    → BF IR（内部ではprovenance付きvariantも使用）
    → BF peephole optimization
    → Brainfuck source text

low-level API:
  Cell IR → static-cell backend → BF IR → optimize → Brainfuck
```

「Raw LANG」と呼びたくなる最初の段階は、正式なIRではなくUTF-8のBFC source textである。
同様に「Raw BF」は`BfProgram::to_source`が返す文字列である。token列とASTもコンパイル途中の
表現だが、この文書では、解析後も意図的な契約を持つHIR以降をIRとして扱う。

実際の主要な型と入口は次のとおりである。

| 段階 | Rust上の表現・処理 | 保持する情報 | この段階で確定または失われる情報 |
|---|---|---|---|
| source | `&str` / `SourceFile` | source byteとfile名 | なし |
| token | `Vec<Token>` / `lexer::lex` | token種別、byte offset | 空白とcomment |
| AST | `ast::AstProgram` / `parser::parse` | sourceに近い構文、名前、macro | 括弧など一部の表記差 |
| expanded AST | `macro_expansion::expand` | 展開済みblock、fresh local identity | macro呼出し |
| typed HIR | `hir::HirProgram` / `semantic::analyze` | 解決済みID、nominal型、layout、評価順序、構造化制御フロー | 名前探索、method call糖衣、`len`などのcompile-time構文 |
| Continuation IR | `ContinuationProgram` / `continuation_lowering::lower_hir` | frame-relative storage、基本block相当のcontinuation、call/return/portal境界 | sourceのnominal型、local名、式木 |
| BF IR | `BfProgram`または`AnnotatedBfProgram` / ABI backend | 相対pointer移動、cell加算、I/O、BF loop、任意のprofile provenance | function、frame、continuationという意味 |
| BF source | `String` / `to_source` | `><+-.,[]`列 | IR node境界とprovenance |

通常のprofileなし経路でも、ABI emitterは内部で`AnnotatedBfProgram`を組み立ててからplainな
`BfProgram`へ変換する。profile付き経路はannotationを保ったままpeephole optimizationを行い、
sidecar mapを作る。このannotationは新しい意味IRではなく、同じBF命令に出自を付けたvariantである。

scalar関数、再帰、frame-relativeなローカル変数には、typed HIRとContinuation IRを使用する。
chunked frame stack、call/return、pointer位置の規約は[ABI.md](ABI.md)に定義する。現在のsource
frontendは、名前解決・型検査済みHIRから関数ごとのframe slotとContinuationを生成し、ABI backendへ
渡す。低水準APIとして、静的`CellId`を使用する従来のCell IRとbackendも独立して残すが、これは
source frontendまたは`bfc` CLIの途中に挟まる層ではない。

公開APIでは`lower_source`が`ContinuationProgram`を返し、`compile_source`のbackend errorは
`SourceCompileError::AbiCodegen`として報告する。以前の`Program`を返すsource lowering APIと
`SourceCompileError::Codegen`からは互換性のない変更である。手動で構築した静的Cell IRには、
引き続き`compile(&Program)`または`lower(&Program)`を使用する。

IRを分ける目的は、ソース言語の意味、関数の制御フローとframe-relative storage、ABIの物理配置、
Brainfuckのdata pointerと相対移動を分離することである。

現在の実装はLANGUAGE.mdの第12段階までであり、以下のenum、struct、macro、多段projection、
16-bit aggregate offsetを含むversion 1 IRを使用する。

## Source AST、展開、typed HIR

parserは型定義、文字列、field/index postfix、method call、macro定義・呼出しを保持したASTを
作る。runtime IRへ進む前に次のcompile-time処理を順に行う。

1. macro定義を収集し、block macroをfreshなlocal identityを持つblock ASTへ展開する。
2. semantic analysisがトップレベルの型、定数、global、function名を収集する。
3. type layoutと`cell[]`文字列宣言の長さを確定し、`len`と`const cell`を評価する。
4. local/global/functionの名前解決と型検査を行う。
5. `receiver.function(args)`をreceiverが先頭引数のfunction callへ変換し、nominal typeと評価順序を
   持つtyped HIRを作る。

macro、method call、`cell[]`、`len`はこの段階で消費され、Continuation IRまたはABIへ専用の
runtime機構を追加しない。文字列リテラルは型確定後にaggregate定数初期化として残す。
arena、paged handle、多倍長整数も通常のstruct、array、global、functionとしてloweringし、
専用のHIR命令やABI objectを定義しない。

### 型とlayout

source型はcompilation unit内でstableな`TypeId`で参照する。`TypeTable`内の各`TypeDefinition`が
nominal kindとflatten後のcell数を持ち、structの各fieldが自身のcell offsetを持つ。

```rust
enum TypeKind {
    Cell,
    Void,
    Enum { variants: Vec<EnumVariant> },
    Struct { fields: Vec<Field> },
    Array { element: TypeId, length: usize },
}

struct TypeDefinition {
    name: String,
    kind: TypeKind,
    cells: usize,
}
```

enumはnominal情報をHIRまで保持するが、layout後は1 cell scalarとして扱う。struct field offsetは
宣言順の先行fieldのcell数合計、配列strideは要素型のcell数である。多次元配列はarray型の
再帰として表し、sourceの`T[N][M]`は`Array(N, Array(M, T))`になる。logical layoutは
row-majorで、structは宣言順にflattenする。zero-length arrayは0 cell aggregateである。

layout計算にはchecked arithmeticを使用する。型cycle、host整数overflow、実体化時にtarget tapeへ
配置できない大きさは、backendまで黙って持ち越さずcompile errorにする。

### Place projection

変数、field、配列indexを別々の式variantへ固定せず、一つのrootとprojection列として保持する。

```rust
struct HirPlace {
    root: VariableRef,
    projections: Vec<Projection>,
    ty: TypeId,
}

enum Projection {
    Field {
        cell_offset: usize,
    },
    Index {
        index: ArrayIndex, // Constant(u8) | Dynamic(HirExpression)
        length: usize,
        element_cells: usize,
    },
}
```

`nodes[page][slot].kind`は一つの`HirPlace`であり、途中のarrayまたはstructへのruntime referenceを
生成しない。定数indexとfield offsetは可能な限り一つの定数offsetへ畳み込む。動的indexは
projection順にそれぞれ1回評価し、aggregate rootからのlogical flat offsetをbackend用temporaryへ
計算する。

function callなどplaceでないaggregate式へfield/index postfixを適用する場合、HIRは
`HirExpressionKind::Project { base, projections }`として保持する。Continuation loweringがbase式を
一度だけcompiler temporaryへmaterializeし、そのregionにprojectionを適用する。`len`はsemantic
analysisで消費され、runtime評価もtemporary生成も行わない。

placeの読み出し結果が複数cellならaggregate value、1 cellならscalar valueになる。代入ではRHSを
完全にsnapshotしてから、LHSの動的projectionを左から右に評価する。この規則により、RHSやindexが
同じaggregateを参照する場合もsource semanticsを保つ。

HIRはcrate-privateであり、公開APIから直接取得できない。`lower_source`はASTとHIRを通過した後の、
検証済み`ContinuationProgram`を返す。現行の独立したHIR optimizer passはない。
`constant_cell_value`による副作用のないcell式の評価と、定数`if`/`while`の除去だけは、
HIRからContinuation IRへのlowering中に行う。

## Continuation IR

`ContinuationProgram`は、source frontendが生成する公開IRである。関数とglobalのdescriptor、
および全continuationを持ち、constructorで参照先、型、frame範囲、aggregate範囲、call/returnの
整合性を検証する。

```rust
struct Continuation {
    id: ContinuationId,
    function: FunctionId,
    body: Vec<FrameInstruction>,
    terminator: Terminator,
}
```

一つのcontinuationはdispatcherから見たstraight-line entryであり、末尾に必ず一つのterminatorを持つ。
通常のCFGのbasic blockに近いが、`FrameInstruction::Loop`と`FrameInstruction::Branch`は
BF templateへ直接落とすための構造化bodyを内部に持てるので、厳密なthree-address basic blockではない。

### Storageと命令

`Address`は物理tape位置ではなく、current frameの`FrameSlot`、static `GlobalId`、aggregateの定数要素、
またはABI value cellを指す。`AggregateRegion`はcurrent frame、global、return outboxを区別する。
sourceのenumやstructというnominal型は消え、`ValueType::Cell`またはcell数だけを持つ
`ValueType::Aggregate`になる。

`FrameInstruction`は次を表す。

- `Set`、`AddConst`、sourceを保存する`Copy`、破壊的な`Transfer`
- unsigned大小比較の`Compare`と、差・borrowを同時に生成する`SubWithBorrow`
- protocol cellを除外した`AggregateCopy`
- byte単位の`Input`と`Output`
- BFへ構造的にloweringできる`Loop`と、条件を消費する`Branch`

temporary cellはsource localと同じ`FrameSlot`として表される。現行lowererはlocal storageを先に割り当て、
式評価で必要になるvirtual temporary cellとaggregate regionを単調に追加する。その後、
`frame_allocation`がCFG上の生存期間を解析し、scalar slotと同じサイズのaggregate regionを再利用する。
source localとtemporaryを区別せず、同時に必要な値が同じ領域へ割り当てられないようにする。

### 局所的な算術fusion

`frame_fusion`はsourceのframe割当て前、およびbinary CIRをFrame命令へ変換した後に適用する。
同一の直列領域内でコピーと値の更新を追跡し、`a < b`（結果反転も可）に続く
`a - b`が同じ入力snapshotを使う場合、`SubWithBorrow`へ融合する。
関数名、ソース言語の型追加、intrinsic、binary CIRの新opcodeには依存しない。

`SubWithBorrow`は入力をsnapshotし、両入力を0にしてから、modulo 256の差、指定された
borrow値の順に出力する。全operandのaliasを許し、出力同士がaliasする場合はborrowが残る。
ABI backendは比較countdownの残りを差として使う。既存の`Restore`と`Scratch0..3`を
使用し、終了時に作業セルを0に戻す。

差の出力先を比較位置で先に書いても途中で観測されない場合は、最終出力先へ直接生成する。
それ以外は一時slotに保存し、元の減算位置で書き戻す。融合で不要になったコピーは、
次の読み出しより先に上書きされると証明できるものだけ除去する。元の一時セルのclearも保持する。
call、portal、I/O、非local storage、aggregate copy、制御境界を跨ぐ融合は行わない。
Loop/Branchの各bodyは独立に処理する。加算＋carryや、減算より後に現れる比較は未対応。

### TerminatorとCFG

`Terminator`は次の制御境界を明示する。

- 同一関数内の`Goto`と、条件cellを消費する`Branch`
- frameを切り替える`Call`と`Return`
- 動的offsetのaccessをportalへ渡す`AggregateLoad`と`AggregateStore`
- 互換API用の単一cell `ArrayLoad`と`ArrayStore`
- 即時停止する`Abort`と、mainの正常終了である`Halt`

callの`return_to`とportal accessの`return_to`もcontinuation IDを実行時データとして使用する。
したがってCFGを変形するときは、通常の`Goto`/`Branch` successorだけでなく、function entry、call return、
portal resumeを含むすべてのID参照を書き換えなければならない。

HIRからのloweringは、sourceの`if`、`while`、短絡論理演算、call、動的aggregate accessでcontinuationを
分割し、同時にframe slot、aggregate region、continuation IDを割り当てる。この層の責務は、sourceの
評価順序を、ABI backendが直接実装できるframe-relativeな制御フローと破壊的cell操作へ変換することである。

### 現在の最適化pass

source frontendでは、再帰 SCC を除く Call を allocation 前の CIR で clone／splice する。
試験 allocation と B1 layout で frame cost を確認し、global を使う caller の frame 増加を抑える。
main と global initializer から到達しない関数は lowering／inline 後の reachability で除去する。
全 reachable 関数を virtual slot の Continuation に lowering し、`continuation_pipeline` が
CFG cleanup／local reconstruction の後で関数ごとに `frame_fusion` と `frame_allocation` を実行する。
最後は空の Goto の threading だけを行い、再利用後の descriptor と命令列を
`ContinuationProgram` constructorで検証する。これはsource frontend内のpassであり、公開APIで手動構築した
Continuation IRや`--cir-input`のalias-preserving flat frameには自動適用しない。
詳細と計測結果は[FRAME_ALLOCATION.md](FRAME_ALLOCATION.md)と[CIR_INLINE.md](CIR_INLINE.md)に記録する。
`--disable-function-inline` は source の Call を保持し、`--disable-region-emission` は BF backend を B0 に戻す。
両者は独立した比較オプションで、source 構文には inline 指定を追加していない。

開発時は`bfc --run-ir source.bfc`で、ABI backendとBrainfuckへの展開を行わず
`ContinuationProgram`をRust上で直接実行できる。この経路はcellのmod 256演算、frame、call/return、
aggregate outbox、動的portalの意味を保ち、標準出力を固定長bufferからstreamingする。10秒ごと
（`--ir-progress-interval`で変更可能）にcontinuation/frame命令数、call depth、出力byte数、
Linux上のRSS/HWMを標準エラーへ出し、終了時には上位10件のhot continuationも表示する。
巨大なBF artifactを介さずselfhost compilerの意味上の失敗とhot pathを調べるための経路であり、
最終的なBF backendの互換性検証を置き換えるものではない。

最近のdispatcherのhigh/low byte countdown化は、Continuation IRを書き換える処理ではない。
ABI backendがuser continuationとhidden portal continuationからdispatch tableを組み立て、より短い
BF templateを選ぶbackend最適化である。同様に、連続するaggregate cellのclearをrange templateへ
融合する処理もABI backendにある。どちらも物理pointer位置とprotocolを知る必要があるため、配置は妥当である。

## 最適化を置く層と追加IRの判断

最適化は、必要な意味をまだ保持している最も低い層へ置く。

| 最適化 | 置く層 | 理由 |
|---|---|---|
| source定数評価、pure式の簡約、評価順序を要する変形 | HIRまたはHIR lowering | nominal型、式木、副作用の順序が残る |
| 到達不能continuation除去、jump threading、block結合、ID再採番 | Continuation IR | CFG、function所有者、call/portal resumeが見える |
| dispatcher、call/return、portal、global navigation、pointer-aware template融合 | ABI backend | frame layout、protocol、entry/exit pointer条件を知る |
| 隣接`Move`/`Add`、clear loopなど局所的なBF正規化 | BF IR | 上位の意味を必要としない |

### 直近に追加するならIRではなくContinuation CFG pass

次に実装する価値があるのは、新しい表現ではなく、既存の`ContinuationProgram`を入力と出力にする
独立passである。最初の対象は次とする。

1. function entry、通常successor、call return、portal resumeからの到達可能性を使うunreachable除去。
2. 空の`Goto`に対するjump threading。
3. successorが一つで、runtimeから直接addressされないcontinuationの安全な結合。
4. 解析で定数と証明できるterminator `Branch`の簡約。
5. 全参照を更新した後の密なID再採番。

提案するCFG passの初版は全`FunctionDescriptor::entry`をrootにして、function内の到達不能continuationだけを
除去する。現行のunused function除去はHIR側でmainとglobal initializerをrootに行っている。

function entry、callの`return_to`、aggregate portalの`return_to`はaddress-takenとして保守的に扱う。
これらをthreadingまたは結合する場合は、backendが期待するcontextとresume protocolを保つことを
個別に証明する。passの前後で`ContinuationProgram`のvalidationを行い、ID境界、再帰、portalを含む
differential testを置く。

temporaryの削減は現行の`frame_allocation`がContinuation CFG上のlivenessと干渉グラフで実装する。
localとtemporaryの両方を対象にするため、compiler-owned temporaryという追加metadataは必要ない。

### SSAは現時点では追加しない

現状の主要なcostはdispatcher、frame/portal間の移動、BF templateであり、scalarの再計算を減らす
古典的SSA最適化が直接解く問題ではない。またHIRのlocal/globalは可変storageであり、aggregate、
動的projection、call、I/Oを含む。SSA化にはphiまたはblock parameterだけでなく、aliasとeffectの契約、
memory操作、値を再びframe cellへ割り当てる処理が必要になる。SSA temporaryをspillしてframeを広げると、
Brainfuckではpointer移動とframe costが悪化する可能性もある。

したがって、次の条件がprofileと実例で満たされるまではSSA専用IRを追加しない。

- CFG縮約、backend template改善、temporary再利用の後も、pure scalar式の再計算や不要copyが支配的である。
- call、I/O、global、aggregate、dynamic projectionのeffect/alias分類を定義できている。
- constant propagation、CSE、loop-invariant code motionなど、複数のpassで追加IRの維持費を回収できる。

条件を満たした場合の候補は、全面的なmemory SSAではなく、HIRとContinuation IRの間だけに存在する
内部`Value CFG`である。basic blockとblock parameter、typedなscalar virtual value、明示的な
load/store/call/effectを持たせ、pure scalarだけをSSA対象とする。aggregate、global、dynamic portalは
memory operationのまま保守的に扱い、最適化後にvirtual valueの生存期間を見てframe slotを割り当てる。
この候補は現行pipelineの一部ではない。

## 低水準API用Cell IR

セルIRは、物理的なテープ位置を意識しない、構造化されたIRである。
現在の `bf-compiler` crateに実装されている `Instruction` はこの層に相当する。

各変数は論理的な `CellId` で参照する。命令の利用側は、あるセルがテープ上の
何番目に配置されるかや、命令実行前後のデータポインタ位置を意識しない。

### 現在の命令

概念上の定義は次のとおりである。

```rust
enum Instruction {
    Set {
        dst: CellId,
        value: u8,
    },
    AddConst {
        dst: CellId,
        value: u8,
    },
    Transfer {
        src: CellId,
        targets: Vec<TransferTarget>,
    },
    Input {
        dst: CellId,
    },
    Output {
        src: CellId,
    },
    Loop {
        condition: CellId,
        body: Vec<Instruction>,
    },
    Branch {
        condition: CellId,
        then_body: Vec<Instruction>,
        else_body: Vec<Instruction>,
    },
}

struct TransferTarget {
    dst: CellId,
    factor: u8,
}
```

すべての算術は8 bitのwrapping演算、すなわちmod 256で行う。

### `Set`

```text
dst = value
```

`dst`の以前の値を破棄し、定数で上書きする。BFでは通常、clearと定数加算へ
展開する。独立した命令として保持することで、連続する代入や加算を最適化
しやすくする。

### `AddConst`

```text
dst += value
```

`value`は`u8`であり、減算もmod 256の加算として表現できる。例えば
`AddConst(dst, 255)`は`dst -= 1`と等価である。BF生成時には`+`と`-`のうち
短い表現を選択する。

### `Transfer`

```text
for target in targets:
    target.dst += src * target.factor
src = 0
```

`Transfer`は破壊的な転送命令であり、clear、移動、減算、複製、定数倍転送を
一般化したものである。

```text
targets = []
    → Clear(src)

targets = [(a, 1)]
    → a += src; src = 0

targets = [(a, 255)]
    → a -= src; src = 0

targets = [(a, 1), (b, 1)]
    → a += src; b += src; src = 0
```

IRの妥当性条件は次のとおりである。

- `src`を転送先に含めない。
- 同じ転送先を複数回指定しない。
- 係数0の転送先を含めない。
- 実行後の`src`は必ず0になる。

係数付き転送はBFの典型的な`[->+++<]`形式に対応する。`Add`、`Sub`、
`Clone`を別々の命令にするより、BFループの最適化単位を直接表現できる。

### `Input`と`Output`

```text
Input(dst):
    dst = 次の入力byte、またはEOFなら0

Output(src):
    srcを1 byte出力する
    srcは変更しない
```

入出力は文字ではなくbyteを扱う。

### `Loop`

```text
while condition != 0:
    body
```

Brainfuckの`[`と`]`に直接対応する。条件セルは暗黙には変更されない。
ループを終了させる責任は`body`側にあり、`body`は条件セルを含む任意の
論理セルを変更できる。

### `Branch`

```text
if condition != 0:
    condition = 0
    then_body
else:
    else_body
condition = 0
```

`Branch`は条件を消費し、選択したbodyをちょうど1回実行する。条件値が1以外の
非ゼロ値でも`then_body`は1回だけ実行する。bodyが条件セルを書き換えた場合も、
命令終了時には0へ戻す。

条件を保存する必要がある場合は、上位の変換処理が事前に`Transfer`と一時セルを
使って複製する。`else_body`をBFで実現するためのフラグセルはコード生成器が
割り当てる。

## 低水準Cell IRの最適化候補

これは低水準APIに対する未実装の候補であり、source frontendのContinuation IR最適化とは別である。
Cell IRでは次の局所最適化を行える。

```text
Set(x, 10)
AddConst(x, 5)
    ↓
Set(x, 15)
```

```text
Set(x, 10)
Set(x, 20)
    ↓
Set(x, 20)
```

その他の候補は次のとおりである。

- 値が0の`AddConst`の除去
- 後続の上書きによって不要になる命令の除去
- 連続する`Transfer`の統合
- 一時セルの生存期間解析と再利用
- 頻繁に相互参照するセルを近くへ配置するレイアウト最適化
- 定数条件を持つ`Loop`や`Branch`の簡約

## BF IR

BF IRはBrainfuckの構造を保ちつつ、文字列より編集・解析しやすい表現とする。
`bf-compiler` crateの`BfInstruction`および`BfProgram`として実装している。

```rust
enum BfInstruction {
    Move(isize),
    Add(u8),
    Input,
    Output,
    Loop(Vec<BfInstruction>),
}
```

`Move`は相対的なポインタ移動量を、`Add`は一命令分の加算量を表す。
`Add`は現在セルへのmod 256の加算である。例えば`Add(255)`は1減算と等価で、
BF文字列へ変換するときは`-`として出力する。

lowering自体は独立したBF peephole passを実行せず、Cell IR backendまたはABI backendが各命令を
BF IRへ変換する。
したがって公開APIの`lower`と`lower_continuations`は、隣接する`Move`や`Add`、
`Move(0)`や`Add(0)`もそのまま保持する。未加工の結果を検査・計測できるようにするためである。
`Move(0)`と`Add(0)`を文字列化した結果は空文字列になる。

`compile`と`compile_continuations`はlowering後に独立したBF IR最適化パスを実行する。
`optimize_bf`を直接呼び、手動で構築した`BfProgram`へ同じパスを適用することもできる。
現在のパスはループbodyを再帰的に処理し、次の局所変換を行う。

```text
Move(a), Move(b)       → Move(a + b)
Add(a), Add(b)         → Add((a + b) mod 256)
Move(0), Add(0)        → 削除
Add(...), [Add(odd)]   → [-]
[Add(odd)], [-]        → [-]
Add(...), [Add(odd)], Input → Input
```

加算・移動を統合した結果が0なら、その命令も削除する。奇数を加える単一命令loopは8-bit
wrapping cellを必ず0にするため、`[+]`や`[-]`も含めて短い標準形`[-]`へ統一する。
I/Oや一般のloopを越えた並べ替えは行わない。
相殺するポインタ移動の削除は、最適化前の実行がテープ境界を越えないことを前提とする。

低水準Cell IR backendは、各命令をBF IRへ変換する際に次を担当する。

- `CellId`から物理テープ位置への変換
- 必要な位置へのデータポインタ移動
- `Branch`などが要求する一時セルの割り当て
- 命令終了時のデータポインタ位置の追跡
- 30,000セルの範囲内に収まることの検査

source本線のABI backendは別に、frame/static layout、dispatcher、call/return、aggregate portal、
pointer位置の規約を具体的なBF操作へ展開する。

### BF IR最適化の今後の候補

BF固有の分岐、比較、divmod、moving-indexなどのlowering候補とcost modelは
[BF_OPTIMIZATION_NOTES.md](BF_OPTIMIZATION_NOTES.md)にまとめる。

さらに、ループを解析してCell IRまたはFrameInstruction相当の操作へ戻せる場合には、最適なBF表現へ
再構成できる。ただし、compiler backendは上位の意味を知った時点で転送loopなどを選択できるため、
BF IRの当面の責務はBrainfuckの構造を保持した中間表現と最終的な文字列化である。

## Aggregate storageと動的projection

現在のstruct、固定長array、多次元arrayは、localならalignedな`FrameAggregateId`、globalなら
static `GlobalId`のaggregate regionへ配置する。定数offsetのsubobjectはregion内の
`Address::ArrayElement`として直接扱い、動的offsetのsubobjectは[ABI.md](ABI.md)のaggregate portalへ
loweringする。version 0の`Array`という型名とterminatorは、公開Continuation IRの互換APIとして残る。

### 定数projection

fieldと定数indexだけからなるplaceは、flatten済みaggregate内の定数logical cell範囲へ解決する。

```text
node.left.slot       → root + field offset
pages[2][10]         → root + 2 * row_cells + 10
nodes[3].kind        → root + 3 * cells(Node) + field_offset(kind)
```

現行実装は、local aggregateが定数offsetだけで使われる場合もregion全体を`FrameAggregateId`として
確保する。将来scalarizeしてもよいが、source-levelの値渡しとsnapshot semanticsを保ち、動的accessの
可能性がないことを証明する必要がある。物理chunk flagを挟ぐかどうかはABI layoutが決め、
source-levelの連続性やpointer値として観測できない。aggregate copyはlogical leaf順に行い、
protocol cell、chunk head、paddingを含めない。

### 動的projection

動的indexを含むplaceは、選択された途中のsubarrayやstructをruntime referenceとして表さない。
HIR loweringは各indexを左から右に一度だけ評価し、root aggregate内のflat logical offsetを
compiler-ownedな2 cell temporaryとして計算する。

```text
offset = constant_field_offsets
for each Index(index, element_cells):
    offset += index * element_cells
```

各source indexは従来どおり1 cellであり、`0..=255`を表す。2 cellなのは最大65,536 cellのpayload内で
`0..=65,535`のoffsetを保持するcompiler-owned値だけであり、sourceへ`u16`やpointerを追加しない。
通常compileではstatic領域とframe領域に30,000-cell tapeのcapacity checkも適用する。現行の
`*_unbounded` APIが外すのはstatic layout側のcapacity checkであり、function frameは引き続き
`FrameLayout`の30,000-cell上限を満たす必要がある。
範囲外indexはLANGUAGE.mdどおり未定義動作である。

Continuation IRでは配列専用identityをaggregate regionへ一般化する。

```rust
enum ValueType {
    Cell,
    Array(usize), // version 0 compatibility
    Aggregate { cells: usize },
    Void,
}

enum AggregateRegion {
    Frame(FrameAggregateId),
    Global(GlobalId),
    Outbox,
}

struct LogicalOffset {
    low: Address,
    high: Address,
}

enum ValueOperand {
    Cell(Address),
    Aggregate {
        region: AggregateRegion,
        offset: usize,
        cells: usize,
    },
}
```

sourceのnominal type一致はtyped HIRで検査済みなので、Continuation IRのaggregate typeは
storage cell数だけを保持する。enumもこの層では`Cell`へeraseする。定数subobjectはregion、
offset、cellsで表し、動的subobjectは次のterminatorでportalへ渡す。

```rust
AggregateLoad {
    source: AggregateRegion,
    offset: LogicalOffset,
    destination: ValueOperand,
    cells: usize,
    return_to: ContinuationId,
}

AggregateStore {
    destination: AggregateRegion,
    offset: LogicalOffset,
    source: ValueOperand,
    cells: usize,
    return_to: ContinuationId,
}
```

`cells == 1`は現在のscalar array load/storeに相当する。複数cellのload/storeはlogical offsetから
連続するleafを順にcopyする。store前にはRHS全体をtemporaryへsnapshotし、load完了またはstore
開始から終了までuser codeや別のportal operationを実行しない。これによりcopy途中のaliasを
観測できず、source-levelには一回のaggregate read/writeとして見える。

zero-size aggregateのcopyは何もせず、動的access terminatorも生成しない。zero-length arrayへの
indexは定数ならfrontend error、動的ならsource-level undefined behaviorなので、backendが有効な
zero-size portalを提供する必要はない。

文字列リテラルはdecoded byte列を持つaggregate constantとしてHIRへ残し、宣言位置または
argument/return temporaryへ各byteを設定する。末尾NUL、文字列専用region、runtime length cellは
追加しない。

### `abort`

macro展開後も`abort` statementはHIRの終端操作として残し、どのfunctionからでも生成できる
`Terminator::Abort`へloweringする。`Abort`にはsuccessorとreturn valueがなく、以後のstatementは
到達不能である。通常の`Halt`はmainの正常終了専用として区別する。

## セル配置

現実装では`CellId`の番号をそのまま物理テープ位置としている。将来的には
`CellId`と物理位置を分離し、配置表を導入する。

```rust
struct Layout {
    positions: Vec<usize>,
    temporary_start: usize,
}
```

これにより、IRを書き換えずに次を変更できる。

- 使用頻度に基づくセル順序
- 配列用作業セルの挿入
- 一時セルの再利用
- 静的セル領域、配列領域、作業領域、スタック領域の分離

## 不変条件と検証

typed HIRは次を検証する。

- enum、struct、array、field/index projectionがLANGUAGE.mdの型規則を満たす。
- type layoutとprojection offsetのchecked arithmeticがoverflowしない。
- assignment/call/returnのnominal型が一致し、各動的indexの評価順序が保持される。
- macro、method call、`len`、`cell[]`がruntime HIRへ残っていない。

Continuation IRのconstructorは次を検証する。

- global、function、continuation IDが一意で、function entryとsuccessorの所有関係が正しい。
- `FrameSlot`、global、aggregate subrange、logical offsetがdescriptorの範囲と型に一致する。
- `Copy`のsource/destination、`Transfer`のsource/targetと係数、`Branch`のconditionが各命令の契約を満たす。
- call argumentとreturn valueがcallee/callerの型、parameter location、outbox容量に一致する。
- main、`Return`、`Halt`、`Abort`、portal terminatorの配置が制御規約を満たす。

セルIRはコード生成前に全体を検証する。

- 参照する`CellId`がプログラムのセル数以内である。
- `Transfer`のsourceとtargetがaliasしない。
- `Transfer`のtargetが重複しない。
- `Transfer`の係数が0でない。
- 一時セルを含む物理配置が30,000セル以内である。
- 構造化命令のbodyも再帰的に同じ条件を満たす。

不正なIRをコード生成器が暗黙に修正するのではなく、明示的なエラーとして
拒否する。

## selfhost binary CIR

stage-2 selfhost compilerは、arena recordやtyped ASTをserializeせず、lowering後のflat ABI CIRだけを
`BFCIR\0\x01\n` magicで始まるbinary record streamへ出力できる。headerは24-bit little-endianの
static cell数と16-bit main function IDを持つ。その後にfunction、continuation、instruction、
terminator recordが続き、`0xff`で終了する。

- function recordはID、entry continuation、flat frame幅、return種別、parameterのdestination/幅を持つ。
- continuation recordは16-bit dispatch IDとowner functionを明示する。
- scalar slotは8-bit、dispatcher/function IDとlogical offset量は16-bit、global base/lengthは24-bitである。
- call recordはcallee、resume、評価済みargumentのsource/destination/幅を持つ。
- dynamic access recordはoperation、data slot、offset low/high、base、region幅、frame/global種別を持つ。

Rust decoderはrecordを直接typed vectorへ読み、flat frame全体を一つのaligned aggregateへ写す。
配列命令はhidden continuationを挟むportal terminatorへ分割する。globalはdynamic accessされた区間だけを
aggregate segment化し、それ以外で実際に参照されたcellだけをscalar globalにする。このため24-bitの
source logical addressをtarget tape上へそのまま再現する必要はなく、sourceが観測できるaliasだけを保つ。

CLIは`bfc --cir-input FILE --unlimited-tape`（または`--cir-input -`）でこのstreamをRust ABI backendへ
渡す。BF IRのserializerは長いMoveを巨大な`String`へ展開せずstdoutへ直接書く。

## 当面の実装順序

1. 現在のセルIR、BF IR、BF文字列化を基準実装として安定させる。（完了）
2. `main`をroot activationとするContinuation IRとABI frame layoutを導入する。（完了）
3. scalar function call、return、直接・相互再帰をfrontendとbackendへ接続する。（完了）
4. localの定数添字配列を実装する。（完了。第7段階でaligned frame arrayへ統合）
5. global scalarとstatic領域の初期化を接続する。（完了）
6. array portalと動的配列命令を追加する。（完了）
7. 配列の値渡し、aggregate return outboxを接続する。（完了）
8. BF IRの局所最適化を別パスとして追加する。（完了）
9. source型をTypeId/layout tableへ一般化し、enum、struct、再帰的array、projectionを追加する。（完了）
10. aggregate regionと16-bit logical offset portalをContinuation IR/ABIへ追加する。（完了）
11. 文字列、`len`、`const cell`、method sugar、block macro、`abort`をfrontendへ追加する。（完了）
12. BFCでstreaming lexer/parserを記述し、self-hostに不足する最小機能を実測から判断する。
    （着手。`selfhost/stage2/compiler/`でLANGUAGE.mdの初期実装第1〜12段階をBF上から
    BFへコンパイルし、外部harnessで生成物を実行検証できる。packed arenaを4,096 cellから
    16,384 cellへ拡張し、旧上限を超える入力を回帰検証している。固定13-cell nodeをkind別3〜13-cell
    recordへcompact化し、同じarenaでの自己入力到達位置を12,567 byteから15,454 byteへ改善した。
    旧direct parser専用sourceをproduction連結から除外した。さらにscalar literalの4-cell化、binary
    右辺即値、parse時定数畳み込みにより、直前構成の15,431 byteから17,726 byteまで到達する。現在の
    全source需要に対して16-bank arenaとbinary CIR streamingを導入し、selfhost frontendからRust ABI
    backendへ渡す完全自己入力経路を通した。）
13. profileに基づき、BF固有templateとcost modelを段階的に追加する。（着手）
14. `ContinuationProgram → ContinuationProgram`のCFG縮約passを追加し、dispatcher case数と
    continuation IDを削減する。
15. profileが導入条件を満たした場合だけ、scalar `Value CFG`の試作を判断する。
