# BFC 言語仕様

この文書は、Brainfuckへコンパイルするための小さなC風言語BFCについて、実装済みの
version 0とセルフホストに向けたversion 1の言語仕様を定義する。末尾の実装段階で、仕様と
現在の実装範囲を区別する。

BFCはBrainfuckを直接記述するためのアセンブリではなく、人が普通に読み書き
でき、最終的にはBFC自身でコンパイラを実装できる言語を目指す。一方で、実装を
小さく保つため、ポインタ、参照、runtime可変長型、汎用heapは持たない。セルフホストに
必要な名前付きデータ表現は`enum`と`struct`、compile-timeのコード再利用は限定された
block macroで提供する。

## 設計方針

- 構文と演算子は可能な範囲でCに合わせる。
- 実行時のscalar値はBrainfuckの1セルに対応する`cell`またはpayloadなし`enum`とする。
- `struct`と固定長配列はcopy semanticsを持つaggregate valueとする。
- 言語上の代入、式、条件判定は非破壊である。
- Cell IRへの変換時に必要な一時セルと破壊的操作をコンパイラが挿入する。
- 動的メモリ確保、ポインタ、参照、address-of、pointer演算は持たない。固定長配列上の
  arena、handle、多倍長整数はlibraryとして記述する。
- ユーザー定義関数、直接再帰、相互再帰を認める。
- token置換preprocessorは持たない。macroは衛生的なblock statement展開に限定する。
- 暗黙の入出力は行わない。
- 実行開始点は、引数を取らず`void`を返す`main`関数とする。

## 最小例

```c
void main() {
    cell ch = input();
    while (ch != 0) {
        output(ch);
        ch = input();
    }
}
```

`main`はプログラムにちょうど1つ定義する。ユーザー定義関数は同じコンパイル単位の
トップレベルへ定義し、`main`または別の関数から呼び出せる。

## ソースファイル

- ソースはUTF-8文字列とする。
- 改行はLFまたはCRLFを受け付ける。
- ASCIIの空白、タブ、改行、復帰を空白として扱う。
- 識別子は`[A-Za-z_][A-Za-z0-9_]*`とする。
- コメント内には日本語を含む任意のUTF-8文字を記述できる。
- コメント以外の構文、識別子、リテラルにはASCII文字だけを使用できる。
- 大文字と小文字を区別する。
- ファイル拡張子は`.bfc`を推奨する。

1つのプログラムを複数のソースファイルから構成できる。複数ファイルは指定順に1つの
コンパイル単位として扱い、型、定数、macro、global変数、関数の定義を共有する。字句
要素はファイル境界を越えないため、文字列、文字リテラル、ブロックコメントを次の
ファイルまで継続することはできない。プログラム全体で`main`はちょうど1つでなければ
ならない。

これはmoduleまたは`include`構文ではなく、コンパイラへ複数の入力ファイルを渡すための
機能である。現在のCLIでは次のようにファイル順を明示する。

```console
bfc prelude.bfc macros.bfc tokenizer.bfc parser.bfc main.bfc
```

コメントは次の2形式を認める。

```c
// 行末までのコメント

/* 複数行の
   コメント */
```

ブロックコメントはネストしない。

## 型

### `cell`

`cell`は符号なし8 bit整数であり、値域は`0..=255`である。

```c
cell value;
cell initialized = 42;
```

- 加減算はmod 256で行う。
- `255 + 1`は`0`になる。
- `0 - 1`は`255`になる。
- 宣言時に初期値を省略した変数は0で初期化する。
- 符号付き整数型は持たない。
- 真偽値専用の型は持たない。

### `enum`

payloadを持たない名前付き列挙型を定義できる。

```c
enum TokenKind {
    Eof,
    Identifier,
    Number = 10,
    Plus
}
```

各variantは1 cellへ格納する`0..=255`のdiscriminantを持つ。先頭variantの省略値は0、
以降の省略値は直前の値に1を加えた値とする。明示値は`cell`の定数式でなければならない。
discriminantは同じ`enum`内で重複してはならず、zero initializationを有効な値にするため
値0のvariantをちょうど1つ含めなければならない。variant数と最大値は256を越えられない。

```c
TokenKind kind = TokenKind::Identifier;

if (kind == TokenKind::Eof) {
    // ...
}
```

