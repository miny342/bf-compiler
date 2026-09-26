# BFC Brainfuck ABI — 共通規約

この文書は、Rust製backendとBFC製selfhost backendに共通する実行モデルを説明する。
物理的なデータ配置とhelperの規約は、次の文書に分ける。

- [ABI-rust.md](ABI-rust.md): Rust製`bf-compiler`のContinuation IR backend。
- [ABI-selfhost.md](ABI-selfhost.md): `selfhost/stage2/compiler/`のBFC製BF backend。

ここでいうABIは、一つの生成BF内で関数・dispatcher・配列accessorが守る内部実行規約である。
別々にコンパイルしたBFをlinkするためのbinary interfaceや、共有libraryとの境界は定義しない。
両backendでframe幅、field順序、globalの物理位置などが一致する必要はない。
それぞれのbackendがprogram全体を、自身の規約で一貫して生成する。

旧ABI.mdのchunk構造と「version 0 / version 1」はRust側の設計を記述していたため、
[ABI-rust.md](ABI-rust.md)へ移した。「セルフホスト拡張version 1」はセルフホストに必要な
aggregate等の機能をRust側へ追加したという意味であり、BFC製backendとのABI互換性を示さない。

## どちらのABIが使われるか

生成物のABIは、最後にBFを生成するbackendで決まる。

| 生成経路 | 最終BFのABI |
| --- | --- |
| BFC source → Rust `bfc` → BF | Rust |
| source → stage2の`main` / `compressed` / `profile` entry → BF | selfhost |
| source → stage2の`cir` entry → CIR → Rust `bfc --cir-input` → BF | Rust |

Rustの`--run-ir`でBFC製コンパイラを実行しても、そのコンパイラのBF backendが出力するBFは
selfhost ABIになる。反対に、BFC製frontendからCIRを受け取ってRust backendがBFを出力する場合は
Rust ABIになる。コンパイラ自身を動かす方式と、そのコンパイラが生成するprogramのABIは別である。

CIRは論理的なframe slot、global address、continuation等を渡すための中間形式である。
Rust側の[selfhost_cir_adapter.rs](crates/bf-compiler/src/selfhost_cir_adapter.rs)はこれをRustの
Continuation IRへ変換し、Rust backendが改めて物理配置を作る。selfhostの物理テープを保持したり、
二種類のBFを接続したりする仕組みではない。IRの詳細は[IR.md](IR.md)を参照。
Rust backendを使う経路同士でも、frontendや最適化が異なればslot数やregionの分け方が変わり得る。
同じABIに従うことは、同じsourceから常に同一のテープ配置を生成するという意味でもない。

## 共通する値の意味

言語仕様は[LANGUAGE.md](LANGUAGE.md)、BFのcell・テープ・入出力の実行仕様は[SPEC.md](SPEC.md)に従う。
以下は両backendが受理するprogramについて共通する規約であり、受理できる型サイズやframeサイズの
上限まで同じであることを意味しない。

```text
cells(cell)       = 1
cells(enum E)     = 1
cells(struct S)   = field宣言順のcell数の合計
cells(T[N])       = N * cells(T)
```

structはfield宣言順、配列はrow-majorで論理的にflattenする。`T[N][M]`ではouter arrayの長さが`N`、
inner arrayの長さが`M`である。protocol cell、allocation flag、padding等はこのcell数に含めない。
これはlogical payloadの順序であり、payloadがBFテープ上でも連続するという意味ではない。

aggregateは値渡し・値返しとする。calleeのparameterを変更してもcallerの元の値は変更しない。
引数は左から右へ評価し、aggregate引数は後続引数の評価で元の値が変わっても影響されないように
snapshotする。代入も右辺を評価してから値をcopyする。最適化はこの観測可能な意味を保つ。

ソース言語は物理address、pointer、reference、`inout`を公開しない。
enumの型identity、method call、macro、文字列、`len`等はfrontendで処理する。
文字列は`cell[N]`の値であり、ABIが末尾NULや文字列headerを追加することはない。

## Activationとcall/return

