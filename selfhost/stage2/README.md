# 第12段階bootstrap compiler

このディレクトリには、最初に実行可能になったセルフホスト用の小さなコンパイラを置く。
まずRust版コンパイラでBFC製コンパイラをBrainfuckへ変換し、そのBrainfuckプログラムで
初期実装第1〜12段階のBFCをBrainfuckへ変換する。

## ファイル構成

コンパイラ本体は役割ごとに分割している。

- `compiler/00_definitions.bfc`: production定数、token型、global状態
- `compiler/00_legacy_stage4.bfc`: 内部test専用の旧direct parser状態
- `compiler/01_common.bfc`: エラー終了、文字分類、小さな算術補助
- `compiler/02_lexer.bfc`: streaming字句解析
- `compiler/03_symbols.bfc`: keyword判定とtoken消費
- `compiler/03_legacy_symbols.bfc`: 内部test専用の旧scalar symbol table
- `compiler/04_codegen.bfc`: productionと旧parserで共有するBrainfuck出力primitive
- `compiler/04_legacy_codegen.bfc`: 内部test専用の旧direct codegen helper
- `compiler/05_parser.bfc`: 内部test専用の第1〜4段階direct parser
- `compiler/06_arena.bfc`: 24-bit bank/page/slot handle、packed arena、identifier intern
- `compiler/07_ast_parser.bfc`: 第12段階surface syntaxのfull AST構築
- `compiler/07_macro_expansion.bfc`: block macroの検証、衛生的AST複製、nested展開
- `compiler/08_semantic.bfc`: nominal型layout、function収集、名前解決、型検査
- `compiler/09_continuation_ir.bfc`: typed ASTからContinuation IRへのlowering
- `compiler/10_abi_codegen.bfc`: static global、array portal、uniform frame、aggregate outbox ABI
- `compiler/main.bfc`: production標準入出力とentry point

BF上で動くBFC製コンパイラの入力は1本のbyte streamなので、セルフコンパイル時は連結して渡す。
`scripts/concat-stage2-compiler.sh main`がproduction sourceを連結し、各ファイル境界へ改行を補う。
`00_legacy_stage4.bfc`、`03_legacy_symbols.bfc`、`04_legacy_codegen.bfc`、`05_parser.bfc`はproductionの
到達可能性に関与しないため除外する。`test` entryだけは旧回帰を維持するためこれらも連結する。

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
- arena page/bank境界、full AST、前方callの名前解決、Continuation IR、ABI出力
- local配列の連続slot配置、定数式添字の畳み込み、宣言ごとのゼロ初期化
- static global layout、宣言順initializer、local/global配列の動的添字
- 配列の値渡しとsnapshot、全体代入、activation固有outboxによるaggregate return
- enum/struct、多次元配列、nominal型検査、field/index projection
- 文字列escape、`cell[]`長さ推論、`len`の非評価、`const cell`と定数名の配列長
- 16-bit logical offsetの構築、aggregate elementと多次元の動的projection portal
- method call糖衣、definition/call-site名前衛生を持つblock macro、program全体の`abort`

内部testの追加時は任意の`*_test.bfc`へtest関数を定義し、`tests/test.bfc`の`main`から明示的に
呼び出す。

## 対応する入力

`LANGUAGE.md`の初期実装第1〜12段階から、次を受理する。

- ちょうど1つの`void main()`とscalar/array/void function定義
- scalar/array parameter、前方call、直接・相互再帰、scalar/aggregate `return`
- nested block、空文、scalar `cell`宣言
- scalar/aggregate globalと、宣言順に実行するinitializer
- payloadなし`enum`、nominal `struct`、任意要素型・多次元の固定長配列
- enumの`Type::Variant`、struct fieldの`.`、定数添字の多段projection
- 型が一致するaggregate initializer、全体代入、値渡し、値返し
- local/globalのaggregate要素と多次元配列に対する動的な多段projection
- 文字列リテラル、直接文字列initializerによる`cell[]`長さ推論、compile-time `len`
- file-scopeの`const cell`、forward定数参照、定数名を使う配列長
- receiverを第1引数にするmethod call糖衣
- file-scope block macro、nested macro、expression/place parameter、fresh localとdefinition-site free name
- 展開先functionからのmacro `return`と、任意のactivationから即時停止する`abort()`
- 10進・16進整数リテラルと文字リテラル
- `input()`、`output`、`=`、`+=`、`-=`
- 単項および二項の`+`、`-`
- 単項`!`、`==`、`!=`、`<`、`<=`、`>`、`>=`
- 短絡評価する`&&`と`||`
- `if`、`else`、`while`
- ASCII空白、行コメント、blockコメント