`enum`はnominal typeである。代入、引数、return、同じenum型同士の`==`と`!=`を認める。
`cell`との暗黙変換、異なるenum型同士の比較、加減算、大小比較、論理演算、条件としての
使用は認めない。初期仕様ではpayload付きvariant、`match`、明示castを持たない。

### `struct`

固定レイアウトの値型を定義できる。

```c
struct NodePtr {
    cell page;
    cell slot;
}

enum NodeKind {
    Empty,
    Literal,
    Binary
}

struct Node {
    NodeKind kind;
    NodePtr left;
    NodePtr right;
    cell value;
}
```

- `struct`は1個以上のfieldを持つ。
- field型には`cell`、`enum`、`struct`、固定長配列を使用できる。
- 同じstruct内でfield名を重複できない。
- 自身を直接または配列・別structを経由して再帰的に含む定義はコンパイルエラーとする。
- 宣言時のinitializerを省略すると、すべてのleaf cellを0で初期化する。
- 同じnominal struct型同士の代入、値渡し、returnを認める。
- struct全体の比較、算術、論理演算、destructuring、struct literalは持たない。

fieldは`.`で参照する。fieldを含むplaceは代入先にもできる。

```c
Node node;
node.kind = NodeKind::Literal;
node.left.page = 0;
```

### 配列

任意の値型を要素とする固定長配列を宣言できる。

```c
cell[256] buffer;
Node[255] nodes;
cell[8][255] pages;
```

- 各次元の長さは0から256までのコンパイル時定数とする。
- `T[N][M]`は、外側が長さ`N`、内側が長さ`M`の配列であり、`value[i][j]`と参照する。
- 全要素を要素型のzero valueで初期化する。
- 要素型と全次元の長さが同じ配列同士で全体代入できる。
- 配列を関数へ値渡しし、戻り値として値返しできる。
- 配列全体の比較はできない。
- 配列initializerは同じ配列型の式でなければならない。波括弧による要素列初期化子は持たない。
- 定数添字の範囲外アクセスはコンパイルエラーとする。
- 実行時添字の範囲外アクセスは未定義動作とする。
- 長さ0の配列はstorageを占有せず、copyも何もしない。どの添字によるアクセスも範囲外になる。

定数添字とは、実行時の値を読まずに`cell`値を決定できる式である。
整数・文字リテラルと、それらに対する単項、加減算、比較、論理演算は定数式に
なりうる。加減算は通常の`cell`式と同じくmod 256で評価する。`&&`と`||`で
短絡するoperandは評価されないため、そのoperandが実行時の値を含んでいても式全体の
値が決まる場合は定数添字とみなす。

ただし、その実装段階で未対応のoperationは、短絡されるoperand内にあってもコンパイルエラーとして
診断する。短絡によって未実装機能そのものが
受理されるわけではない。

```c
buffer[1 + 2]       // 3
buffer[255 + 1]     // 0
buffer[1 || input()] // 1; input()は評価しない
```

変数の読み出し、`input()`、function callなどが値の決定に必要な添字は動的添字で
ある。

各次元の添字は`cell`である。複数の動的添字は左から右にそれぞれ1回評価する。

```c
buffer[index] = value;
value = buffer[index];
node = nodes[index];
kind = pages[page][slot];
```

配列長を256にすれば、任意の`cell`値を有効な添字として利用できる。より小さい
配列へ動的にアクセスする場合、プログラム側で範囲内であることを保証する。

配列の要素型がaggregateの場合、要素全体を値として読み書きできる。`nodes[index].kind`
のように最終fieldまでprojectionした場合、途中の`Node`を観測可能な一時値へcopyする必要はない。

配列代入と引数・戻り値は意味上copyである。compilerは結果が同じならmove、caller
outboxへの直接構築、projection先への直接copy、その他のcopy elisionを行ってよい。
各型が占有するlogical cell数は再帰的に決まり、実体化されるglobal、local、parameter、
return outboxを含む物理配置が30,000-cell tapeへ収まらなければコンパイルエラーとする。
したがって`cell[255][255]`という型構文自体は正しいが、その値を現行targetへ配置することは
できない。

### 型のzero valueとcell数

型のzero valueとlogical cell数を次で定義する。

