# 第5段階bootstrap compiler

このディレクトリには、最初に実行可能になったセルフホスト用の小さなコンパイラを置く。
まずRust版コンパイラでBFC製コンパイラをBrainfuckへ変換し、そのBrainfuckプログラムで
初期実装第1〜5段階のBFCをBrainfuckへ変換する。

## ファイル構成

コンパイラ本体は役割ごとに分割している。

- `compiler/00_definitions.bfc`: 定数、token型、global状態
- `compiler/01_common.bfc`: エラー終了、文字分類、小さな算術補助
- `compiler/02_lexer.bfc`: streaming字句解析
- `compiler/03_symbols.bfc`: 識別子とblock scopeの管理
- `compiler/04_codegen.bfc`: Brainfuckのcell移動、copy、加減算、制御loopの生成
- `compiler/05_parser.bfc`: 式、宣言、代入、block、`if`、`while`の構文解析
- `compiler/06_arena.bfc`: 16-bit handle、packed arena、identifier intern
- `compiler/07_ast_parser.bfc`: 第5段階surface syntaxのfull AST構築
- `compiler/08_semantic.bfc`: function収集、名前解決、scalar型検査
- `compiler/09_continuation_ir.bfc`: typed ASTからContinuation IRへのlowering
- `compiler/10_abi_codegen.bfc`: uniform frame、dispatcher、call/return ABI
- `compiler/main.bfc`: production標準入出力とentry point

Rust版`bfc`へは、これらを番号順に複数sourceとして直接渡してもよい。BF上で動くBFC製
コンパイラの入力は1本のbyte streamなので、セルフコンパイル時は単純に連結して渡す。
`scripts/concat-stage2-compiler.sh main`がその連結を行い、各ファイル境界へ改行を補う。

## BFCで記述した内部テスト

productionの`main.bfc`を結合せず、代わりに`tests/test.bfc`と各`*_test.bfc`を結合すると、
BFC製コンパイラ自身の内部テストprogramになる。`test.bfc`の`main`には、各ファイルで定義した
test関数の呼出しだけを手動で列挙する。test登録用のmacroや新しい言語機能は追加しない。

```console
scripts/concat-stage2-compiler.sh test > stage2-tests.bfc
bfc --unlimited-tape stage2-tests.bfc > stage2-tests.bf
bf-interpreter --unlimited-tape stage2-tests.bf
```

`main.bfc`は`read_character`と`compiler_output!`を実際の`input()`と`output()`へ接続する。
`test.bfc`は同じ名前の入力関数と出力macroを埋め込みsourceとcapture bufferへ接続するため、
production実装を変えずに次をBF上で検証できる。

- 文字分類と16進digit変換
- comment、keyword、`0x2A`、`'\x21'`、`!`、`!=`を含むstreaming lexer
- block scopeとshadowingを扱うsymbol table
- 定数設定や破壊的transferを出力するBF codegen
- `if`、`else`、`while`、`!`、比較を含む第4段階direct parser
- arena page境界、full AST、前方callの名前解決、Continuation IR、ABI出力

内部testの追加時は任意の`*_test.bfc`へtest関数を定義し、`tests/test.bfc`の`main`から明示的に
呼び出す。

## 対応する入力

`LANGUAGE.md`の初期実装第1〜5段階から、次を受理する。

- ちょうど1つの`void main()`とscalar/void function定義
- scalar parameter、前方call、直接・相互再帰、`return`
- nested block、空文、scalar `cell`宣言
- 10進・16進整数リテラルと文字リテラル
- `input()`、`output`、`=`、`+=`、`-=`
- 単項および二項の`+`、`-`
- 単項`!`、`==`、`!=`、`<`、`<=`、`>`、`>=`
- 短絡評価する`&&`と`||`
- `if`、`else`、`while`
- ASCII空白、行コメント、blockコメント

production経路には、identifier 64 byte、function 255個、block nesting 16段、uniform
frameのlocal/temporary 239 cell、packed AST/IR arena 4,096 cellという明示的な制限がある。
制限超過または後段階の構文を検出すると`BFC_STAGE5_ERROR`を出力して停止する。runtime演算は
通常のBFCと同じくmod 256でwrapする。旧第4段階direct parserは内部回帰test用に残している。

## 検証

repository rootで次を実行する。

```console
scripts/verify-stage2-selfhost.sh
```

検証scriptは最初に`test.bfc`版をBFへ変換して内部testの`ok`を確認する。続いて`main.bfc`版を
連結して二段階のコンパイルを実行する。生成結果にエラーmarkerやBrainfuck以外のbyteがないことを
調べた後、前方call、直接・相互再帰、連続scalar return、void call、比較、`while`、短絡する
`input()`を含む生成programを実行し、期待するbinary出力と比較する。

## セルフホスト時のテープ容量

通常targetとの互換性確認には30,000 cellを使う。ただし、full AST arenaを含むcompiler
artifactは`bfc --unlimited-tape`で生成し、`bf-interpreter --unlimited-tape`で実行する。
完全なセルフホストcompilerの
開発・bootstrap・検証で不足する場合は、BF interpreterとcompiler backendのテープ上限を
動的拡張または実質無制限にしてよい。30,000 cellへ収めるためだけに言語機能を大幅に削ったり、
compilerを過度にmemory tuningしたりすることは目標にしない。セルフホスト経路が成立した後、
必要なら別途profileを取り、有限target向けの現実的な構成を検討する。

現在のpacked AST arenaは16 page、4,096 logical cellである。handle自体はpage/slotの
16-bit形式を保ち、容量超過はcompile errorにする。65,536-cell arenaは意味上は扱えるが、
現ABI backendではglobalとframe間の絶対pointer移動により生成BFが過大になるため、static
region navigationを距離非依存にしてからpage数を引き上げる。
