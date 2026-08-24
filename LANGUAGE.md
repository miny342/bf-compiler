# BFC 言語仕様

この文書は、Brainfuckへコンパイルするための小さなC風言語、BFCの初期仕様を
定義する。

BFCはBrainfuckを直接記述するためのアセンブリではなく、人が普通に読み書き
でき、最終的にはBFC自身でコンパイラを実装できる言語を目指す。一方で、実装を
小さく保つため、Cのプリプロセッサ、ポインタ、参照、構造体、可変長整数などは
初期仕様に含めない。

## 設計方針

- 構文と演算子は可能な範囲でCに合わせる。
- 実行時のscalar値はBrainfuckの1セルに対応する`cell`とする。
- 固定長`cell`配列はcopy semanticsを持つaggregate valueとする。
- 言語上の代入、式、条件判定は非破壊である。
- Cell IRへの変換時に必要な一時セルと破壊的操作をコンパイラが挿入する。
- 動的メモリ確保、ポインタ、参照、address-of、pointer演算は持たない。
- ユーザー定義関数、直接再帰、相互再帰を認める。
- マクロおよびプリプロセッサは持たない。
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

`main`はプログラムにちょうど1つ定義する。ユーザー定義関数は同じfileのトップレベルへ
定義し、`main`または別の関数から呼び出せる。

## ソースファイル

- ソースはUTF-8文字列とする。
- 改行はLFまたはCRLFを受け付ける。
- ASCIIの空白、タブ、改行、復帰を空白として扱う。
- 識別子は`[A-Za-z_][A-Za-z0-9_]*`とする。
- コメント内には日本語を含む任意のUTF-8文字を記述できる。
- コメント以外の構文、識別子、リテラルにはASCII文字だけを使用できる。
- 大文字と小文字を区別する。
- ファイル拡張子は`.bfc`を推奨する。

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

### 配列

固定長の`cell`配列を宣言できる。

```c
cell[256] buffer;
cell[16] small;
```

- 長さは1から256までのコンパイル時定数とする。
- 全要素を0で初期化する。
- 同じ長さの配列同士で全体代入できる。
- 配列を関数へ値渡しし、戻り値として値返しできる。
- 配列全体の比較はできない。
- 配列初期化子は初期仕様では持たない。
- 定数添字の範囲外アクセスはコンパイルエラーとする。
- 実行時添字の範囲外アクセスは未定義動作とする。

定数添字とは、実行時の値を読まずに`cell`値を決定できる式である。
整数・文字リテラルと、それらに対する単項、加減算、比較、論理演算は定数式に
なりうる。加減算は通常の`cell`式と同じくmod 256で評価する。`&&`と`||`で
短絡するoperandは評価されないため、そのoperandが実行時の値を含んでいても式全体の
値が決まる場合は定数添字とみなす。

ただし、動的配列アクセスなど現在の実装段階で未対応のoperationは、短絡される
operand内にあってもコンパイルエラーとして診断する。短絡によって未実装機能そのものが
受理されるわけではない。

```c
buffer[1 + 2]       // 3
buffer[255 + 1]     // 0
buffer[1 || input()] // 1; input()は評価しない
```

変数の読み出し、`input()`、function callなどが値の決定に必要な添字は動的添字で
ある。

添字も`cell`である。

```c
buffer[index] = value;
value = buffer[index];
```

配列長を256にすれば、任意の`cell`値を有効な添字として利用できる。より小さい
配列へ動的にアクセスする場合、プログラム側で範囲内であることを保証する。

配列代入と引数・戻り値は意味上copyである。compilerは結果が同じならmove、caller
outboxへの直接構築、その他のcopy elisionを行ってよい。

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
丸めない。

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

文字列リテラルは初期仕様では持たない。

## 変数とスコープ

トップレベルまたはブロック内で変数を宣言できる。

```c
cell counter;
cell[256] buffer;

{
    cell nested;
}
```