```text
cells(cell)       = 1
cells(enum E)     = 1
cells(struct S)   = sum(cells(field))
cells(T[N])       = N * cells(T)

zero(cell)        = 0
zero(enum E)      = discriminant 0のvariant
zero(struct S)    = 各fieldのzero value
zero(T[N])        = N個のzero(T)
```

cell数の計算がcompiler内部整数でoverflowする型、または実体化するとtarget容量を越える型は
コンパイルエラーとする。これはruntime多倍長整数を言語へ追加することを意味しない。

## リテラル

### 整数リテラル

10進整数と16進整数を認める。

```c
0
42
255
0x00
0x2a
0xFF
```

値は`0..=255`でなければコンパイルエラーとする。リテラル自体を暗黙にmod 256へ
丸めない。配列長contextの整数リテラル`256`だけは、runtime `cell`値を作らないため例外として
認める。

### 文字リテラル

文字リテラルは1 byteの`cell`値である。

```c
'A'
'0'
'\n'
'\r'
'\t'
'\0'
'\\'
'\''
'\x1b'
```

通常形式には表示可能なASCII byteを1つ記述する。`\xNN`は2桁の16進数を必須と
する。Unicode文字および複数byte文字は認めない。

### 文字列リテラル

文字列リテラルはnull終端されない固定長`cell`配列である。

```c
cell[] message = "message";       // cell[7]と推論
cell[3] prefix = "abc";
cell[2] newline = "\r\n";
cell[0] empty = "";
```

文字列の長さ`N`はescapeをbyteへ展開した後のbyte数であり、型は`cell[N]`になる。
末尾へNULを追加せず、文字列中の`\0`と`\x00`は通常の配列要素として保持する。
使用できる通常文字とescapeは文字リテラルと同じで、`\"`を追加で認める。長さが256を
越える文字列リテラルはコンパイルエラーとする。

`cell[]`は、文字列リテラルで直接初期化する変数宣言だけに認める長さ推論構文である。
parameter、return型、struct field、初期化子のない宣言では使用できない。明示された長さと
文字列の長さは完全一致しなければならず、暗黙のNUL追加、padding、切り詰めは行わない。

文字列専用のruntime型、連結、比較、formatは定義しない。通常の配列、`len`、macro、関数で
必要な操作を記述する。

### `len`

`len(array)`は配列型の最外次元の長さを返すcompile-time演算である。operandは型検査だけを
行い、function callや動的添字を含んでいてもruntimeには評価しない。

```c
cell[] message = "message";
cell length = len(message);        // 7
cell columns = len(pages[0]);      // 内側の長さ
```

結果をruntimeの`cell`式として使用する場合、長さが`0..=255`に収まらなければコンパイル
エラーとする。したがって長さ256の配列にも型としての長さは存在するが、`len(array256)`を
`cell`値へ変換できない。`len`は通常のfunctionではなく、値を読み出さない。

### compile-time定数

file scopeに`cell`のcompile-time定数を定義できる。

```c
const cell MaxSlot = 255;
const cell Newline = '\n';
```

initializerはruntime状態を参照しない`cell`定数式でなければならない。定数はstorageを持たず、
代入先にできない。整数リテラルと演算は通常の`cell`と同じ範囲・wrapping規則を使うため、
runtime多倍長整数や別のruntime整数型は導入しない。配列長には値が範囲内なら定数名を使用
できる。配列長grammarのidentifierは`const cell`だけを参照し、通常のvariableやenum variantは
参照できない。長さ256には整数リテラル`256`を直接使用する。

## 変数とスコープ

トップレベルまたはブロック内で変数を宣言できる。

```c
cell counter;
cell[256] buffer;
Node current;

{
    cell nested;
}
```

- file scopeには型、compile-time定数、macro、global変数、functionが属する。
- 型、定数、macro、global、functionのトップレベル名は互いに重複できない。enum variantは
  `EnumName::Variant`、struct fieldは`.`で修飾されるため、それぞれの型内だけで一意とする。
- function parameterとfunction bodyはactivationごとのlocal scopeを形成する。
- ローカル変数の有効範囲は宣言位置から、そのブロックの終端までとする。
- 同じスコープで同名の識別子を複数宣言できない。
- 内側のブロックから外側の変数をshadowできる。
- `cell`、`void`、`enum`、`struct`、`const`、`macro`、`return`、`if`、`while`、`abort`
  などの予約語は識別子に使用できない。