production経路には、identifier 64 byte、function 255個、block nesting 16段、uniform
frameのlocal/temporary 239 cell、16-bit dynamic aggregate offset、packed AST/IR arena
261,120 cellという明示的な制限がある。型layout、struct field offset、static globalのbase/sizeは
little-endian 24-bit値で保持し、配列長256と`cell[255][256]`のようなlarge globalを受理する。
streaming parserは、function bodyの
local宣言を開始するnominal型定義がそのfunctionより前に現れることを要求する。
制限超過または後段階の構文を検出すると`BFC_STAGE12_ERROR`を出力して停止する。runtime演算は
通常のBFCと同じくmod 256でwrapする。動的projectionはlow/high byteのlogical offsetを
左から右へ各indexを1回ずつ評価して構築する。非placeのaggregate式への動的projectionは、
frameへmaterializeできる239 cell以下の値だけに対応する。旧第4段階direct parserは内部回帰test用に
残している。

Continuationにはarena上の`NodeId`とは別に1始まりの密な16-bit dispatch IDを割り当てる。
ABI backendはhigh byteのpage選択とpage内low byteの両方を破壊的countdownでdispatchし、
caseごとのPC copy/restoreと定数比較を行わない。call先のPCは移動先contextの`NextPc`へ設定し、
dispatch cycle末までは`Pc`を0に保つ。

arena recordは`next`と頻出fieldを前方へ置いた4〜20 cellのkind別layoutを使用する。
Continuation、global、wide addressを保持するname/index/field expressionは20 cellである。
literal、input、unary、binary、
`len`の結果型は`cell`から自明なのでhandleを保存しない。その他のexpressionはsemantic解決後に
source名fieldを型handleとして再利用する。これにより型情報専用fieldを増やさず、profileした
node領域を30.5%削減する。scalar literalは値を短いrecord内へ詰め、parse時に連続して確保された
右辺literalはbinary recordの未使用fieldへ即値として埋め込む。両辺literalのbinary式とliteralへの
unary式はその場で畳み込み、不要になった末尾recordをarenaへ戻す。

static globalsの右にzero anchorを置き、各activationの`Active`をallocation flagとして保存する。
global accessはcurrent frameからanchorへ左走査し、処理後にanchorからfrontierへ右走査して同じ
activationへ戻るため、再帰深度によらず単一のstatic領域を参照する。動的添字はsource indexを
一度だけframe temporaryへ評価し、local/global共通の破壊的countdown portalで対象要素を選択する。
配列引数は後続引数の評価前にcaller temporaryへ完全にsnapshotする。配列returnは全functionで
必要な最大長を予約したcaller固有outboxへcopyし、resume continuationが直ちに所有temporaryへ回収する。

## 検証

repository rootで次を実行する。

```console
scripts/verify-stage2-selfhost.sh
```

巨大なbootstrap BFを生成・実行する前に、同じBFC製compilerをContinuation IR上で直接動かせる。
出力は逐次書き出され、進捗とmemory指標は標準エラーへ出る。

```console
scripts/concat-stage2-compiler.sh main > stage2-compiler.bfc
bfc --run-ir --ir-progress-interval 1s stage2-compiler.bfc \
  < stage2-compiler.bfc > stage2-self.bf 2> stage2-self.metrics
```

この経路はfrontend/loweringとselfhost compiler本体の高速な診断用である。生成したcompilerを
Brainfuck VM上で動かす回帰は、引き続き`verify-stage2-selfhost.sh`で確認する。

production selfhostでは、selfhost frontendがcompact binary CIRを出力し、Rust ABI backendへ
渡せる。`cir` entryだけがserializerを含み、targetの`main` entryには含めない。

```console
scripts/concat-stage2-compiler.sh cir > stage2-cir-compiler.bfc
scripts/concat-stage2-compiler.sh main > stage2-compiler.bfc
bfc --run-ir --ir-progress-interval 15s stage2-cir-compiler.bfc \
  < stage2-compiler.bfc > stage2-compiler.cir 2> stage2-cir.metrics
bfc --cir-input stage2-compiler.cir --unlimited-tape \
  > stage2-compiler.bf 2> stage2-backend.metrics
```

`--cir-input -`でstdinからも読める。decoderはmagic/version、24-bit範囲、frame/global境界、
call layout、CFG所有関係を検証する。backendの最終BF文字列化はrun-length BF IRからstdoutへ
直接streamingし、出力サイズ分の`String`を確保しない。

