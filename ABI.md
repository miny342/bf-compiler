# BFC Brainfuck ABI

この文書は、BFCからBrainfuckへ関数、再帰、ローカル変数、aggregateをloweringするための
実験的な実行時ABIを定義する。

ABI version 0と、その上に定義するセルフホスト拡張version 1の設計仕様である。
現在の`bf-compiler`はversion 1まで実装している。version 0のscalar/array frame、continuation
dispatch、call/return、直接・相互再帰、static global、array portal、aggregate argument/returnに加え、
enum、struct、任意要素型・多次元固定長配列のlogical aggregate layout、16-bit offset portal、
任意のactivationからの`abort`を使用する。定数offsetはlayoutから直接解決し、動的offsetは
local/globalに共通のportal accessorを使用する。sourceのmethod call、macro、文字列、`len`はfrontendで
消費されるためABI機能を追加しない。独立した低水準の検証コードは引き続き
`bf-frame-experiment` crateに置く。

## 目的

このABIの目的は次のとおりである。

- 関数本体を呼び出し箇所へinlineせず、1回だけ生成する。
- 直接再帰と相互再帰を実現する。
- 関数ごとにコンパイル時に確定する大きさのframeを一括確保する。
- callerのローカル値を個別に`push`せず、caller frameへ残す。
- globalは静的位置に保ち、任意の再帰深度から到達できるようにする。
- global/local aggregateに同じchunk内logical offset変換を使う。
- BFデータポインタが動的位置にあっても、ABI境界で位置を一意に解釈できるように
  する。

このABIは、ソース言語へBrainfuckの物理アドレスやポインタを公開しない。

## 基本定数

1 chunkは、1個のhead cellと固定数のdata cellで構成する。

```text
head | data[0] data[1] ... data[D - 1]
```

以下の定数を用いる。

```text
D = CHUNK_CELLS
S = CHUNK_STRIDE = D + 1
R = ARRAY_RESERVED_CELLS
P = DISPATCH_PORTAL_CHUNKS = ceil(R / D)
```

- `D`はコンパイラbackend全体で一つの値を使用する。
- 一つの生成BF内で異なる`D`を混在させない。
- version 0のdefaultは`D = 16`とする。
- `D = 8`を互換なbackend構成として維持し、layout testを通す。
- version 0でbackendが受け付ける値は8または16とする。
- `R = 16`とする。
- `R`は各配列regionと各frame contextの先頭に置くcompiler-owned protocol cell数である。
- `R`の先頭cellはversion 0 array、version 1 aggregate load/storeの`VALUE_PORT`である。
- `P`は`D = 16`で1 chunk、`D = 8`で2 chunkになる。

`D`はBFCソースから観測できる値ではない。変更すると物理配置と生成BFは変わるが、
ソースプログラムの意味は変わらない。

## テープ全体の配置

テープは左から次の領域に分ける。

```text
ordinary globals
| aligned global regions
| anchor chunk
| allocated stack chunks
| frontier chunk
| unused tape
```

概略は次のとおりである。

```text
globals
    |
    v
... | aux data[D] | aux data[D] | anchor=0 abi[D]
                                      |
                                      v
    | flag=1 data[D] | flag=1 data[D] | flag=0 data[D] | ...
         allocated         allocated        frontier
```

anchorより左はコンパイル時に位置が確定するstatic領域、anchorより右は実行時に
伸縮するframe stack領域である。

## Chunk headの役割

同じ物理位置にあるhead cellは、領域ごとに異なる役割を持つ。

| 領域 | headの名称 | 値 | 意味 |
| --- | --- | --- | --- |
| global aligned region | `aux` | 任意 | compiler-ownedなlive cell |
| anchor chunk | `anchor` | 常に0 | static領域とstack領域の境界 |
| allocated stack chunk | `flag` | 1 | 使用中chunk |
| frontier chunk | `flag` | 0 | 最初の未使用chunk |
| frontierより右 | `flag` | 0 | 未使用chunk |

### Global `aux`

global配列はstatic領域にあるため、そのheadをallocation flagとして使う必要がない。
このcellは`aux`として、通常のglobal scalar、定数、またはcompiler用の長寿命一時値を
配置してよい。

```text
aux(global scalar) | reserved prefix | array elements...
```

次をABIの不変条件とする。

- `aux`は配列要素数に含めない。
- `aux`の値は0、1を含む任意のbyteでよい。
- array accessorは`aux`を条件flagやmarkerとして使用しない。
- array accessorはアクセス前後で`aux`の値を保存する。
- デバッグ情報では、`aux`へ割り当てたglobalの論理名を通常どおり記録する。

この規則により、global配列のchunk headはテープ容量上の無駄にならない。

### Anchor chunk

anchor headはstack flag laneの左端sentinelであり、常に0を保つ。このcellへ値を
割り当ててはならない。

anchor直後の`D`個のdata cellは配列要素ではなく、compiler-ownedな固定ABI領域と
する。dispatcher、global/frame間の値の搬送、array accessorなどの共有scratchへ
使用してよい。

```text
anchor=0 | abi_scratch[D]
```

anchor headだけは、プログラム実行中に一時的にも非ゼロへ変更してはならない。

## Global aligned region

global配列の先頭head位置を`A`、論理添字を`i`とする。先頭から`R`個の論理data slotは
配列要素ではなく、array accessor用の予約prefixである。最初のslotは`A + 1`にある。
要素の物理位置は次である。

```text
slot(i)   = R + i
chunk(i)  = floor(slot(i) / D)
within(i) = slot(i) mod D

address(A, i) = A + chunk(i) * S + 1 + within(i)
```

defaultの`D = 16`、`R = 16`では、最初のchunk全体がportalで、配列要素は次のchunk
から始まる。