- `input`、`output`、`len`は予約され、変数名またはfunction名として使用できない。

トップレベル定義はソース順によらず名前解決できる。再帰的なstruct定義、compile-time定数の
循環参照、macro展開の循環はコンパイルエラーとする。global initializerだけは従来どおり
宣言順に実行する。

file scopeの変数はstatic領域、function parameterとlocalはactivation frameへ置く。
ブロックを抜けた後、その領域を別のlocalまたはtemporaryへ再利用してよい。

## 式

算術・順序比較・論理式の評価結果は`cell`である。function callは宣言された任意の値型を
返せる。オペランド、function引数、index projectionの評価順序は左から右とし、式の評価は
変数、field、配列要素の値を暗黙には破壊しない。

### 一次式

```c
variable
value.field
array[index]
matrix[row][column]
EnumName::Variant
42
'A'
"bytes"
input()
function(arguments)
receiver.function(arguments)
len(array)
(expression)
```

structと配列は同じ型への代入、function引数、returnでaggregate valueとして使用できる。
scalar演算のoperandにはできない。fieldとindexは左から右に連鎖できる。定数indexは
compile-timeにfield/element offsetへ畳み込み、動的indexは各出現につき1回だけ評価する。

### method call糖衣

`.`の後ろにfunction callを置くと、receiverを第1引数にした通常のglobal function callへ
構文的に変換する。

```c
value.advance(amount)
// advance(value, amount)と同じ

value = value.advance(amount);
// value = advance(value, amount); と同じ
```

method宣言、`impl`、overload、dynamic dispatch、function valueは導入しない。`.`の後ろの名前は
通常のglobal function名として解決する。receiverは第1引数として1回評価し、残りの引数が
続く。戻り値をreceiverへ暗黙に書き戻さないため、更新する場合は上例のように明示的に代入する。
method callを文として使用できるのは、desugar後のfunction return型が`void`の場合だけである。

### 単項演算子

```c
+value
-value
!value
```

- 単項`+`は値を変更しない。
- 単項`-`は`0 - value`をmod 256で計算する。
- `!`は値が0なら1、それ以外なら0を返す。
- 3演算子のoperandはいずれも`cell`でなければならない。

### 算術演算子

```c
left + right
left - right
```

初期仕様では乗算、除算、剰余、ビット演算、シフト演算を持たない。必要になった
演算は変数とループを使ってプログラム中に記述できる。`+`と`-`の両operandは`cell`で
なければならない。

### 比較演算子

```c
left == right
left != right
left < right
left <= right
left > right
left >= right
```

比較結果は偽なら0、真なら1である。`<`、`<=`、`>`、`>=`は符号なし`cell`値にだけ
使用する。`==`と`!=`は`cell`同士、または同じnominal enum型同士に使用できる。

### 論理演算子

```c
left && right
left || right
```

- `&&`と`||`は短絡評価する。
- 結果は必ず0または1である。
- `left && right`は`left`が0なら`right`を評価しない。
- `left || right`は`left`が非ゼロなら`right`を評価しない。

### 演算子の優先順位

高い順に次のとおりとする。

| 優先順位 | 演算子 | 結合方向 |
| --- | --- | --- |
| 1 | `()`、`[]`、`.`、function/method call、`input()`、`len()` | 左から右 |
| 2 | 単項`+`、単項`-`、`!` | 右から左 |
| 3 | 二項`+`、二項`-` | 左から右 |
| 4 | `<`、`<=`、`>`、`>=` | 左から右 |
| 5 | `==`、`!=` | 左から右 |
| 6 | `&&` | 左から右 |
| 7 | `||` | 左から右 |

代入と`output`は式ではなく文である。このため、次のような記述は
できない。

```c
// 不正
a = b = 1;
while ((ch = input()) != 0) {
}
```

## 文

### 空文と組み込み操作文

```c
;
output(expression);
abort();
void_function(arguments);
statement_macro!(arguments);
```

値を計算して捨てるだけの式文は認めない。`output`は専用文として扱い、function callを
文として使えるのはreturn型が`void`の場合だけとする。`abort`は現在のfunctionだけでなく
program全体を直ちに終了する。終了コードを持たず、local、frame、globalをclearする義務もない。

### 変数宣言

