# BFC Brainfuck ABI — selfhost backend

この文書は、`selfhost/stage2/compiler/`のBFC製backendが直接生成するBFの現行仕様である。
共通する値の意味と実行モデルは[ABI.md](ABI.md)、Rust製backendの配置は[ABI-rust.md](ABI-rust.md)を参照。
旧文書のRust ABI version 0 / 1とは別の物理配置を使用する。

`main`、`compressed`、`profile` entryは同じ配置を使う。BFの圧縮形式やprofile markerは配置を変えない。
ソース連結時の`--enable-nibble-transfer`は、anchor手前に共有scratchを6 cell追加する。
logical global address、frame幅、PC、CIRは変更しない。既定ではscratchを追加しない。
`cir` entryはこのBF backendを通らず、Rust `--cir-input`へ渡した後のBFにはRust ABIが適用される。
BFC製コンパイラをRustの`--run-ir`で実行する場合も、直接生成するBFには本仕様が適用される。

主な実装箇所は次のとおり。

- [08_semantic.bfc](selfhost/stage2/compiler/08_semantic.bfc): 型layout、logical global base、parameter/local slot。
- [09_continuation_ir.bfc](selfhost/stage2/compiler/09_continuation_ir.bfc): temporary、引数snapshot、call/return、global initializer。
- [10_abi_codegen.bfc](selfhost/stage2/compiler/10_abi_codegen.bfc): uniform frame、header、dispatch、call/return、inline array access。
- [10_portal_codegen.bfc](selfhost/stage2/compiler/10_portal_codegen.bfc): global page配置、共有accessor、request/resume。

## Uniform frame

`calculate_stage8_frame_size`は、lowering済みの全関数から一つのframe幅を決める。
`slots(f)`にはparameter、local、scalar/aggregate temporaryを含み、headerとoutboxは含めない。

```text
M = max(slots(f))                         // 全関数の最大slot数
O = max(aggregate return cells(f))        // aggregateを返す関数がなければ0
stage8_outbox_base  = M                   // data領域内のlogical slot
stage8_outbox_cells = O
W = stage8_frame_cells = 16 + M + O

physical_frame_slot(q) = 16 + q
```

`W <= 255`、すなわち`M + O <= 239`でなければcompile errorとする。
`M`と`O`は別々に全関数から最大値を取る。ある関数がaggregateを返すcalleeを持たなくても、
program全体の`O`が非ゼロなら、その関数のactivationにも同じoutboxを予約する。
同様に、slotの少ない関数にも`M` cellのdata領域を予約する。

frame baseを`B`とすると配置は次のとおり。

```text
B                B+16                 B+16+M             B+W
| header[16]     | data[M]             | outbox[O]        | next frame
  ACTIVE at B      slot q at B+16+q      leaf i at B+16+M+i
```

配列・structは通常のdata slotに連続してflattenする。local aggregate専用のhead、portal prefix、
chunk paddingはない。Rust側の`D = 16`、`S = 17`をこの配置へ適用してはならない。
`FRAME_DATA_BASE = 16`はheaderの長さであり、frame幅やchunk幅ではない。

## Frame header

offsetはheadの次のcellからではなく、frame baseそのものから数える。

| Offset | 定数名 | 用途 |
| --- | --- | --- |
| 0 | `FRAME_ACTIVE` | trampoline継続判定とactivation使用中flag |
| 1 | `FRAME_PC_LOW` | 現在のdispatch code low byte |
| 2 | `FRAME_PC_HIGH` | 現在のdispatch code high byte |
| 3 | `FRAME_NEXT_PC_LOW` | 次のdispatch code low byte |
| 4 | `FRAME_NEXT_PC_HIGH` | 次のdispatch code high byte |
| 5 | `FRAME_RETURN_PC_LOW` | caller/site resume code low byte |
| 6 | `FRAME_RETURN_PC_HIGH` | caller/site resume code high byte |
| 7 | `FRAME_VALUE` | scalar return、byte搬送 |
| 8 | `FRAME_CONDITION` | 条件評価scratch |
| 9 | `FRAME_RESTORE` | copy時の復元scratch |
| 10 | `FRAME_BRANCH` | countdown caseの選択flag |
| 11..14 | `FRAME_SCRATCH_0..3` | 演算・offset・配列accessのscratch |
| 15 | 対応する`FRAME_*`定数なし | nibble要求搬送のhigh digit、global portalでは復路counter |

headerへparameter/localを割り当てない。portalでは同じ16-cell contextを使い、work cellを
要求packet用に読み替える。`VALUE`やscratch全体がすべてのinstruction境界で0になるという規約はない。
各helperが必要なcellを初期化し、liveなPC、戻り先、local値を保つ。