```text
A+0  aux
A+1  VALUE_PORT
...
A+16 portal[15]
A+17 aux
A+18 array[0]
...
A+33 array[15]
```

配列長`L`が使用するchunk数は次である。

```text
array_chunks(L) = ceil((R + L) / D)
```

最後のchunkで配列長を超えるdata cellはpaddingである。compilerは、その配列の
accessorがpaddingへ触れないことを保証できる場合、別のstatic scalarをpaddingへ
配置してよい。

実行時添字が`0..L`を外れた場合は、BFC言語仕様どおり未定義動作である。物理的な
paddingの存在によって範囲外アクセスを有効にしてはならない。

## Stack flag lane

anchor headの位置を`H`とする。最初のstack chunk headは`H + S`にある。

stackには常に次の形の連続したflag列が存在する。

```text
H:       0                 anchor
H + S:   1                 allocated chunk 0
H + 2S:  1                 allocated chunk 1
...
F - S:   1                 last allocated chunk
F:       0                 frontier
```

`F`は最初の未使用chunk headであり、抽象的なstack pointerに相当する。数値としての
`F`をcellへ保存する必要はない。

次を不変条件とする。

- anchorからfrontierまでのstack flagはすべて1であり、途中に0を含まない。
- frontier flagは0である。
- frontierより右の未割り当てflagは0である。
- frameの確保・解放を除き、stack flagを変更しない。
- helperがflag laneを走査するとき、すべてのflagを保存する。

## Anchorとfrontier間の移動

### Frontierからanchor

処理開始時のBFデータポインタを`F`とする。

```text
pointer -= S
while current_flag != 0:
    pointer -= S
```

ループ終了時、ポインタはanchor `H`にある。

### Anchorからfrontier

処理開始時のBFデータポインタを`H`とする。

```text
pointer += S
while current_flag != 0:
    pointer += S
```

ループ終了時、ポインタは現在のfrontier `F`にある。

static globalへアクセスするhelperは、原則として`F`で開始し、anchor経由でglobalへ
移動し、同じ`F`へ戻る。

## Function frame

各関数について、compilerは次を含む`FrameDescriptor`を作る。

```text
function entry continuation
frame chunk count K
parameter locations
local scalar locations
dynamic-index local array regions (v0)
dynamic-projection local aggregate regions (v1)
expression temporary locations
common header locations
aggregate return outbox size and locations
```

一つのactivationが使用するchunk数`K`はコンパイル時定数である。再帰の深さだけが
実行時に変化する。`K`にはcommon headerと、その関数がcallerとして使用するaggregate
return outboxも含む。outbox、parameter、local、temporaryの物理領域は重複させない。

現在のframeが`K` chunkを使い、そのfrontierが`F`であるとき、frame bottom headは
次になる。

```text
B = F - K * S
```

frame bottomから数えた論理data cell `q`の位置は次である。

```text
frame_address(F, K, q)
    = F - K * S
      + floor(q / D) * S
      + 1
      + (q mod D)
```

したがって、現在のframeにあるscalar `FrameSlot`は、frontierからの負のコンパイル時定数offsetとして
アクセスできる。aligned aggregate region内の定数projectionも、region baseとlogical offsetから
物理位置をcompile timeに決定できる。

frame内の大分類は低addressから次の順とする。

```text
B | aligned aggregate regions | scalar parameters / locals / temporaries | outbox high chunks ... chunk 1 | chunk 0 | dispatch context | F
```

outboxを持たない関数ではその領域を省略する。scalar領域をcontext寄りへ置くことで、
大きなlocal aggregateがあっても式評価と16-bit offset計算のpointer移動を増やさない。
個々のparameter/local配置は
`FrameDescriptor`で決めるが、dispatch contextとoutbox logical chunk 0のfrontier
相対位置は全関数で共通にする。

## Common frame header

各frameの最上位`P` chunkに、array portalと同じ16 logical cellのdispatch contextを置く。
frame frontierを`F`、context base headを`C`とする。

```text
C = F - P * S

field_address(C, j)
    = C
      + floor(j / D) * S
      + 1
      + (j mod D)

j=0   VALUE
j=1   ACTIVE
j=2   PC_LOW
j=3   PC_HIGH
j=4   NEXT_PC_LOW
j=5   NEXT_PC_HIGH
j=6   CONDITION
j=7   RESTORE
j=8   BRANCH
j=9   INDEX
j=10  RETURN_PC_LOW
j=11  RETURN_PC_HIGH
j=12  SCRATCH_0
j=13  SCRATCH_1
j=14  SCRATCH_2
j=15  SCRATCH_3
```

このlogical field順とarray portalのfield順を共通化する。`D = 8`では途中に2個目の
stack flagまたはglobal aux headを挟むが、`field_address`がそれを飛ばす。

### `ACTIVE`

現在のframeがdispatcherの実行対象である間は1とする。trampolineのBFループは、
反復終端で次に実行するframeの`ACTIVE`を指す。

`main`を終了するときはmain frameの`ACTIVE`を0にし、そのcell上でtrampoline
ループを終了する。

### `PC_LOW`と`PC_HIGH`

continuation IDをlittle endianの16 bit値として保持する。

```text
pc = PC_LOW + 256 * PC_HIGH
```

- continuation ID 0は通常のdispatch対象に使用しない。
- 有効なcontinuation IDは`1..=65535`とする。
- 初期loweringはlow byteとhigh byteを順に照合する16 bit dispatchを使用する。
- backendは意味を保つ範囲でpage/slot countdownへ置換してよい。
- continuation IDの物理的な番号付けはcompiler内部仕様である。

### `NEXT_PC_LOW`と`NEXT_PC_HIGH`

