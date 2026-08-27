# 中間表現の設計

この文書は、ソース言語から Brainfuck を生成するまでの中間表現（IR）の
責務と、現時点での設計方針を記録する。

## 全体構成

コンパイラは、構文木から直接 Brainfuck の文字列を生成しない。source frontendと
低水準APIは、それぞれ用途に応じたIR pipelineを持つ。

```text
source frontend:
  BFC source → AST → macro expansion/desugaring → typed HIR
             → Continuation IR → ABI backend → BF IR → optimize → Brainfuck

low-level API:
  Cell IR → static-cell backend → BF IR → optimize → Brainfuck
```

scalar関数、再帰、frame-relativeなローカル変数には、typed HIRとContinuation IRを
使用する。chunked frame stack、call/return、pointer位置の規約は[ABI.md](ABI.md)に
定義する。現在のsource frontendは、名前解決・型検査済みHIRから関数ごとのframe slotと
Continuationを生成し、ABI backendへ渡す。低水準APIとして、静的`CellId`を使用する従来の
セルIRとbackendも独立して残す。

公開APIでは`lower_source`が`ContinuationProgram`を返し、`compile_source`のbackend errorは
`SourceCompileError::AbiCodegen`として報告する。以前の`Program`を返すsource lowering APIと
`SourceCompileError::Codegen`からは互換性のない変更である。手動で構築した静的Cell IRには、
引き続き`compile(&Program)`または`lower(&Program)`を使用する。

IRを分ける目的は、ソース言語の意味、関数の制御フローとframe配置、Brainfuckの
データポインタや相対移動を分離することである。

現在の実装はLANGUAGE.mdの第12段階までであり、以下のenum、struct、macro、多段projection、
16-bit aggregate offsetを含むversion 1 IRを使用する。

## Source AST、展開、typed HIR

parserは型定義、文字列、field/index postfix、method call、macro定義・呼出しを保持したASTを
作る。runtime IRへ進む前に次のcompile-time処理を順に行う。

1. トップレベルの型、定数、macro、global、function名を収集する。
2. block macroをfreshなlocal identityを持つblock ASTへ展開する。
3. `receiver.function(args)`を`function(receiver, args)`へdesugarする。
4. `cell[]`文字列宣言の長さを確定し、`len`と`const cell`を評価する。
5. 名前解決と型検査を行い、nominal typeと評価順序を持つtyped HIRを作る。

macro、method call、`cell[]`、`len`はこの段階で消費され、Continuation IRまたはABIへ専用の
runtime機構を追加しない。文字列リテラルは型確定後にaggregate定数初期化として残す。
arena、paged handle、多倍長整数も通常のstruct、array、global、functionとしてloweringし、
専用のHIR命令やABI objectを定義しない。

### 型とlayout

source型は概念的にstableな`TypeId`で参照し、layout tableを別に持つ。

```rust
enum TypeKind {
    Cell,
    Enum { variants: Vec<u8> },
    Struct { fields: Vec<Field> },
    Array { element: TypeId, length: usize },
    Void,
}

struct TypeLayout {
    cells: usize,
    fields: Vec<FieldLayout>,
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
        field: FieldId,
        cell_offset: usize,
    },
    Index {
        index: HirExpression,
        length: usize,
        element_cells: usize,
    },
}
```

`nodes[page][slot].kind`は一つの`HirPlace`であり、途中のarrayまたはstructへのruntime referenceを
生成しない。定数indexとfield offsetは可能な限り一つの定数offsetへ畳み込む。動的indexは
projection順にそれぞれ1回評価し、aggregate rootからのlogical flat offsetをbackend用temporaryへ
計算する。

function callなどplaceでないaggregate式へfield/index postfixを適用する場合、base式を一度だけ
評価してcompiler temporaryへmaterializeし、そのtemporaryをrootとする`HirPlace`へ変換する。
`len` operandだけはunevaluatedなのでtemporaryを作らない。