- file scopeにはglobal変数とfunction名が属する。
- function parameterとfunction bodyはactivationごとのlocal scopeを形成する。
- ローカル変数の有効範囲は宣言位置から、そのブロックの終端までとする。
- 同じスコープで同名の識別子を複数宣言できない。
- 内側のブロックから外側の変数をshadowできる。
- `cell`、`void`、`return`、`if`、`while`などの予約語は識別子に使用できない。
- `input`、`output`は予約され、変数名またはfunction名として使用できない。

file scopeの変数はstatic領域、function parameterとlocalはactivation frameへ置く。
ブロックを抜けた後、その領域を別のlocalまたはtemporaryへ再利用してよい。

## 式

算術・比較・論理式の評価結果は`cell`である。function callは宣言された`cell`または
固定長配列型を返せる。オペランドと関数引数の評価順序は左から右とし、式の評価は
変数や配列要素の値を暗黙には破壊しない。

### 一次式

```c
variable
array[index]
42
'A'
input()
function(arguments)
(expression)
```

配列名は同じ配列型への代入、関数引数、returnでaggregate valueとして使用できる。
scalar演算のoperandにはできない。

### 単項演算子

```c
+value
-value
!value
```

- 単項`+`は値を変更しない。
- 単項`-`は`0 - value`をmod 256で計算する。
- `!`は値が0なら1、それ以外なら0を返す。

### 算術演算子

```c
left + right
left - right
```

初期仕様では乗算、除算、剰余、ビット演算、シフト演算を持たない。必要になった
演算は変数とループを使ってプログラム中に記述できる。

### 比較演算子

```c
left == right
left != right
left < right
left <= right
left > right
left >= right
```

比較結果は偽なら0、真なら1である。比較は符号なし`cell`値として行う。

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
| 1 | `()`、`[]`、function call、`input()` | 左から右 |
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
void_function(arguments);
```

値を計算して捨てるだけの式文は認めない。`output`は専用文として扱い、function callを
文として使えるのはreturn型が`void`の場合だけとする。

### 変数宣言

```c
cell value;
cell value = expression;
cell[256] buffer;
```

配列宣言に初期化式は指定できない。

### 代入

```c
value = expression;
value += expression;
value -= expression;

buffer[index] = expression;
buffer[index] += expression;
buffer[index] -= expression;
```

- 左辺は`cell`変数、同じ型の配列変数、または配列要素でなければならない。
- 右辺を評価してから左辺の場所を決定する。
- 配列添字は左辺の更新直前に評価する。
- 右辺や添字に現れた変数の値は保存される。

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

- 条件が0なら偽、それ以外なら真とする。
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

各反復の開始前に条件式を再評価する。条件式が参照した変数は、式自身に`input()`または
副作用を持つfunction callが含まれる場合を除いて変更しない。

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

void emit(cell value) {
    output(value);
    return;
}
```

- return型は`cell`、`cell[N]`、`void`のいずれかとする。
- parameter型は`cell`または`cell[N]`とし、常に値渡しとする。
- `cell`および配列を値としてreturnできる。
- `void`以外の全実行経路は対応する型の値をreturnしなければならない。
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

配列を書き換えてcallerへ返す場合は、変更後の配列を明示的にreturnして代入する。

```c
buffer = transform(buffer);
```

実装ABI、再帰frame、aggregate return outboxは[ABI.md](ABI.md)に定義する。

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

`cell`だけでは一つの動的添字が0から255に制限される。より大きい仮想メモリが
必要な場合は、複数の256要素配列をページとして用意し、上位byteに相当する値で
使用する配列を分岐する。将来複数セル整数を追加する場合も、BFCの配列アクセス
規則とは独立に拡張する。

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

`input`、`output`以外に呼び出し形式の組み込み操作は定義しない。stack、clear、copy、
文字列出力などは通常の関数、代入、配列、ループで表現する。

## プログラムの実行

トップレベルはglobal変数宣言と関数定義だけを含む。トップレベルの実行文は認めない。
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

## 文法概要

以下は字句の詳細を省略したEBNFである。