continuation bodyが次に実行するIDを書くstaging registerである。dispatcherは一つの
continuationを実行した後、`NEXT_PC`を`PC`へmoveして次の反復へ入る。現在の`PC`は
continuation選択時に0へclearするため、同じdispatcher反復で次のcontinuationまで
誤って実行しない。

### `RETURN_PC_LOW`と`RETURN_PC_HIGH`

callee frameではcallerのresume continuation、array portalではaccess site固有の
resume continuationを保持する。return helperはこの値を次のcontextの`NEXT_PC`へmove
する。

### `VALUE`

scalar関数の戻り値と、calleeからcallerへscalar値を渡すinboxを兼ねる。

- `cell`関数はreturn前に自身の`VALUE`へ戻り値を置く。
- return処理はcalleeの`VALUE`をcallerの`VALUE`へ移す。
- callerのreturn continuationは自身の`VALUE`から結果を読む。
- `void`関数はcallerの`VALUE`を0にする。
- 配列やstructなど、複数cellのaggregate戻り値には`VALUE`を使用しない。

### ABI work cells

`CONDITION`、`RESTORE`、`BRANCH`、`INDEX`、`SCRATCH_0..3`はdispatcherとABI helperだけが
使用する。continuation bodyへ通常制御を渡す時点ではすべて0でなければならない。
array accessor中の`INDEX`とscratchは例外で、resume continuationが回収時にclearする。

parameter、local、temporaryをこの16 cellsへ割り当ててはならない。

## Frame内の動的添字配列region

定数添字だけを使うlocal配列は、各要素を通常の`FrameSlot`へscalarizeできる。この
場合は配列固有のbase head、head alignment、`R`個のreserved prefixを持たず、各要素を
frontier-relativeなコンパイル時定数offsetで直接読み書きする。

以下のregion layoutは、第7段階で動的添字のarray accessorに渡すlocal配列に対する
規約である。そのようなlocal配列のbase headは必ずchunk headへalignする。base head
直後の`R`個はglobal配列と同じ予約prefixとし、配列の最初の要素はその後へ置く。

```text
base flag=1 | reserved prefix | local_array...
```

要素位置の変換はglobal aligned regionと同じである。

```text
slot(i) = R + i
element_offset(i) = floor(slot(i) / D) * S + 1 + (slot(i) mod D)
```

違いはbase headの求め方だけである。

- global配列のbaseはanchorより左の静的絶対位置である。
- local配列のbaseは現在のfrontierからの負の定数offsetである。

scalar localはchunk headへalignする必要はない。compilerはframe内の任意のdata cellへ
配置してよい。ただし動的添字を持つ配列のbaseだけは必ずhead alignedとする。

配列の末尾paddingへscalar localを配置してよいが、配列accessorは宣言長を越えて
paddingへ触れてはならない。

## Array accessor contract

global配列とlocal配列のaccessorは、同じchunk内添字変換とpointer-relativeなregion
layoutを使用する。version 0の基準案では、callerがデータポインタを対象配列regionへ
合わせて共有accessorへ制御を渡す。

```text
array region prefix:
    0   VALUE_PORT
    1   ACTIVE
    2   PC_LOW
    3   PC_HIGH
    4   NEXT_PC_LOW
    5   NEXT_PC_HIGH
    6   CONDITION
    7   RESTORE
    8   BRANCH
    9   INDEX
    10  RETURN_PC_LOW
    11  RETURN_PC_HIGH
    12  QUOTIENT / SCRATCH_0
    13  REMAINDER / SCRATCH_1
    14  PHASE / SCRATCH_2
    15  ZERO_FLAG / SCRATCH_3
```

frameとarrayで同じfield番号を使用し、共通dispatcherを使う。独立したarray専用
dispatcherは生成しない。

accessorは次を満たさなければならない。

- base headを配列要素として扱わない。
- global `aux`を変更しない。
- local stack flagを変更しない。
- loadは選択要素を保存したまま`VALUE_PORT`へcopyする。
- storeは選択要素だけを上書きする。
- source上のindexを保存する必要がある場合、frontendが一時cellへcopyする。
- helper自身がindex一時値を消費してよい。
- helperはreturn continuation PCをaccessor側の`PC`へ戻す。
- helper終了時のpointerは、array regionに規定されたdispatch位置へ置く。

loadの制御遷移は次とする。

```text
caller continuation at frame:
    array.return_pc = call-site-specific resume continuation
    array.index = runtime index
    array.pc = ARRAY_COPY
    array.active = 1                 // 独立ACTIVEを使うlayoutの場合
    pointer = array dispatch position

ARRAY_COPY continuation:
    (quotient, remainder) = divmod(array.INDEX, D)
    array.VALUE_PORT = copy(array[quotient * D + remainder])
    array.next_pc = array.return_pc
    pointer = same array dispatch position

resume continuation at array:
    result = array.VALUE_PORT
    clear array protocol cells
    if global array:
        move by call-site-known constant distance to anchor
        scan anchor -> current frontier
    if current-frame local array:
        move to array base head
        scan stack flags right -> current frontier
    continue at current frame dispatcher
```

resume continuationはcall siteごとに生成され、対象がglobalかlocalか、globalなら
そのbaseからanchorまでの静的距離を知っている。したがって共有accessorはregionの
種類もcaller frameの位置も知る必要がない。再帰時もlocal配列のprotocol cellは各
activationのframe内に別々に存在する。

global配列のprotocol cellは全activationで共有されるが、問題にはならない。
`ARRAY_COPY`と対応するresumeまでをnon-reentrantなABI操作とし、その途中でuser関数を
callしたり、別のarray accessorへ制御を渡したりしないためである。

この方式では、accessorからfrontierへ戻る処理まで共有する必要はない。accessorは
array側のPCをreturn continuationへ変更するだけであり、そのcontinuationがpointerを
normalizeする。関数本体と`ARRAY_COPY`本体はどちらも1回だけ生成される。