placeの読み出し結果が複数cellならaggregate value、1 cellならscalar valueになる。代入ではRHSを
完全にsnapshotしてから、LHSの動的projectionを左から右に評価する。この規則により、RHSやindexが
同じaggregateを参照する場合もsource semanticsを保つ。

## セルIR

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

## セルIRの最適化候補

セルIRでは、ソース言語の意味を保ったまま次の最適化を行える。

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

lowering自体は最適化を行わず、セルIRの各命令を機械的にBF IRへ変換する。
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

セルIRの各命令をBF IRへ変換する際に、コード生成器は次を担当する。

- `CellId`から物理テープ位置への変換
- 必要な位置へのデータポインタ移動
- `Branch`などが要求する一時セルの割り当て
- 配列操作の具体的なテープ操作への展開
- 命令終了時のデータポインタ位置の追跡
- 30,000セルの範囲内に収まることの検査

### BF IR最適化の今後の候補

BF固有の分岐、比較、divmod、moving-indexなどのlowering候補とcost modelは
[BF_OPTIMIZATION_NOTES.md](BF_OPTIMIZATION_NOTES.md)にまとめる。

さらに、ループを解析してセルIR相当の操作へ戻せる場合には、最適なBF表現へ
再構成できる。ただし、コンパイラ自身が生成したコードではセルIRの段階ですでに
転送ループを認識しているため、BF IRの当面の責務はBrainfuckの構造を保持した
中間表現と最終的な文字列化である。

## Aggregate storageと動的projection

現在実装済みの`cell[N]`は、定数添字だけを使うlocalなら個別の`FrameSlot`へscalarizeし、
動的添字を使うlocal/globalなら[ABI.md](ABI.md)のaligned array portalへloweringする。
セルフホスト拡張では、この仕組みをstruct、任意要素型配列、多次元配列へ一般化する。

### 定数projection

fieldと定数indexだけからなるplaceは、flatten済みaggregate内の定数logical cell範囲へ解決する。

```text
node.left.slot       → root + field offset
pages[2][10]         → root + 2 * row_cells + 10
nodes[3].kind        → root + 3 * cells(Node) + field_offset(kind)
```

local aggregateをすべて定数offsetで使用できる場合、各leafを通常の`FrameSlot`へscalarizeしてよい。
物理chunk flagを挟ぐかどうかはABI layoutが決め、source-levelの連続性やpointer値として観測
できない。aggregate copyはlogical leaf順に行い、protocol cell、chunk head、paddingを含めない。

### 動的projection

動的indexを含むplaceは、選択された途中のsubarrayやstructをruntime referenceとして表さない。
HIR loweringは各indexを左から右に一度だけ評価し、root aggregate内のflat logical offsetを
compiler-ownedな2 cell temporaryとして計算する。

```text
offset = constant_field_offsets
for each Index(index, element_cells):
    offset += index * element_cells
```

各source indexは従来どおり1 cellであり、`0..=255`を表す。2 cellなのは最大30,000-cellの
aggregate内offsetを保持するbackend内部値だけであり、sourceへ`u16`やpointerを追加しない。
範囲外indexはLANGUAGE.mdどおり未定義動作である。

Continuation IRでは配列専用identityをaggregate regionへ一般化する。

```rust
enum ValueType {
    Cell,
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

セルIRはコード生成前に全体を検証する。

- 参照する`CellId`がプログラムのセル数以内である。
- `Transfer`のsourceとtargetがaliasしない。
- `Transfer`のtargetが重複しない。
- `Transfer`の係数が0でない。
- 一時セルを含む物理配置が30,000セル以内である。
- 構造化命令のbodyも再帰的に同じ条件を満たす。

不正なIRをコード生成器が暗黙に修正するのではなく、明示的なエラーとして
拒否する。

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
    （着手。`selfhost/stage2/compiler/`でLANGUAGE.mdの初期実装第1〜3段階をBF上から
    BFへコンパイルし、外部harnessで生成物を実行検証できる。）
13. profileに基づき、BF固有templateとcost modelを段階的に追加する。