```c
cell value;
cell value = expression;
cell[256] buffer;
Node node;
Node[255] nodes;
cell[] message = "message";
```

任意の値型に同じ型のinitializerを指定できる。省略時は型のzero valueで初期化する。
`cell[]`による推論だけは文字列リテラルinitializerを必須とする。

### 代入

```c
value = expression;
value += expression;
value -= expression;

buffer[index] = expression;
buffer[index] += expression;
buffer[index] -= expression;

node.kind = NodeKind::Identifier;
node_pages[page][slot].value = expression;
```

- `=`の左辺は変数、field、配列要素を連鎖したplaceでなければならず、右辺と同じ型を持つ。
- `+=`と`-=`の左辺・右辺は`cell`でなければならない。
- 右辺を評価してから左辺の場所を決定する。
- 左辺の動的添字は、右辺の評価後に外側から内側へそれぞれ1回評価する。
- 右辺や添字に現れた変数、field、配列要素の値は保存される。

`++`、`--`および複合代入式は初期仕様では持たない。

### ブロック

```c
{
    statement
    statement
}
```

ブロックは新しいローカルスコープを作る。

### `if`

```c
if (condition) {
    statement
} else {
    statement
}
```

- conditionは`cell`でなければならず、0なら偽、それ以外なら真とする。
- 条件式の結果そのものは分岐処理により消費してよい。
- 条件式が参照した変数や配列要素は変更しない。
- `else`は省略できる。
- 文法上はブロック以外の1文も許可する。
- `else`は最も近い未対応の`if`に結び付く。

### `while`

```c
while (condition) {
    statement
}
```

conditionは`cell`でなければならない。各反復の開始前に条件式を再評価する。条件式が参照した
変数は、式自身に`input()`または副作用を持つfunction callが含まれる場合を除いて変更しない。

初期仕様では`for`、`do`、`break`、`continue`、`switch`、`goto`を持たない。

## 関数

関数はfileのトップレベルに定義する。nested function、overload、可変長引数は持たない。

```c
cell increment(cell value) {
    return value + 1;
}

cell[100] transform(cell[100] source) {
    // 配列は値渡し
    return source;
}

Node update(Node node) {
    node.value += 1;
    return node;
}

void emit(cell value) {
    output(value);
    return;
}
```

- return型は任意の完全な値型または`void`とする。`cell[]`は使用できない。
- parameter型は任意の完全な値型とし、常に値渡しとする。`cell[]`は使用できない。
- `cell`、enum、struct、配列を値としてreturnできる。
- `void`以外の全実行経路は対応する型の値をreturnしなければならない。
- `abort();`で終了する経路はcallerへ戻らないため、return値を要求しない。
- `void`関数末尾には暗黙の`return;`がある。
- 直接再帰と相互再帰を認める。
- function名はfile scopeで解決し、定義より前から呼び出せる。
- functionからglobal変数を参照できるが、callerのlocalを直接参照できない。
- `&`、pointer、reference、参照渡しは持たない。

### `main`

プログラムの実行開始点は、次のsignatureを持つ`main`関数とする。

```c
void main() {
    // program body
}
```

- `main`は各programにちょうど1つ定義しなければならない。
- `main`のreturn型は`void`、parameter数は0でなければならない。
- `main`を明示的にcallすることはできない。
- `main`の末尾へ到達するか、`return;`を実行するとprogramを正常終了する。
- 終了コードの概念は持たない。

aggregateを書き換えてcallerへ返す場合は、変更後の値を明示的にreturnして代入する。

```c
buffer = transform(buffer);
node = node.update();
```

実装ABI、再帰frame、aggregate return outboxは[ABI.md](ABI.md)に定義する。

## Block macro

compile-timeのstatement展開だけを行うmacroをfile scopeへ定義できる。

```c
macro print(array) {
    cell i = 0;
    while (i < len(array)) {
        output(array[i]);
        i += 1;
    }
}

macro assert(condition) {
    if (!condition) {
        abort();
    }
}

void main() {
    cell[] message = "message";
    print!(message);
    assert!(len(message) == 7);
}
```

macroはparse後、名前解決と型検査の前にASTのblockへ展開する。