`VALUE_PORT`は配列要素とは別のhidden data cellである。global `aux`はlive valueを
保持でき、localのheadはstack flagなので、どちらもportとして上書きしてはならない。
callerは値を回収した後にportを0へ戻す。store helperのsource portも終了時に0へ戻す。

代替案として、compiler-privateなfat region handleをfrontier-normalized helperへ渡す
方式も実装可能である。しかしpointer-relative portal方式が実測で成立する限り、
version 0ではruntime region handleを導入しない。

version 0ではsource-level pointer/referenceを導入せず、共有array accessorが必要とする
間接操作はこのcompiler-owned protocolへ限定する。

初期loweringは、最大宣言長を持つ配列に合わせたchunk/withinの二段静的dispatchを
`ARRAY_COPY`内に1回だけ生成する。短い配列の有効添字は同じprefixを使って共有できる。
256要素では生成codeが大きいため、将来はmoving-index方式などへ置換してよいが、これは
array portal ABIを変更しないbackend最適化とする。

## Version 1 aggregate region

version 1はversion 0のaligned array regionを、flatten済みaggregate payloadを持つregionへ
一般化する。source型のlogical layoutは次である。

```text
cells(cell)       = 1
cells(enum E)     = 1
cells(struct S)   = declaration orderでのfield cell数の合計
cells(T[N])       = N * cells(T)
```

structはfield宣言順、配列はrow-majorでflattenする。`T[N][M]`は長さ`N`のouter arrayであり、
各要素は長さ`M`のinner arrayになる。enumのnominal identityとstruct field型はfrontendで検査し、
ABIはscalar leaf列と総cell数だけを扱う。

aggregate rootの先頭headを`A`、flatten済みpayload offsetを`o`とする。物理位置はversion 0の
式を長さ256より大きいpayloadへそのまま拡張する。

```text
slot(o)    = R + o
chunk(o)   = floor(slot(o) / D)
within(o)  = slot(o) mod D

address(A, o) = A + chunk(o) * S + 1 + within(o)
```

protocol prefix、chunk head、paddingはpayload cell数に含めず、aggregate copyでも読み書き
しない。zero-size aggregateはregionを確保せず、copyも行わない。現行backendはlocal aggregateを
一律にaligned regionへ置く。将来、動的indexを一度も使わないlocal aggregateをscalarizeしてよいが、
これは解析を伴う未実装のlayout最適化である。regionは1個のrootにつき1個のprotocol prefixを置き、
nested arrayごとにprefixを重ねない。

### Flat logical offset

sourceの各array indexは1 cellのままとする。frontendがindexを左から右に一度ずつ評価した後、
backendは次の式をcompiler temporaryで計算する。

```text
o = constant field/subobject offset
for each dynamic array projection from outer to inner:
    o += index * cells(indexed element type)
```

Continuation IRが表現できるaggregate payloadは最大65,536 cellであり、`o`の値域は
`0..=65,535`なのでlittle-endianの2 cellで十分である。この2 cell値はsourceから構築、保存、比較、
returnできる整数型ではなく、portal呼出しの間だけ存在するcompiler-owned値である。

通常compileではstatic領域と各frameの物理layoutに30,000-cell tapeのcapacity checkを適用する。
現行の`*_unbounded` APIが外すのはstatic layout側のcapacity checkであり、function frameは引き続き
30,000-cell上限を満たさなければならない。

version 1では共通contextのlogical fieldを次のように解釈する。

```text
j=9   OFFSET_LOW       // version 0 INDEXと同じ位置
j=12  OFFSET_HIGH      // version 0 SCRATCH_0と同じ位置
j=13  SCRATCH_1
j=14  SCRATCH_2
j=15  SCRATCH_3
```

offset計算中に追加scratchが必要ならanchor scratchまたはfunction temporaryを使用する。
通常continuationへ制御を戻す前に`OFFSET_LOW`、`OFFSET_HIGH`、scratchを0へ戻す。
version 0の1次元`cell[N]` accessは`OFFSET_HIGH = 0`、`OFFSET_LOW = INDEX`として表せる。

### Aggregate accessor contract

portalのprimitive operationは、region payload内の1 logical cellを16-bit offsetでload/storeする。

```text
load_cell(region, o):
    VALUE_PORT = copy(payload[o])

store_cell(region, o, VALUE_PORT):
    payload[o] = move(VALUE_PORT)
```

複数cell subobjectのload/storeは、call siteがbase offsetを一度計算し、offsetをincrementしながら
logical leaf順にprimitive operationを繰り返す。RHSは最初のstoreより前にcaller-owned temporaryへ
完全にsnapshotする。load/store列の途中ではuser continuation、function call、別regionのportalを
実行しない。このnon-reentrant規則により、sourceからはaggregate accessを一回の不可分な値copy
として観測する。

継続するleaf copyのoffsetはcaller frameの2 cell temporaryへ保持する。各primitive callでその値を
region prefixの`OFFSET_LOW/HIGH`へcopyし、portalがprefix側を消費・clearした後、resume continuationが
caller側offsetをincrementする。portal contextへ次回用offsetをliveなまま残さない。

accessorはさらに次を満たす。

- `0 <= o < payload_cells`のとき対応するpayload cellだけを読み書きする。
- sourceの範囲外indexは未定義動作だが、有効offsetでprotocol、head、paddingへ触れない。
- global `aux`とlocal stack flagを保存する。
- offset値、`VALUE_PORT`、使用したscratchをresume時に0へ戻す。
- global/local regionからcurrent frame frontierへversion 0と同じ規則でnormalizeする。

version 0の全要素を列挙するstatic accessorは256要素を前提にしているため、そのまま大きな
aggregateへ拡張しない。version 1 accessorは16-bit logical offsetを消費するcountdown、moving-index、
または同じcontractを満たす別実装を使用する。これはsource-level pointerを導入しない。

