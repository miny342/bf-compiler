# 中間表現の設計

この文書は、ソース言語から Brainfuck を生成するまでの中間表現（IR）の
責務と、現時点での設計方針を記録する。

## 全体構成

コンパイラは、構文木から直接 Brainfuck の文字列を生成せず、二段階の IR を
経由する。

```text
ソース言語
    ↓ 字句解析・構文解析
構文木（AST）
    ↓ 名前解決・型検査・変数割り当て
セルIR（Cell IR）
    ↓ 配列展開・一時セル割り当て・低水準化
BF IR
    ↓ BF固有最適化・文字列化
Brainfuckソース
```

関数、再帰、frame-relativeなローカル変数を追加する次段階backendでは、ASTとセルIRの
間またはセルIR内部にContinuation IRを導入する。実験中のchunked frame stack、
call/return、pointer位置の規約は[ABI.md](ABI.md)に定義する。現在の実装はまだこの
ABIを使用せず、すべての`CellId`を静的セルとして割り当てる。

二段に分ける目的は、ソース言語の意味と、Brainfuckのデータポインタや
相対移動を分離することである。

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

現時点では最適化パスを持たない。セルIRの各命令を機械的にBF IRへloweringし、
隣接する`Move`や`Add`も統合せず、そのまま保持する。`Move(0)`や`Add(0)`も
BF IR上では有効であるが、文字列化した結果は空文字列になる。

セルIRの各命令をBF IRへ変換する際に、コード生成器は次を担当する。

- `CellId`から物理テープ位置への変換
- 必要な位置へのデータポインタ移動
- `Branch`などが要求する一時セルの割り当て
- 配列操作の具体的なテープ操作への展開
- 命令終了時のデータポインタ位置の追跡
- 30,000セルの範囲内に収まることの検査

### 将来BF IRで行える最適化

以下は将来の候補であり、現在は実装しない。
BF固有の分岐、比較、divmod、moving-indexなどのlowering候補とcost modelは
[BF_OPTIMIZATION_NOTES.md](BF_OPTIMIZATION_NOTES.md)にまとめる。

```text
Move(3), Move(-1) → Move(2)
Add(10), Add(-3)  → Add(7)
Move(0)           → 削除
Add(0)            → 削除
```

さらに、ループを解析してセルIR相当の操作へ戻せる場合には、最適なBF表現へ
再構成できる。ただし、コンパイラ自身が生成したコードではセルIRの段階ですでに
転送ループを認識しているため、BF IRの当面の責務はBrainfuckの構造を保持した
中間表現と最終的な文字列化である。

## 配列

動的添字によるアクセスは通常の`CellId`参照として扱えない。物理表現とcontrol flowは
[ABI.md](ABI.md)の16-cell array portalへloweringする。

### 定数添字

```text
array[3]
```

定数添字は実行時命令にせず、セル配置時に論理セルへ解決する。配列要素間へ
作業セルを挟むレイアウトを採用する可能性があるため、単純な
`array_top + index`とは限らない。

概念的には、名前解決中に次のような場所を扱う。

```rust
enum Place {
    Cell(CellId),
    ArrayElement {
        array: ArrayId,
        index: usize,
    },
}
```

セルIRが確定するまでに、定数添字の`ArrayElement`は`CellId`へ変換する。

### 動的添字

```text
array[index]
```

BFでは実行時に選択したセルを、後続命令から通常の`CellId`として参照することは
できない。そのため、単なる`Array(array_top, index)`ではなく、選択した要素に
対する操作まで命令に含める必要がある。

セルIRには次を追加する。

```rust
ArrayLoad {
    dst: CellId,
    array: ArrayId,
    index: CellId,
}

ArrayStore {
    array: ArrayId,
    index: CellId,
    src: CellId,
}

ArrayTake {
    dst: CellId,
    array: ArrayId,
    index: CellId,
}
```

暫定的に想定する意味は次のとおりである。

```text
ArrayLoad:
    dst = array[index]
    array[index]は不変
    index = 0

ArrayStore:
    array[index] = src
    index = 0
    src = 0

ArrayTake:
    dst += array[index]
    array[index] = 0
    index = 0
```

`ArrayTake`は破壊的な最適化用命令であり、frontendが通常の配列readへ直接使用しない。
添字や格納元をsource上で保存する場合は、上位の変換処理がcompiler temporaryへ複製
してから破壊的なABI helperへ渡す。

global/localとも、16 logical protocol cellsの後ろに配列要素を置く。動的load/storeは
共有array continuationへ入り、call-site固有resume continuationが値を回収してframeへ
戻る。初期backendはindexをchunk/withinへ分けた二段静的dispatchを生成する。

- 配列長の上限は`cell`添字が全要素を表現できる256とする。
- 実行時範囲外添字は検査せず未定義動作とする。
- 定数添字はportalを呼ばず、layoutから直接offsetを解決する。
- load/storeと直後の演算を融合する最適化は後から追加してよい。

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

1. 現在のセルIR、BF IR、BF文字列化を基準実装として安定させる。
2. 論理セルと物理セルを分離する`Layout`を導入する。
3. Continuation IRとABI frame layoutを導入する。
4. 定数添字配列を論理layoutへ接続する。
5. array portalと動的配列命令を追加する。
6. function call、return、aggregate valueをfrontendへ追加する。
7. 必要性を測定してから、BF IRの最適化を別パスとして追加する。