- 定義bodyと展開結果は必ず1個のblock statementとする。
- 呼出構文は`name!(arguments);`とし、通常のfunction callと区別する。
- parameterとargumentの個数は一致しなければならない。
- parameterはbody内のexpressionまたはplace位置だけで使用でき、型名、field名、function名、
  宣言名などのtoken生成には使用できない。
- argumentはcall siteの式構文として展開する。macro parameterをplaceとして使う位置へ
  assignできない式を渡した場合は、展開後の通常の型検査でエラーにする。
- macro parameterは値渡しparameterではない。body中に複数回現れれば、argumentもその位置で
  複数回評価されうる。1回だけ評価したいscalarはmacro bodyでlocalへ明示的に保存する。
- macro body内で宣言したlocalとその参照は展開ごとにfreshなidentityへ束縛する。argument内の
  identifierはcall siteでの束縛を保持し、同じ綴りのmacro localにcaptureされない。
- parameter以外の名前はmacro definitionのfile scopeで解決する。
- macro body中の`return`は展開先functionからreturnし、`abort`はprogram全体を終了する。
- macroから別のmacroを呼び出してよいが、直接・間接に循環する展開はコンパイルエラーとする。

expression macro、型macro、top-level item生成、可変長parameter、token結合、文字列化、条件付き
コンパイル、`include`は定義しない。compile-time定数には`const cell`、値を返す再利用可能な
処理にはfunctionを使う。

## VMの記述

BFC自身の関数とは別に、動的配列を使って対象VMのデータstack、call、returnなどを
BFCプログラム内に実装できる。

VMの基本的なdispatch loopは次のように記述できる。

```c
cell[256] code;
cell[256] data_stack;
cell pc;
cell sp;
cell opcode;

void main() {
    cell running = 1;
    while (running) {
        opcode = code[pc];
        pc += 1;

        if (opcode == 0) {
            running = 0;
        } else if (opcode == 1) {
            data_stack[sp] = code[pc];
            sp += 1;
            pc += 1;
        } else if (opcode == 2) {
            sp -= 1;
            opcode = data_stack[sp];
        }
    }
}
```

`cell`だけでは一つの動的添字が0から255に制限される。より大きい論理indexやcounterは、
言語組み込み整数を追加せずstructとfunctionで表現できる。

```c
struct U16 {
    cell low;
    cell high;
}

struct NodePtr {
    cell page;
    cell slot;
}
```

固定長のpaged storage、arena、free list、overflow方針はBFC library側で定義する。
多次元配列を使う場合も、各次元の添字は独立した`cell`である。言語処理系はarena allocation、
汎用heap、`U16`演算を組み込み操作として認識しない。

## 組み込み操作

### `input`

```text
input() -> cell
```

- 実行時入力から1 byte読む。
- EOFなら0を返す。
- 引数は取らない。
- 入力中のNUL byteとEOFは区別できない。

### `output`

```text
output(cell value)
```

- `value`を1 byte出力する。
- 改行や文字コードの変換は行わない。
- 引数は左から右の通常規則に従って1回評価する。

### `abort`

```text
abort() -> never returns
```

`abort();`はどのfunction activationからでもprogram全体を直ちに終了する。出力を追加せず、
終了コードを返さず、stack frameやglobalをclearしない。容量不足などを診断して終了したい
libraryは、必要なbyteを先に`output`してから`abort`する。

`input`、`output`、statement形式の`abort`以外にruntime組み込み操作は定義しない。`len`は
compile-time演算である。stack、clear、copy、文字列出力、format、arena、多倍長算術などは
通常のfunction、macro、代入、aggregate、ループで表現する。

## プログラムの実行

トップレベルは型、定数、macro、global変数、関数の定義だけを含む。トップレベルの実行文は認めない。
globalを初期化した後、`main`の新しいactivationを作り、その本体から実行を開始する。

```c
void main() {
    cell value = input();
    if (value != 0) {
        output(value);
    }
}
```

- global変数は宣言順に初期化し、初期値を省略した場合は0とする。
- 関数定義は登録されるだけで、`main`または別の関数からcallされるまで実行しない。
- `main`以外の関数を暗黙に実行することはない。
- `main`の終了をもってprogramを正常終了する。
- `abort();`はglobal初期化中または任意のfunction実行中からprogramを直ちに停止する。

## 文法概要

以下は字句の詳細を省略したEBNFである。