### Arrays of struct and alternative layout

canonical ABI layoutはrow-majorのarray-of-structである。例えば`Node[N]`のlogical offsetは
`index * cells(Node) + field_offset`になる。backendはprogram全体を同時に変換し、全access/copyの
意味を保てる場合に限りstruct-of-arraysなど別の物理layoutを最適化として選んでよい。その選択を
sourceから観測したり、異なるlayoutのaggregateを同じportal contractで混同したりしてはならない。

## Source-level referenceを持たない規則

version 0とversion 1のBFCは、次を持たない。

- address-of演算子`&`
- dereference演算子`*`
- pointer型
- reference型
- aggregateやlocalのaddressを整数へ変換する操作

したがって、local aggregateまたはscalar localのaddressをcalleeへ渡したり、returnしたり、
globalへ保存したりできない。

異なるlocal `a`と`b`を同じ関数へ参照渡しするために、関数本体を複製する必要が必ず
あるわけではない。例えば次のfat handleを実行時に解決すれば、単一の関数本体で
実装できる。

```text
ReferenceHandle {
    address_space,
    frame_depth,
    chunk_offset,
    cell_offset,
}
```

しかし、この方式には次が必要になる。

- 可変長のframe chainを辿るdereference helper
- frame境界またはframe sizeの実行時表現
- referenceのlifetime検査
- aliasing規則
- referenceを別の関数へ転送するときのframe depth更新
- return後のframeを指すdangling referenceの禁止

これらは実装可能だが、初期BFCの目的に対して大きすぎる。関数間のaggregate受け渡しは
参照ではなく値渡しと値返しを標準とする。

参照に似た呼出構文が将来必要になっても、addressを言語値にする必要はない。
non-escapingかつ重複しない`inout`引数だけを認め、次のcopy-in/copy-outへloweringできる。

```text
update(inout a)       // source sugar
a = update(a)         // ABI上の意味
```

複数の`inout`引数はstructまたは複数cell aggregateとしてまとめて返し、call siteが
元の変数へwritebackする。writeback先はcall siteごとに静的に既知なので、callee本体の
複製もruntime referenceも要らない。同じ変数を複数の`inout`引数へ渡すalias、参照の
保存、calleeからの再返却はこの変換では認めない。version 0とセルフホスト拡張version 1は
この糖衣を導入せず、method callも含めて明示的な値返しと代入だけを仕様とする。

## Aggregate value

固定長配列とstructを、複数cellからなるaggregate valueとして扱う。version 0互換の`cell[N]`に加え、
現行version 1実装はenum leaf、struct、任意要素型・多次元配列を扱う。

```text
AggregateType {
    cell_count: compile-time constant
    alignment: chunk head alignment when dynamically indexed
}
```

source-levelな意味はcopy semanticsとする。

- aggregate引数はcalleeが所有するparameter領域へcopyする。
- aggregate戻り値はcallerが所有するresult領域へ値として返す。
- calleeがparameterを変更してもcallerの元の値は変更しない。
- callerのaggregateを書き換えたい関数は、変更後の値をreturnし、callerが代入する。
- compilerは観測可能な意味を変えない範囲でcopy elisionまたはmoveを行ってよい。
- aggregateの`==`、順序比較、暗黙のscalar変換は別途定義しない限り認めない。

想定するsource表現は次のようなものである。

```c
cell[100] transform(cell[100] source) {
    cell[100] result;
    // ...
    return result;
}

buffer = transform(buffer);
```

同じnominal aggregate型同士の全体代入を認め、右辺を完全に評価してからcopyする意味とする。
compilerは同じ意味になるmoveとcopy elisionを行ってよい。文字列は`cell[N]`のaggregate
initializerであり、末尾NULや専用runtime metadataを追加しない。

## Aggregate return outbox

各frameは、その関数が呼び出す関数のaggregate戻り値を受け取るoutboxを持てる。
必要な大きさはcall siteの戻り型からコンパイル時に計算する。

```text
outbox_cells(function)
    = そのfunction内の全call siteにおける最大aggregate return size

outbox_chunks(function)
    = ceil(outbox_cells(function) / D)
```

aggregateを返すcall siteがない関数では、`outbox_cells = 0`としてoutbox chunkを予約
しない。

outboxを使う関数は、dispatch context直下のchunkから必要数を予約する。callerの
frontierを`F_parent`、context baseを`C_parent`、outbox chunk数を`O`とする。

```text
C_parent = F_parent - P * S
O = outbox_chunks(function)

outbox_chunk(i)  = floor(i / D)
outbox_within(i) = i mod D

outbox_address(C_parent, i)
    = C_parent
      - (1 + outbox_chunk(i)) * S
      + 1
      + outbox_within(i)
```

logical chunk 0をdispatch context直前に置き、番号が増えるほど左へ伸ばす。この逆向き
配置により、callerが確保したoutbox全体の大きさ`O`に依存せず、calleeは同じ定数offsetで
`parent.outbox[i]`へ書ける。frame layoutはこの範囲を先に予約し、parameter、local、
temporaryを別領域へ配置する。

callerはcall前に、自身のframeへ必要な最大outboxサイズを確保済みである。callee
frontierを`F`、callee frame sizeを`K` chunkとすると、parent frontierは次である。

```text
F_parent = F - K * S
```

calleeは自身の`K`と共通outbox offsetを知っているため、pointer値を受け取らずに
parent outboxへaggregateを構築できる。

```text
callee aggregate return:
    parent.outbox[0..N] = return value
    deallocate callee frame
    resume caller continuation
```

再帰呼び出しでは各activationが別のoutboxを持つため、global scratchへ戻り値を置く
方式と異なり、深いcallによる上書きが起こらない。