検証scriptは最初に`test.bfc`版をBFへ変換して内部testの`ok`を確認する。続いて`main.bfc`版を
連結して二段階のコンパイルを実行する。生成結果にエラーmarkerやBrainfuck以外のbyteがないことを
調べた後、第7段階のglobal/portal回帰に加え、配列initializer、全体代入、値渡し、再帰的aggregate
return、引数snapshot、enum/struct、多次元配列、定数/動的field/index projection、文字列、`len`、
`const cell`、method、macro、`abort`を含む生成programを
実行し、期待するbinary出力と比較する。配列のscalar利用、長さの型不一致、定数範囲外アクセス、
enumのzero variant欠落、再帰struct layout、定数循環、不正な配列長推論、macro循環も拒否を確認する。
さらに旧4,096-cell arenaを超える400 statementの入力をコンパイルし、拡張容量を回帰検証する。

## セルフホスト時のテープ容量

通常targetとの互換性確認には30,000 cellを使う。ただし、full AST arenaを含むcompiler
artifactは`bfc --unlimited-tape`で生成し、`bf-interpreter --unlimited-tape`で実行する。
完全なセルフホストcompilerの
開発・bootstrap・検証で不足する場合は、BF interpreterとcompiler backendのテープ上限を
動的拡張または実質無制限にしてよい。30,000 cellへ収めるためだけに言語機能を大幅に削ったり、
compilerを過度にmemory tuningしたりすることは目標にしない。セルフホスト経路が成立した後、
必要なら別途profileを取り、有限target向けの現実的な構成を検討する。

現行flagはstatic layoutとinterpreterの実行時上限を外すが、個々のfunction frameを作る
`FrameLayout`には30,000-cell上限が残る。現在のcompiler frameはこの範囲内であり、大きなglobal arenaを
static regionへ置くためにこのflagを使用している。

第13段階の初期arenaは64 page、16,384 logical cellだった。第13段階の自己入力計測では、固定13-cell
recordが12,567 byteでarenaを使い切ったのに対し、kind別compact recordは15,454 byteまで到達し、
同じ容量で22.97%改善した。旧direct parser専用sourceをproduction連結から除外すると、この構成の
到達位置は15,431 byte、production sourceは165,134 byte、Rust-bootstrap BFは約199 MBになった。
さらに4-cell literal、binary右辺即値、parse時定数畳み込みを導入すると、169,294-byteのproduction
sourceに対して17,726 byteまで到達し、直前構成から14.87%改善した。現在の密度で全sourceへ単純外挿
すると約156,000 cellであり、16-bit handleの65,536-cell上限を超える。

このため現在は`cell[255][256]`を16 bank置き、NodeIdをbank/page/slotの3 cellへ拡張して
1,044,480 logical cellを確保する。bankはarena access helperだけで選択し、Continuationのdispatch IDは
従来どおり独立した16-bit page/slot値を使う。参照fieldが1 cell増えるためrecordも拡大するが、
自己入力の旧密度から見積もった必要量には余裕がある。代償として内部test用のRust-bootstrap BFは
約1.76 GBになる。開発用interpreterはprofileなしの実行時にraw命令配列を作らずFast IRへ直接RLE変換し、
pointer source offsetも連続rangeへ圧縮する。これにより同artifactの内部testは最大RSS約2.56 GBで
実行できる。その後、型・struct offset・global base/sizeを24-bit化し、階層portalで
`cell[255][256]`の最終要素を読み書きする回帰まで通した。

旧経路の自己コンパイル上の制限は、full ASTと生成中Continuation IRを同じarenaへ同居させる空間量だった。
4 bank（261,120 cell）と8 bank（522,240 cell）はIR lowering中に枯渇した。測定用の16 bank
（1,044,480 cell）ではloweringを越えてBF出力へ入ったが、335秒で5.34 GBを出力しても未完であり、
bank追加だけでは最終解にならなかった。

binary CIR経路では184,330-byteのproduction sourceを約248秒、最大RSS約89 MiBで自己入力し、
245 function、5,737 continuationを含む145,210-byteのCIRを出力できた。Rust側はflat frameを
aliasを保つ単一aggregateへ写し、16-bit local offset portalと24-bit global baseを既存ABIへ接続する。
global dynamic regionはsegment化し、scalar globalをanchor近傍へ置き、遠距離portal requestは
regionごとのhidden routerで共有する。測定時の917MB BFは約2.5秒、最大RSS約131 MiBでstreaming
出力できた。最初の素朴な接続は8.42GB、75秒、最大RSS約8.36GBだったため、出力を89.1%、backend
時間を96.7%、peak memoryを98.4%削減した。artifact自体はまだ大きく、routerから各segmentへの
request packet搬送とdispatcher縮約が次のcode-size改善点である。