```ebnf
program          = { top-level-item } ;

top-level-item   = enum-definition
                 | struct-definition
                 | const-definition
                 | macro-definition
                 | function-definition
                 | declaration ;

enum-definition  = "enum" identifier "{"
                   enum-variant { "," enum-variant } [ "," ] "}" ;
enum-variant     = identifier [ "=" constant-expression ] ;

struct-definition
                 = "struct" identifier "{" { field-definition } "}" ;
field-definition = complete-type identifier ";" ;

const-definition = "const" "cell" identifier "=" constant-expression ";" ;

macro-definition = "macro" identifier "(" [ identifiers ] ")" block ;
identifiers      = identifier { "," identifier } ;

block-item       = declaration | statement ;

block            = "{" { block-item } "}" ;

statement        = ";"
                 | block
                 | assignment
                 | output-statement
                 | abort-statement
                 | call-statement
                 | macro-invocation
                 | return-statement
                 | if-statement
                 | while-statement ;

base-type        = "cell" | identifier ;
complete-type    = base-type { "[" array-length "]" } ;
inferred-string-type
                 = "cell" "[" "]" ;
return-type      = complete-type | "void" ;
array-length     = integer | identifier ;

declaration      = complete-type identifier [ "=" expression ] ";"
                 | inferred-string-type identifier "=" string ";" ;

function-definition
                 = return-type identifier "(" [ parameters ] ")" block ;
parameters       = parameter { "," parameter } ;
parameter        = complete-type identifier ;

assignment       = place ( "=" | "+=" | "-=" ) expression ";" ;

output-statement = "output" "(" expression ")" ";" ;
abort-statement  = "abort" "(" ")" ";" ;
call-statement   = postfix ";" ;
macro-invocation = identifier "!" "(" [ arguments ] ")" ";" ;
return-statement = "return" [ expression ] ";" ;

place            = identifier { "[" expression "]" | "." identifier } ;

if-statement     = "if" "(" expression ")" statement
                   [ "else" statement ] ;

while-statement  = "while" "(" expression ")" statement ;

expression       = logical-or ;
logical-or       = logical-and { "||" logical-and } ;
logical-and      = equality { "&&" equality } ;
equality         = comparison { ( "==" | "!=" ) comparison } ;
comparison       = additive { ( "<" | "<=" | ">" | ">=" ) additive } ;
additive         = unary { ( "+" | "-" ) unary } ;
unary            = ( "+" | "-" | "!" ) unary | postfix ;

postfix          = primary {
                     "[" expression "]"
                   | "." identifier [ "(" [ arguments ] ")" ]
                 } ;

primary          = integer
                 | character
                 | string
                 | identifier
                 | function-call
                 | enum-variant-reference
                 | "input" "(" ")"
                 | "len" "(" expression ")"
                 | "(" expression ")" ;

function-call    = identifier "(" [ arguments ] ")" ;
enum-variant-reference
                 = identifier "::" identifier ;
arguments        = expression { "," expression } ;

constant-expression
                 = expression ;
```

構文規則に加えて、programは`void main()`をちょうど1つ含まなければならない。
`call-statement`の`postfix`は、desugar後に`void` function callでなければならない。
`constant-expression`にはruntime状態を読むvariable、`input`、function/method callを含められない。
ただし`len`のoperand内は型だけを参照し、runtime評価されないためこの制限の対象外とする。

## コンパイル時エラー

少なくとも次をコンパイルエラーとする。

- 字句または構文が不正である。
- 未定義の識別子を参照する。
- 同じスコープまたは同じトップレベル名前空間で識別子を再定義する。
- enumに値0のvariantがない、discriminantが重複する、または`0..=255`を越える。
- structがfieldを持たない、field名が重複する、または値として再帰的に自身を含む。
- aggregateまたはenumを、その型で許可されない算術、比較、論理、条件、`output`に使用する。
- fieldが存在しない、arrayでない値をindexする、または定数添字が範囲外である。
- 式中の整数リテラルが`0..=255`の範囲外である。
- 配列の各次元の長さが`0..=256`の範囲外である。ただし整数リテラル256はarray length
  contextだけで認める。