## Globalの論理配置と物理配置

`stage7_static_cells = G`をglobal payloadの総cell数とする。
`N = 6`（`NIBBLE_BF_TRANSFER = 1`）、それ以外は`N = 0`を共有scratch幅とする。
各globalには24-bitのlogical baseを
割り当て、paddingなしでpayloadを詰める。型のflatten順序は[共通規約](ABI.md#共通する値の意味)に従う。

現行の`layout_global_group`は低addressから次の順にbaseを割り当てる。

1. 16 cellを超えるaggregateのうち、サイズが`MAX_NAME`と等しくないものを宣言順。
2. サイズが`MAX_NAME`（現在64 cell）のaggregateを宣言順。
3. 16 cell以下のaggregateを宣言の逆順。
4. scalar globalを宣言順。

2はtoken buffer等をanchor寄りへ置くための、サイズに基づく分類である。
Rustの大きいaggregateを逆宣言順にする配置とは一致しない。global initializerの評価順序は
この並べ替えによらず宣言順のままである。

### 共有portalが不要なprogram

`stage12_portals == 0`なら、logical addressがそのままphysical addressになる。

```text
physical_global(a) = a
physical_static_cells() = G + N
```

大きなglobalが宣言されていても、共有portalが必要な動的accessがなければprefixを挿入しない。

### 共有portalを使うprogram

`prepare_array_portals`は、global動的accessの対象regionが256 cell以上なら、そのinstructionを
共有accessorへの要求とresumeに分割する。操作はload/store/add/subtractの4種類である。
対象regionの長さは1〜65,535 cellの表現範囲内でなければならない。

このようなaccessが一つでもあると`stage12_portals = 1`になり、scalarも含むglobal論理空間全体を
256-cell pageに区切り、各pageの前に16-cell headerを置く。
regionごとに専用prefixを挿入するのではなく、複数のglobalが同じpageを共有できる。

```text
page(a)                 = floor(a / 256)
physical_page_origin(a) = page(a) * 272
physical_global(a)      = page(a) * 272 + 16 + (a mod 256)
physical_static_cells() = ceil(G / 256) * 272 + N

| header[16] | global payload[0..255] |
| header[16] | global payload[256..511] | ...
```

最後のpageも256 payload cell分を確保する。宣言されたpayloadを越える部分はpaddingである。
定数global accessや255 cell以下のinline accessも同じ変換を使うので、動的accessと同じcellを参照する。
aggregate baseがpage境界に一致する必要はなく、配列の途中やstructのfield境界にpage headerが入り得る。

## Anchorとactivation stack

`H = physical_static_cells()`をzero anchorの位置とする。root frameは`H + W`から始まる。

```text
static globals | H: zero region[W] | root frame[W] | callee[W] | ... | free frame
                 anchor=0           ACTIVE=1       ACTIVE=1          ACTIVE=0
```

`[H, H+W)`は0のままの領域で、最初のcellをanchorとして使う。Rustの17-cell anchor chunkや
anchor scratchとは異なる。depth 0をrootとすると、activationのbaseは次の式になる。

```text
B(depth) = H + (depth + 1) * W
caller base = B - W
callee base = B + W
```

使用中の全activationは、現在実行されていないcallerも含めて`ACTIVE = 1`を保つ。
anchorと最初の未使用frameの`ACTIVE`は0である。globalへの移動は`W` strideの走査で行う。

```text
current -> anchor:
    pointer = current frame base
    while *pointer != 0: pointer -= W

anchor -> current:
    pointer = H + W
    while *pointer != 0: pointer += W
    pointer -= W
```

右走査は最初の未使用frameで止まってから1 frame戻る。そのためhelperが返るのはcurrent frame baseであり、
Rust文書でいうfrontier headそのものではない。
anchorからglobalへの距離は`H - physical_global(a)`で、24-bitの`WideValue`で計算する。
使用中flagとanchorはglobal往復で変更しない。

### 選択式nibble搬送

`NIBBLE_BF_TRANSFER = 1`なら`[H-6,H)`を共有scratchとして予約し、global→frameのcopyに使う。
global byteを消費しながら、固定距離移動でscratchの下位4bitをincrementして上位nibbleへcarryする。
中間byte bufferと、それを使う追加の移動template/loopは出力しない。
二つのdigitからglobalの元値とframeのdestinationを復元する間だけstackを走査する。
scratchはhelper境界で全cellが0、source globalは保存される。
この操作はuser functionを呼ばず、別のglobal/portal操作とinterleaveしない。

共有portal要求ではpayloadとoffset low byteの搬送を分解する。validityをstageした後で
使い終えたcaller headerの9、11..15をscratchにする。低nibbleを11、高nibbleを15へ作り、
搬送後は全scratchが0、元のsource byteも消費される。
PC・戻り先・dispatcherのBranchを保存し、portal側に既にstage済みのvalidity/carry/payloadも保持する。
loadのresumeでは、使い終えたpage headerの9、11..15でpayloadを直接分解し、元のportal fieldを消費してcallerへ届ける。
anchor scratchへのbyte搬送を挟まず、ACTIVE・PC・Branchを保つ。
high byteの搬送、page間payload搬送、page番号の既存nibble分解、frame間の引数・戻り値搬送はこのflagの対象外。

## Callとreturn

call前に引数を左から右へ評価してcaller temporaryへ保持する。aggregate引数も、後続の引数を
評価する前に全leafをsnapshotする。`emit_call_terminator`は次を行う。

```text
callee base = caller base + W
clear callee[0..W]
callee.ACTIVE = 1
callee.NEXT_PC = callee entry
callee.RETURN_PC = caller resume
copy evaluated argument leaves to callee parameter slots
pointer = callee base
```

引数転送は隣のframeへの定数距離copyであり、この途中にuser functionやglobal scanを挟まない。
return後のclearに加えてcall時もframe全体をclearする点は、Rustの通常allocationと異なる。

`emit_return_terminator`は戻り値の種類に応じて次の処理を行う。

- scalar: 結果をcalleeの`VALUE`へcopyし、callerの`VALUE`をclearして結果をmoveする。
- void: callerの`VALUE`をclearする。
- aggregate: 各leafをcalleeの`VALUE`で中継し、callerの`B + 16 + M + i`へ届ける。

aggregateの場合も最終的な保存先はcallerのoutboxである。ただしbyte搬送のscratchとして`VALUE`を
使うので、Rust側の「aggregate returnには`VALUE`を使用しない」をそのまま当てはめられない。
outboxのoffsetは全frameで同じで、logical leafの番号が増えるほど右へ進む。

その後はcalleeの`RETURN_PC`をcallerの`NEXT_PC`へmoveし、calleeの`W` cellすべてをclearして
pointerをcaller baseへ戻す。dispatcher末尾がcallerの`NEXT_PC`を`PC`へmoveする。
resumeの`IR_COPY_ABI_VALUE`または`IR_COPY_OUTBOX`が結果をcaller-owned temporaryへcopyする。
outboxを読んだだけではclearせず、次のreturnで使うleafを上書きする。

## Dispatcherとpointer位置

continuationのarena `NodeId`は24-bit handleであり、BFのPCは別途採番する2-byteのdispatch codeである。
0は予約し、continuation追加時に65,535を超えるIDを拒否する。
共有portalを使うときは4 accessorを先頭に予約し、site固有のresumeを元のcontinuationの直後へ挿入する。

`balance_dispatch_ids`はBFを出力する前にpage幅を調整する。
entry数が256以下ならpage幅256を使い、それを超える場合は原則`ceil(sqrt(count + 1))`にする。
codeはlow byteをpage幅で繰り上げながら再採番するため、元の連番IDと一致するとは限らない。
page番号とpage内のcodeは連続しており、high/lowの両段に破壊的countdownを使う。

選択されたcaseは`FRAME_BRANCH`を消費して一度だけ実行する。移動先contextの`PC`とbranch flagも
残りのcaseを誤実行しない状態に保ち、次のcodeは`NEXT_PC`へ書く。
dispatcher末尾が現在のcontextの`NEXT_PC`を`PC`へmoveし、offset 0の`ACTIVE`で反復を判定する。

codegenの`position`は現在context baseからのoffsetである。frameを変えるhelperはbaseへ移動して
`position = 0`へ基準を更新する。portal request/resumeも同様に原点を切り替える。
個々のhelperの途中ではscratch上などにpointerを置けるが、trampoline終端は次のcontextのoffset 0である。

## 動的aggregate access

source indexは1 cellのまま、型のstrideとfield offsetを使ってlogical offsetを2-byte temporaryへ計算する。
low byteの繰上がりをhigh byteへ反映する。これはcompiler-owned temporaryでありpointerではない。
複数leafのaggregate accessはsnapshotとleafごとの処理にloweringする。

### Localと小さいglobalのinline access

local、および対象regionが255 cell以下のglobalでは、`emit_array_instruction`がcall site内に
countdownを生成する。frame contextに留まったまま対象要素を選ぶので、共有accessorへのdispatchや
local専用portal prefixは不要である。globalの選択要素にはanchor経由で移動する。

高位offsetが0で、低位offsetがregion長未満のcaseだけを実行する。
loadは事前にdestinationを0にし、有効なcaseでは元のpayloadを保存してcopyする。
store/add/subtractは選択したpayloadだけを変更する。

### 大きいglobalの共有portal

regionのlogical baseを`b`、payload offsetを`o`とする。requestのcontextは`b`を含むpageのheaderである。
`o < region length`を判定した後、`b mod 256`をoffset lowへ足し、そのcarryもpage移動へ反映する。
このため有効な16-bit offsetでも、baseのずれによってbase pageから256 page先へ進む場合がある。

page headerはframeと同じ16-cell contextだが、accessor内では次のように使う。

| Offset | Portalでの用途 |
| --- | --- |
| 0..6 | `ACTIVE`、`PC`、`NEXT_PC`、siteの`RETURN_PC` |
| 7 | 搬送するbyte（load結果、store/add/subtractの値） |
| 8 | baseのずれを加えたpage内offset |
| 9 | base pageでの有効範囲guard、load copyの復元scratch |
| 10 | payload countdownのbranch flag |
| 11 | 分解前のoffset high byte |
| 12..13 | 往路の1-page / 16-page移動counter |
| 14 | baseのずれによるcarry、次いで復路の1-page counter |
| 15 | 復路の16-page counter |

requestはbase pageのheaderをclearしてaccessorとresumeのcodeを設定し、caller temporaryから要求を
移した後、そのpageへdispatcher contextを切り替える。callerの`ACTIVE`は1のままにする。

accessorはpage数をnibbleへ分解し、16-page stride（4,352 cell）と1-page stride（272 cell）で
必要なfieldだけを搬送する。payloadそのものを入れ替えることはない。選択したpageで256 caseの
countdownを実行し、同じ経路を逆向きにたどってbase pageへ戻る。
このpayload selectorはload/store/add/subtractごとに一つで、配列長だけcase全体を複製しない。

終了時はbase pageの`RETURN_PC`を`NEXT_PC`へmoveする。site固有のresumeがload結果をcallerへ
回収し、base pageの16 cellをclearしてanchor経由でcurrent frameへ戻る。
中間pageのheaderにはscratchが残り得るので、page間搬送は書込み先fieldを使用前にclearする。
requestからresumeまでuser codeや別portalを実行せず、global pageの管理領域を非再入で共有する。

現行accessorは、計算後のoffsetがregion長以上ならloadを0、writeを無操作にする。
これはbackendの動作であり、各次元の範囲外indexを検出する言語保証ではない。
多次元indexが範囲外でもflatten後のoffsetがregion内に収まる場合や、offsetがwrapする場合まで
安全性を保証するものではなく、source-levelには引き続き未定義動作である。

## 初期化、終了、制限

初期テープが0であることを前提に、`emit_continuation_program`は`H + W`へ移動してrootの
`ACTIVE = 1`、`PC = main entry`を設定する。`lower_ast_program`はmain entryの先頭にglobal initializerを
宣言順で組み込み、その後でmain本体を実行する。
mainのreturn/末尾は`TERM_HALT`、任意activationの`abort()`は`TERM_ABORT`となり、いずれも現在の
`ACTIVE`をclearしてtrampolineを終了する。終了時に全frameを解放する必要はない。

| 対象 | 現行の制限 |
| --- | --- |
| frame全体 | `16 + M + O <= 255`。dataとoutboxの合計は239 cellまで |
| local / parameter / aggregate return | frameへmaterializeするため1-byteサイズ。上記の合計制限も満たす必要がある |
| BF dispatch ID | 1〜65,535。portal accessorと追加resumeもこの空間を使う |
| global base / 型サイズ / field offset | 24-bitの`WideValue`。加算・乗算のoverflowを拒否 |
| BF動的access region | 長さ1〜65,535 cell、offsetは16 bit。65,536-cell regionは受理しない |

このBF backend自体は、static領域とroot frameを合わせた30,000-cellへの収容判定を行わない。
実行時には生成BFを動かすinterpreterのテープ容量を満たす必要がある。`--unlimited-tape`で
interpreterやRust側のcapacity checkを緩和しても、stage2内部のframe幅やoffsetの表現は拡張されない。
lexerやarena等のコンパイラ自身の制限は[stage2 README](selfhost/stage2/README.md#整数幅とabiの制限)を参照。

配置とaccessの既存回帰は[scripts/verify-selfhost-portals.py](scripts/verify-selfhost-portals.py)、
幅の境界は[scripts/verify-stage2-limits.py](scripts/verify-stage2-limits.py)、
call/return・aggregate・再帰を含むbootstrap全体は[scripts/verify-stage2-selfhost.sh](scripts/verify-stage2-selfhost.sh)で検証する。