各activationはparameter、local、temporary、戻り先continuationを保持する。
通常の動的callはcalleeのactivationを新しく確保し、callerのlocal値をそのframeへ残す。
直接再帰と相互再帰も同じ規約で扱う。frameの必要量はcompile timeに決まり、call深度が実行時に変わる。
Rust側の実験的static frame最適化の例外は[ABI-rust.md](ABI-rust.md#実験的static-frame)を参照。

scalar returnはcallerの`VALUE`に結果を届ける。`void` returnはcallerの`VALUE`を0にする。
aggregate returnはcallerが所有するoutboxへpayloadを届け、resume後にcallerが結果を回収する。
outboxの容量の決め方と位置はbackendごとに異なる。再帰中も結果の保存先はactivationごとに分かれる。

正常returnはcallerの次のPCへ戻り先を書き、calleeの動的frameをclearしてcallerを再開する。
outboxやcopyに対する最適化は、この値の所有関係を保つ範囲で行える。

## Continuationとdispatch context

関数本体はcallの前後などでcontinuationへ分ける。dispatcherが現在のcontextから次に実行する
continuationを選ぶ。frameと共有portalは、それぞれのbackend内で共通のdispatch fieldを持つ。
fieldの意味は共通していても、field番号や物理offsetは共通ではない。

```text
ACTIVE       trampolineを継続するか
PC           現在選択するcontinuationのdispatch code（2 byte）
NEXT_PC      次のdispatchへ渡すcode（2 byte）
RETURN_PC    callerまたはportal access siteのresume code（2 byte）
VALUE        scalar結果またはportalで搬送するbyte
work cells   比較・copy・dispatch等のcompiler-owned temporary
```

PCの全体が0のcodeは通常のdispatch対象に使わない。片方のbyteが0のcodeは有効である。
論理continuation ID、backendが採番するdispatch code、コンパイラ内部のarena handleは区別する。
実際のcodeの採番とpage分割はbackend内部の判断であり、両実装で一致しなくてよい。

```text
while current_context.ACTIVE != 0:
    PCに対応する処理を選び、選択用PCを消費する
    bodyを実行する
    call / return / portal等が次のcontextとNEXT_PCを決める
    next_context.PC = move(next_context.NEXT_PC)
    pointer = next_context.ACTIVE
```

call後はcallee、return後はcaller、共有portalへの移行後はportalの`ACTIVE`でloopを判定する。
同じ反復中に別のcaseまで実行しないよう、次のcodeは`NEXT_PC`へ置く。
Rust側では同じframe内の複数continuationをまとめる最適化もあるため、これはdispatch境界の説明である。

## Globalと動的aggregate access

globalは再帰の深さによらず一つの静的領域を参照する。両backendの通常の動的frame方式は
zero anchorと使用中frameを表すcell列を走査してglobalと現在frameを行き来する。
Rustではchunk head、selfhostではframeの`ACTIVE`を走査するので、そのstrideや戻り位置は異なる。

定数projectionはcompile timeに位置を解決する。動的projectionは、評価済みのindexと型layoutから
次の論理offsetを作る。

```text
offset = constant field/subobject offset + sum(index * cells(indexed element type))
```

offsetはcompiler-ownedの16-bit値であり、ソース言語の整数型やpointerではない。
動的loadは元のpayloadを保存し、storeは選択したpayloadを書き換える。
複数cellの値はlogical leaf順にcopyし、管理cellをpayloadとしてcopyしない。

共有portalは要求からresumeまで非再入の操作であり、途中でuser functionを呼び出さない。
この間だけglobalのprotocol cellを共有scratchとして使える。portalを使う条件、配置する単位、
offsetの上限、小さい配列をinline展開するかどうかは各backendの仕様に従う。

動的な範囲外indexの意味は言語仕様上未定義である。selfhostのaccessorが一部の範囲外offsetに対して
zero loadやwrite無操作を実装していても、Rustやソース言語全体にその保証を広げない。

## 初期化と終了

初期BFテープはすべて0とする。global initializerは宣言順に実行し、その後にparameterなしの
`void main()`本体へ進む。global initializer内のcallに必要なcontextは、後にmainが使うroot frameに
先に用意できる。物理的なglobalの順序とinitializerの実行順序は別である。

mainの終了はroot contextの`ACTIVE`を0にしてtrampolineを終了する。`abort()`は任意のactivationで
現在の`ACTIVE`を0にし、ancestorへreturnせず終了する。この場合にframeやglobalをclearする義務はない。

## 物理配置の主な違い

| 項目 | Rust backend（既定） | selfhost BF backend |
| --- | --- | --- |
| 確保単位 | head 1＋data 16の17-cell chunk | 全関数共通の`W`-cell frame |
| frame幅 | 関数ごとに異なるchunk数 | `W = 16 + max(slots) + max(aggregate return cells)`、最大255 |
| header位置 | frame末尾のcontext chunk | frame先頭の16 cell |
| `ACTIVE` / `VALUE` | context headから`+2` / `+1` | frame baseから`+0` / `+7` |
| 使用中領域の走査 | 各chunkのhead flag | 各frameの`ACTIVE` |
| local aggregate | regionごとのportalとchunked payload（scalar化されたものを除く） | 通常のframe dataに連続配置 |
| global portal | aggregate rootごと、256 payload cellごと | 必要なprogramのみ、global論理空間全体の256-cell pageごと |
| outbox | 関数ごとの容量、context下でchunk番号が増えるほど左へ伸びる | 全関数共通の容量・offset、data末尾で右へ伸びる |

変更時は、共通する意味が変わる場合にこの文書を更新し、物理配置・helper規約の変更は該当する
backendの文書と実装を更新する。片方のABI変更だけを理由に、もう片方の配置も揃える必要はない。