```ebnf
program          = { top-level-item } ;

top-level-item   = function-definition | declaration ;

block-item       = declaration | statement ;

block            = "{" { block-item } "}" ;

statement        = ";"
                 | block
                 | assignment
                 | output-statement
                 | call-statement
                 | return-statement
                 | if-statement
                 | while-statement ;

value-type       = "cell" [ "[" integer "]" ] ;
return-type      = value-type | "void" ;

declaration      = value-type identifier [ "=" expression ] ";" ;

function-definition
                 = return-type identifier "(" [ parameters ] ")" block ;
parameters       = parameter { "," parameter } ;
parameter        = value-type identifier ;

assignment       = place ( "=" | "+=" | "-=" ) expression ";" ;

output-statement = "output" "(" expression ")" ";" ;
call-statement   = function-call ";" ;
return-statement = "return" [ expression ] ";" ;

place            = identifier [ "[" expression "]" ] ;

if-statement     = "if" "(" expression ")" statement
                   [ "else" statement ] ;

while-statement  = "while" "(" expression ")" statement ;

expression       = logical-or ;
logical-or       = logical-and { "||" logical-and } ;
logical-and      = equality { "&&" equality } ;
equality         = comparison { ( "==" | "!=" ) comparison } ;
comparison       = additive { ( "<" | "<=" | ">" | ">=" ) additive } ;
additive         = unary { ( "+" | "-" ) unary } ;
unary            = ( "+" | "-" | "!" ) unary | primary ;

primary          = integer
                 | character
                 | identifier
                 | identifier "[" expression "]"
                 | function-call
                 | "input" "(" ")"
                 | "(" expression ")" ;

function-call    = identifier "(" [ arguments ] ")" ;
arguments        = expression { "," expression } ;
```

構文規則に加えて、programは`void main()`をちょうど1つ含まなければならない。
配列には初期化式を指定できない。

## コンパイル時エラー

少なくとも次をコンパイルエラーとする。

- 字句または構文が不正である。
- 未定義の識別子を参照する。
- 同じスコープで識別子を再定義する。
- 配列を算術、比較、論理、`output`など`cell`を要求する位置で使用する。
- 定数添字が配列の範囲外である。
- 式中の整数リテラルが`0..=255`の範囲外である。
- 配列の宣言長が`1..=256`の範囲外である。
- `input`、`output`を仕様と異なる形で使用する。
- function callの引数型・個数、代入先、return型が宣言と一致しない。
- `void`関数の値を使用する。
- `void`以外の関数に値を返さない実行経路がある。
- nested functionを定義する。
- `main`が存在しない、複数存在する、または`void main()`以外のsignatureを持つ。
- `main`を明示的にcallする。
- トップレベルへ実行文を記述する。
- 静的セル、一時セル、配列用作業セル、スタック管理セルがBFテープの30,000セルを
  超える。

## 未定義動作

初期仕様における未定義動作は、実行時の値による配列範囲外アクセスと、再帰または
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
9. BFCで記述するVMまたはセルフホスト用コンパイラ

未実装の構文を受理して誤ったBFを生成するのではなく、実装済みになるまで明示的な
コンパイルエラーとして拒否する。

### 現在の実装状況

第6段階まで実装している。parameterなしの`void main()`をentry pointとし、scalarの
`cell`/`void`関数、parameter、call、return、forward call、直接再帰、相互再帰を使用
できる。callの引数は左から右に評価し、callerのlocalはcalleeの実行中も保存する。
すべての比較演算と、callを含む場合にも短絡評価する論理`&&`および`||`を使用できる。

function内で固定長local配列を宣言し、上記の定数式添字で要素を読み書きできる。
各要素はactivation frameの通常のscalar slotへコンパイル時に展開する。

global変数とglobal配列、動的添字、配列全体の代入、配列parameter、配列returnは、
実装済みになるまで明示的なコンパイルエラーとして拒否する。scalar関数には
[ABI.md](ABI.md)のframe stackとContinuation dispatcherを使用する。array portalと
aggregate argument/returnは独立experimentで検証済みだが、compiler本体にはまだ接続して
いない。