- `cell[]`を文字列リテラルによる変数宣言以外に使用する。
- 文字列リテラルが256 byteを越える、または明示された`cell[N]`と長さが一致しない。
- `len`をarray以外へ適用する、または結果256をruntime `cell`値として使用する。
- compile-time定数がruntime状態を参照する、循環する、または`cell`値にならない。
- `input`、`output`、`abort`を仕様と異なる形で使用する。
- function callの引数型・個数、代入先、return型が宣言と一致しない。
- method callをdesugarしたfunction callの引数型・個数が一致しない。
- `void`関数の値を使用する。
- `void`以外の関数に値を返さない実行経路がある。
- macroのarityが一致しない、展開後の構文・型が不正、またはmacro展開が循環する。
- nested functionを定義する。
- `main`が存在しない、複数存在する、または`void main()`以外のsignatureを持つ。
- `main`を明示的にcallする。
- トップレベルへ実行文を記述する。
- 型のlogical cell数計算がoverflowする、または静的セル、一時セル、aggregate accessor用
  作業セル、最低限のstack管理セルがBFテープの30,000セルを
  超える。

## 未定義動作

初期仕様における未定義動作は、いずれかの次元での実行時配列範囲外アクセスと、再帰または
call深度によるBFテープ右端超過とする。
未定義動作を含むプログラムについて、コンパイラは診断する義務を持たず、生成BFの
動作も保証しない。

可能な限り未定義動作を増やさず、その他の演算は決定的な意味を持たせる。

## 初期実装の段階

仕様全体を一度に実装する必要はない。次の順序で段階的に実装する。

1. 字句解析、構文解析、`void main()`、ブロック、`cell`
2. `input`、`output`、代入、`+`、`-`
3. `if`、`while`、`expression != 0`、`!`
4. その他の比較、`&&`、`||`
5. Continuation IR、scalar関数、call、return、再帰
6. local配列と定数式添字による要素の読み書き
7. static global領域、array portal、動的添字の配列
8. 配列の値渡し、全体代入、aggregate return
9. enum、struct、再帰的な固定長aggregate型、field/index projection
10. 文字列配列、`cell[]`長さ推論、`len`、`const cell`
11. 16-bit logical offsetを使うaggregate portalと動的な多段projection
12. method call糖衣、衛生的block macro、`abort`
13. BFCで記述するstreaming lexer、parser、VMまたはセルフホスト用コンパイラ

未実装の構文を受理して誤ったBFを生成するのではなく、実装済みになるまで明示的な
コンパイルエラーとして拒否する。

### 現在の実装状況

第12段階まで実装している。parameterなしの`void main()`、scalar/aggregate関数、値渡し、
return、forward call、直接・相互再帰を使用できる。enum、struct、任意要素型・多次元の固定長
配列、field/index projection、文字列配列、`len`、`const cell`、method call糖衣、衛生的block
macro、`abort`を使用できる。引数と添字の評価順、短絡評価、aggregateのsnapshot/copy semanticsを
typed HIR、Continuation IR、activation固有outboxで保持する。

globalは宣言順に初期化し、local/global aggregateの動的projectionには16-bit logical offsetの
aggregate portalを使用する。[ABI.md](ABI.md)のbackendは`D = 8`と`D = 16`の両chunk geometryを
サポートする。第13段階には着手しており、`selfhost/stage2/compiler/`のbootstrap compilerが
初期実装第1〜7段階のsubsetをBF上でコンパイルできる。全version 1を入力として自身を再生成する
完全なself-host compilerは未実装である。

### セルフホスト用targetのテープ容量

通常の互換targetは30,000 cellのテープを使用する。一方、第13段階のcompiler開発、bootstrap、
セルフコンパイル検証では、30,000 cellで不足する場合にbackendとBF interpreterの上限を外し、
動的に拡張するテープまたは実質無制限のテープを使用してよい。この開発用targetでは、固定容量へ
収めるためだけにversion 1の言語仕様を削ったり、セルフホストcompilerへ過度なmemory tuningを
要求したりしない。有限テープとの互換性とセルフホストの成立確認は、別のtarget条件として扱う。

現行の`bfc --unlimited-tape`はstatic layoutと、interpreter実行時の右端上限を外す。各function frameの
`FrameLayout`にはまだ30,000-cell上限を適用するため、完全に無制限なcompile targetではない。
現self-host workloadはこの制約内に収まる。frame自体が上限を超える構成が必要になった時点で、
frame layoutのcapacity policyもtarget設定へ分離する。