この配置は、2 chunk outboxを持つ再帰probeで、子のaggregateを各activationのoutboxへ
受け取り、加工後に親outboxへ転送する経路まで検証済みである。

### Copy elision

calleeは、return対象の一時aggregateを自身のframeへ作ってからcopyする代わりに、最初から
parent outboxをreturn objectのstorageとして使用してよい。

caller continuationはoutboxから最終destinationへcopyする。ただしdestinationの
lifetimeと次のcallが競合しない場合、compilerはdestinationをoutboxへ割り当てて
このcopyも省略してよい。

一つの式で複数のaggregate戻り値を同時に保持する必要がある場合、最初の結果を通常の
local temporaryへ退避してから次のcallを行うか、複数のoutbox slotをframe layoutへ
確保する。

### Aggregate argument

aggregate引数のdestinationはcallee frame内のparameter regionとして静的に決まる。
callerはcallee frame確保後、caller側の値をcallee parameter regionへcopyする。

引数がcall後に不要であり、frame layout上安全な場合はmoveまたはstorage共有へ
最適化してよい。source-levelには常に値渡しとして見える。

## Frame確保

calleeが使用するchunk数を`K`とする。call処理は現在のfrontierから次を行う。

```text
repeat K times:
    assert current head == 0       // debug loweringのみ
    current head = 1
    pointer += S
```

終了時のポインタ位置がcalleeのfrontierになる。

未使用chunkのdata cellは0であることを不変条件とする。program開始時はBF tapeの初期値、
再利用時はreturn処理によるclearで保証するため、通常のallocationではdataを再clear
しない。

その後、callee frameを次の順に初期化する。

```text
ACTIVE = 1
PC = 0
NEXT_PC = callee entry continuation
RETURN_PC = caller resume continuation
VALUE = 0
ABI work cells = 0
parameters = 左から右に評価済みの引数
locals = 0、または宣言initializerの値
```

通常のrelease buildではテープ右端の実行時検査を行わない。利用可能領域を越えた
frame確保は未定義動作とする。debug loweringはfrontier guardを検査してよい。

## Call convention

callerのcontinuation bodyはcall前に引数を左から右へ評価する。call terminatorは次を
行う。

```text
allocate callee frame
initialize callee parameters
callee.RETURN_PC = return continuation
callee.NEXT_PC = function entry continuation
pointer = callee.ACTIVE
```

caller frameはstack上に残り、scalar、local array、temporaryを個別にpushしない。
calleeがreturnするまでcallerの値は変更されない。

aggregateを返すcallでは、caller frameが十分なoutboxを持つことを
`FrameDescriptor`構築時に保証する。aggregate引数はcallee frame確保後にcalleeの
parameter regionへcopyする。

再帰呼び出しでも同じ関数用の新しいframeを確保するため、static local cellの上書きや
特別な再帰判定は必要ない。

version 0の初期実装はtail call optimizationを行わない。後から、戻り先が同じでframe
layout上安全な場合にcaller frameの解放とcallee frame確保を統合してよい。

## Return convention

calleeのframe chunk数を`K`、callee frontierを`F`とする。caller frontierは次である。

```text
F_parent = F - K * S
```

return処理は次を行う。

```text
scalar returnなら caller.VALUE = callee.VALUE
aggregate returnなら caller.outbox = callee return object
caller.NEXT_PC = callee.RETURN_PC
clear callee frame data
clear callee frame flags
pointer = F_parent
pointer = caller.ACTIVE
```

return先はcalleeの`RETURN_PC`に保持するため、return addressを別のvalue stackへpush
する必要はない。dispatcher epilogueがcallerの`NEXT_PC`を`PC`へmoveする。

calleeは自身のframe size `K`と、全frameで共通なcaller `VALUE`のfrontier相対位置を
知っているため、callerの関数種類を知らなくても戻り値を転送できる。

source-levelのmainにある`return;`とmain末尾への到達は`Terminator::Halt`へloweringする。
IR-levelの`Return`はmainでは不正であり、`Halt`がmain frameの`ACTIVE`を0にしてprogramを終了する。

## Continuation dispatcher

関数本体は、callの前後などで複数のcontinuationへ分割する。各continuationは一度だけ
BFへ生成する。

```text
Continuation:
    body instructions
    terminator = Goto | Branch | Call | Return
               | ArrayLoad | ArrayStore
               | AggregateLoad | AggregateStore
               | Abort(v1) | Halt
```

dispatcherは、現在contextの16 bit `PC`をhigh byteのpageとpage内のlow byteへ分けて対象を選ぶ。
user continuation、aggregate portal accessor、portal resumeを同じpage表へ入れる。連続したhigh-byte
pageと、page内の連続したlow byteには破壊的countdownを使用し、疎な集合だけID equality scanへ
fallbackする。選択時に`PC`を0へclearし、bodyが書いた`NEXT_PC`を反復末尾で`PC`へmoveする。

```text
while current_context.ACTIVE != 0:
    (PC_LOW, PC_HIGH)に一致するcontinuationを選択
    PC = 0
    continuation bodyを実行
    terminatorが次のcontextとNEXT_PCを決定
    next_context.PC = move(next_context.NEXT_PC)
```

BFのtrampoline loopは反復ごとに異なる物理cellの`ACTIVE`を条件としてよい。call後は
callee `ACTIVE`、return後はcaller `ACTIVE`にデータポインタを置いてループ終端へ
到達する。pointer-relative array accessorを呼ぶ場合はarray portalの`ACTIVE`と`PC`へ
dispatch contextを一時的に移し、accessor後のresume continuationがframeの`ACTIVE`と
`PC`へ戻す。frameとarray portalは同じ16-field相対layoutを使う。

