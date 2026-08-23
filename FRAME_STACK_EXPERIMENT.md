# Chunked frame stack experiment

`bf-frame-experiment`は、Brainfuckのデータポインタそのものを数値的なstack
pointerとして保存せず、chunkの使用flagを走査して現在のfrontierを見つける
関数フレーム案を検証するための独立crateである。

## レイアウト

1 chunkは1個のflagと、`CHUNK_CELLS`個のデータセルからなる。

```text
scalar globals | global array chunks... | anchor=0 data... |
                 flag data... | flag data... | frontier=0 data...
                    使用中         使用中          最初の未使用
```

すべてのflagは`CHUNK_CELLS + 1`セル間隔で同じ位置に揃う。現在の関数を実行する
間、BFデータポインタの基準位置は最初の未使用flagであるfrontierとする。

- frame確保は、必要chunk数だけfrontier flagを1にして右へ進む。
- frame解放は、既知のchunk数だけ左へ戻り、flagとデータをclearする。
- ローカル変数はfrontierからの負の定数offsetで参照する。
- globalへ移動するときはflag laneを左へ走査してanchorへ戻る。
- globalから戻るときはflag laneを右へ走査して最初の0を探す。

global配列もstack frameも同じ`head + data cells`配置を使う。global側のheadは
stack allocation flagではなく、任意値を保持できるcompiler-owned `aux` cellとする。
共通accessorはこのcellを変更せず、配列要素にも数えない。配列要素の論理添字を
物理offsetへ変換する`cell_offset_from_head`は両領域で共通であり、違うのは基準head
が静的なglobal位置か、現在のfrontierからのframe相対位置かだけである。

実験では`CHUNK_CELLS`をconst genericとし、8と16の両方を同じコードで検査する。

## Probes

### Legacy frame probe

`build_probe`は、3 chunkのroot frameと2 chunkのchild frame、旧形式のlocal/global配列、
anchor往復、frame解放を検査する。これはchunk flag laneそのものの回帰testとして残す。

### Array portal probe

`build_portal_probe_for_length`は16 logical cellのportalと16 bit dispatcherを使う。
同じ`ARRAY_COPY` continuationへlocal配列とglobal配列から入り、それぞれcall-site固有の
resume continuationへ戻る。global auxとlocal stack flagを変更しないことも出力で検査
する。別probeでは共有`ARRAY_STORE`で全添字を上書きして再loadする。continuation IDの
high byteが非zeroの経路も含む。

accessorは入力indexを`quotient = index / CHUNK_CELLS`と`remainder`へ分け、chunkと
chunk内要素の二段静的dispatchを行う。これは初期実装候補であり、portal ABIを保った
まま別アルゴリズムへ交換できる。

### Recursive scalar call probe

`build_call_probe`は、既知chunk数のcallee frameを一括確保し、引数をcalleeへcopyし、
直接再帰した後に共通return continuationでscalarをcallerへ戻す。return時にcalleeの
dataとflagをclearし、次のallocationではflagだけを設定する。

`build_mutual_call_probe`はframe sizeが異なるA/Bを交互に再帰させ、各関数固有の`K`で
caller contextを復元できることを検査する。

### Recursive aggregate probe

`build_aggregate_probe`では各activationが2 chunkのoutboxを持つ。子のaggregate returnを
現在activationのoutboxへ受け取り、全要素を更新して親outboxへ転送する。global scratchを
戻り値領域として使わずに再帰できることを検査する。

## 実行方法

```console
# legacy frame probeのBFを出力
cargo run -p bf-frame-experiment -- frame 8
cargo run -p bf-frame-experiment -- frame 16

# array portal probeのBFを出力
cargo run -p bf-frame-experiment -- portal 16

# portalの全添字を実行して測定
cargo run -p bf-frame-experiment -- matrix

# scalar/aggregate再帰を測定
cargo run -p bf-frame-experiment -- abi
```

このcrateはABI候補の検証用であり、現在のCell IRやBFC frontendへはまだ接続していない。

## 結果

- legacy probeは8/16-cellの両方で全添字、anchor往復、nested frame解放を通過した。
- portal probeは長さ16、32について両構成の全添字を通過した。
- store/reload probeも長さ16、32について両構成の全添字を通過した。
- scalar直接再帰は深さ0..20を両構成で通過した。
- 異なるframe sizeの相互再帰も深さ0..20を両構成で通過した。
- aggregate再帰は深さ0..10を両構成で通過した。
- 16-cell構成は同一配列長で生成sourceが約4.5%から6%小さい。
- 256要素では16-cell構成が342,698 bytes、平均410,527 stepsだった。
- 8-cell構成は364,234 bytes、平均416,329 stepsだった。

この結果からversion 0のdefaultを16 data cells/chunk、portalを16 logical cellsとする。
8-cell構成はlayoutの互換性確認用としてtestを維持する。