countdownは`PC`を破壊的に消費し、選択したcaseだけが共通のbranch flagを消費して実行される。
case bodyがcall、return、portalによって別contextへpointerを移した場合、移動先contextの`PC`と
branch flagは0でなければならない。これにより残りのcountdown caseを誤って実行しない。

現行frontendは密なcontinuation IDを割り当てるが、公開Continuation IRは任意のnonzero `u16` IDを
許す。このためbackendは疎な範囲へ巨大なcountdownを生成しない。profileに基づくhot continuationの
ID配置は将来の最適化であり、source順の再現性とdebuggabilityを含めて評価する。

## Pointer position convention

ABI helperは開始・終了位置を明示する。

### Frontier-normalized helper

ローカルアクセス、global往復、frame確保準備、配列wrapperは原則として次を満たす。

```text
entry: pointer = current frontier F
exit:  pointer = same frontier F
```

### Trampoline boundary

```text
entry of loop iteration: pointer = current dispatch context ACTIVE
dispatcher entry:        pointer = current dispatch context base C
terminator exit:          pointer = next dispatch context ACTIVE
```

通常のfunction continuationはterminatorへ渡す前にfrontierへnormalizeする。array call
terminatorだけはarray portalへcontextを移してよい。array resume continuationは
`VALUE_PORT`を回収した後、globalなら既知のbaseからanchorを経由し、localならflag laneを
右へ走査して、current frame frontierへnormalizeする。

## 初期化と終了

program開始時、BFテープはすべて0であることを前提とする。

BFC sourceのentry pointはparameterなしの`void main()`である。ABI上では、そのactivationを
stackのrootとなるmain frameとして扱う。`main`はcallerを持たず、明示的にcallできない。

source-levelにはstatic global initializerを宣言順に実行し、その完了後に`main` activationを
開始する。backendはglobal initializer中のcall/return/`abort`にもdispatcher contextが必要なため、
将来のmain frame領域をbootstrap contextとして先に予約し、初期化continuationをそこで実行してよい。
このbootstrap phaseはsource-levelのmain activationではなく、main localを読み出せない。初期化が
完了すると同じ予約領域をroot main activationとして引き継ぐ。このoverlayはsourceから観測できない。

物理的な初期化順は次のとおりとする。

1. anchorが0であることを保つ。
2. root frame領域を予約し、bootstrap contextの`ACTIVE = 1`、`PC = global initializer entry`とする。
3. static global initializerを宣言順に実行し、global `aux`へ割り当てた値も初期化する。
4. 初期化命令列を終えた実行位置からsource `main`本体へ進み、同じ領域をmain activationとして扱う。
5. pointerをroot contextの`ACTIVE`へ置いたtrampolineを継続する。

program終了時はmain frameの`ACTIVE`を0にする。テープの他の値をclearする義務はない。

### Version 1 `abort`

正常な`Halt`はmain activationだけが生成する。`abort();`は任意のuser continuationから
`Terminator::Abort`へloweringし、次を行う。

```text
current_context.ACTIVE = 0
pointer = current_context.ACTIVE
```

trampoline loopの継続判定cellは反復ごとのcurrent contextにあるため、calleeの`ACTIVE`をclear
すればancestor frameへreturnせず、その場で外側のBF loopを終了できる。allocated frame、flag、
global、outbox、protocol cellをclearする義務はない。`Abort`はarray accessor内部には生成せず、
portalからuser continuationへ戻った後に実行する。

## 容量と効率

stack領域におけるdata cell比率は次である。

```text
D = 8:   8 / 9  = 88.9%
D = 16: 16 / 17 = 94.1%
```

global aligned regionではheadを`aux`として利用できるため、paddingを除けばheadの
容量損失はない。anchor head 1 cellだけは必ずsentinelとして予約する。

大きい`D`には次の性質がある。

- head overheadが小さい。
- anchor/frontier走査のBFポインタ移動列が長い。
- frameごとの末尾paddingが増えやすい。
- 小さい配列や小さいframeで内部断片化が増える。

小さい`D`では逆のtrade-offになる。最終値は生成BF bytes、実行step数、実効テープ
容量を測定して決定する。

## エラーと未定義動作

少なくとも次をcompiler errorとする。

- static global領域、anchor、最低限のmain frameが30,000 cellsへ収まらない。
- 一つのfunction frameの静的サイズが利用可能テープ領域より大きい。
- continuation数が内部PC表現の上限を越える。
- compilerが有効なframe-relative配置を作れない。
- version 1 aggregateのflatten、field offset、stride、outbox size計算がoverflowする。
- version 1 aggregate payloadまたは必要なportal temporaryを含む静的配置・最低main frameが
  30,000-cell tapeへ収まらない。

少なくとも次を実行時未定義動作とする。

- 再帰またはcall深度によりfrontierがテープ右端を越える。
- 実行時添字による配列範囲外アクセス。
- version 1の多段projectionで、いずれかのindexが対応する次元の範囲を外れる。
- 生成BFまたは外部BFがanchor、stack flag、frame headerを破壊する。

debug loweringはguard flagやframe patternを検査して停止してよい。

## Continuation IR

compilerは、静的絶対位置を表す`CellId`とは別に、frame/static/aggregate storageを表す
Continuation IRを実装している。主要なaddressとvalue operandは次のとおりである。

```rust
enum Address {
    Frame(FrameSlot),
    Global(GlobalId),
    ArrayElement { array: ArrayRegion, index: usize },
    AbiValue,
}

enum ArrayRegion {
    Frame(FrameArrayId),
    Global(GlobalId),
    Outbox,
}

enum ValueOperand {
    Cell(Address),
    Array(ArrayRegion),
}
```

`Frame`は現在のactivationに属するscalar parameter、local、一時値を指す。`Global`は
static scalar、`ArrayElement`は定数添字の要素を指す。`AbiValue`はscalar call結果の
受け渡しに使用するcontext cellである。aggregateは`ArrayRegion`でstorage identityを保つ。
Continuationのbodyは`FrameInstruction`列であり、末尾に必ず1個のterminatorを持つ。

```rust
struct Continuation {
    id: ContinuationId,
    function: FunctionId,
    body: Vec<FrameInstruction>,
    terminator: Terminator,
}

enum Terminator {
    Goto {
        target: ContinuationId,
    },
    Branch {
        condition: Address,
        then_target: ContinuationId,
        else_target: ContinuationId,
    },
    Call {
        callee: FunctionId,
        arguments: Vec<ValueOperand>,
        return_to: ContinuationId,
    },
    Return {
        value: Option<ValueOperand>,
    },
    ArrayLoad {
        array: ArrayRegion,
        index: Address,
        destination: Address,
        return_to: ContinuationId,
    },
    ArrayStore {
        array: ArrayRegion,
        index: Address,
        value: Address,
        return_to: ContinuationId,
    },
    Abort,
    Halt,
}
```

typed HIRからのloweringがframe slotとcontinuation IDを割り当て、ABI backendが
`FunctionDescriptor`に基づくframe layout、call/return、dispatcher、物理BF pointer移動を
生成する。validatorは`void main()`、mainのcall禁止、frame/global/array範囲、callの
arityと型、outbox容量、continuation ownership、return型、portal successorなどを検査する。
配列引数はcalleeのframe arrayへcopyし、aggregate returnはcaller activation固有のoutboxへ
書き戻す。version 1では`ArrayRegion`と`ArrayLoad/Store`を前節の`AggregateRegion`、
`AggregateLoad/Store`へ一般化し、enumは`Cell`、struct/arrayはflattenしたcell数で検証する。
`Abort`は全functionで有効、`Halt`はmainだけで有効とする。

## Version 1適合条件

version 1実装は少なくとも次を`D = 8`と`D = 16`の両方で検証する。

- struct field、array-of-struct、struct内array、多次元arrayの定数projectionが同じlogical
  layoutへ解決される。
- dynamic offsetのlow byteが255から0へwrapするとhigh byteへcarryし、offset 255、256、
  chunk境界、payload最終cellを正しくload/storeする。
- 複数の動的indexを左から右に1回だけ評価し、RHS snapshot後にLHS indexを評価する。
- dynamic aggregate load/storeがprotocol、head、paddingをcopyせず、sourceとdestinationが同じ
  root内で重なる場合もsource-level value semanticsを保つ。
- zero-size aggregateの引数、return、代入がstorageを要求せず正常に完了する。
- embedded NULを含む文字列aggregateが末尾追加なしで宣言byte数どおり初期化される。
- 直接・相互再帰中の深いactivationから`Abort`するとcallerへ戻らずtrampolineを終了する。
- version 0の`cell[N]` programがversion 1 backendでも同じbinary outputを生成する。

## 実験結果とversion 0の決定

`bf-frame-experiment`で8/16-cellの両方について次を実行した。

- global/local array portalから単一の`ARRAY_COPY` continuationを呼び、全有効添字をload。
- nonzero high byteを含む16 bit continuation IDのdispatch。
- scalarを返す直接再帰を深さ0から20まで実行。
- frame sizeが異なる2関数の相互再帰を深さ0から20まで実行。
- activationごとに2 chunk outboxを持つaggregate再帰を深さ0から10まで実行。
- return時にcallee全体をclearし、次のallocationではflagだけを設定してframeを再利用。

array portal probeの測定値は次のとおりである。各programは同じ添字でlocal/globalを1回
ずつ読み、stepは全有効添字の平均と最大である。

| D | 配列長 | BF source bytes | 平均steps | 最大steps |
| ---: | ---: | ---: | ---: | ---: |
| 8 | 16 | 14,626 | 60,174 | 70,221 |
| 16 | 16 | 13,807 | 73,460 | 86,625 |
| 8 | 100 | 95,980 | 181,263 | 306,576 |
| 16 | 100 | 90,521 | 193,085 | 316,880 |
| 8 | 256 | 364,234 | 416,329 | 763,194 |
| 16 | 256 | 342,698 | 410,527 | 756,978 |

`D = 8`は中規模配列のstep数で少し有利だが、`D = 16`は同一長でsourceが小さく、
256要素ではstep数も逆転し、flag overheadも半減する。このためdefaultを16とする。

scalar再帰深さ20では、8-cell版が7,196 bytes / 506,300 steps、16-cell版が6,756 bytes /
494,903 stepsだった。aggregate probeは返すcell数が`D + 3`で異なるため直接比較には
使わず、outbox配置と再帰安全性の検証だけに使う。

256要素accessorの約343KBは小さくないが、全access siteへ複製せずprogram全体で1本
だけ生成する初期実装として許容する。array portal ABIを保ったまま、accessor内部を
moving-index方式や別のdispatchへ後から交換する。

## 継続して評価する最適化

次はABIの意味を変えずに比較できる。実装・profiling後に再評価する。
外部記事から収集した具体的なBF lowering候補、適用条件、benchmark順序は
[BF_OPTIMIZATION_NOTES.md](BF_OPTIMIZATION_NOTES.md)に分離して記録する。

- 照合型16 bit dispatcherのpage/slot countdownへの置換。（実装済み。疎なIDは照合型へfallback）
- 二段静的array dispatchをmoving-index方式へ置換するか。
- call graphと頻度推定を使うcontinuation ID割当。
- aggregate copy elisionとoutboxを複数slotへ拡張する条件。
- tail call optimization。
- debug専用のstack overflow guard。

これらの未実装候補を除くversion 0の項目とversion 1の節は、現在の実装基準である。
