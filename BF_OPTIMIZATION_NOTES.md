# Brainfuck backend最適化メモ

この文書は、version 0の言語仕様やABIを変更しない、将来のbackend最適化候補を記録する。
ここに載せた手法は未採用であり、実装時には生成BFの長さ、実行step、追加cell数を現在の
loweringと比較してから選ぶ。

主な調査元は、angel_p_57氏の
[Brainf**k記事一覧](https://zenn.dev/angel_p_57/articles/40838978dcaf7b)である。記事中の
記号付きBFは説明用のコメントを含むため、そのままcompilerへ埋め込まず、entry/exit条件を
持つlowering templateとして実装する。

## 最優先で試す候補

### 非破壊的な非同期分岐

[非破壊的条件分岐](https://zenn.dev/angel_p_57/articles/afcae170f7311a)では、次の2方式が
整理されている。

- 条件値を評価用cellと保存用cellへ複製し、評価後に保存値を戻す。
- `[`に入ったcellとは別のcellで`]`を評価する非同期的な分岐と、必ず存在するelse側の
  経路を組み合わせ、条件値を消費せずに最終pointer位置を揃える。

後者は条件値のbackup cellの代わりに、経路を合流させるためのzero cellを要求する。
現在のcontinuation dispatcherとarray accessorもpointer位置の移動を積極的に利用するため、
次の箇所で候補になる。

- `Branch`と短絡評価のlowering。
- dispatcherのcase判定と、array portalからresume continuationへ戻る判定。
- 条件値が後続でもliveな場合の、copy/restoreの省略。

ただし、これはCell IRの`Branch`の意味を変える機能ではない。BF lowering内で両経路の
終了pointer位置を静的に証明できる場合だけ選択する。通常のbackup方式もfallbackとして残す。

### 破壊的な大小比較と差分の同時計算

[効率のよいアルゴリズム](https://zenn.dev/angel_p_57/articles/2d6f2f36eb235a)の大小比較は、
2値を同時に1ずつ減らし、一方が先に0になった時の終了cellの違いで大小を表す。同時に
絶対差に相当する値が残るため、比較後に差、`min`、`max`を使う場合は処理を融合できる。

source-levelの比較演算へは次の規則で使える。

- 両operandが比較後にdeadなら、operandを直接消費する最短形を選べる。
- operandがliveなら、一時cellへcopyした値を消費する。元値を壊してはならない。
- 比較と差、`min`、`max`の複数結果が直後に必要なら、保存cellを追加するvariantと後続処理を
  融合し、別々に再計算しない。
- 終了pointer位置自体が比較結果を表すので、直ちにbranchへつなぐtemplateとして扱う。
  booleanの0/1へ一度materializeする形と、branchへ直接loweringする形を別々に測る。

この方式はunsigned 8-bit値の大小比較に適する。wrapping subtractionの符号だけを見て
比較する方式ではないため、`0`, `255`, equalを含む全`256 * 256`組で検証する。

### 固定除数用divmod

[divmodアルゴリズムと数値出力](https://zenn.dev/angel_p_57/articles/d5bd4cf2d32168)は、被除数を
1ずつ消費しつつ、除数counterと余りを周期的に回して商と余りを同時に得る実装を示している。
さらに[効率のよいアルゴリズム](https://zenn.dev/angel_p_57/articles/2d6f2f36eb235a)には、除数を
codeへ埋め込み、除数cellを省く固定除数版がある。

array accessorの`index / D`と`index % D`では`D`がcompile-time定数であり、defaultは16、
互換構成は8である。そのため固定除数版は特に相性がよい。記事の固定除数版は通常の商・余りと
値の向きや更新時点が異なるので、canonicalな`(q, r)`へ補正してからdispatchする案だけでなく、
その表現のままpage/slot countdownへ融合する案も比較する。

除数1への対応や任意除数版は、一般の`/`、`%`演算を実装するときの候補として分離する。
array用templateは`D = 8`と`D = 16`の全入力についてのみ保証すればよい。

### スライド型loopとmoving-index accessor

[スライド型ループ](https://zenn.dev/angel_p_57/articles/d0c8c51b244cc8)は、対になる`[`と`]`が
同じ物理cellを使う必要はないことを利用し、loopの継続判定位置を反復ごとに移動する。
記事ではsentinelまでの走査、marker jump、指定距離の移動を例示している。

これは次の最適化候補になる。

- 二段静的array dispatchを、indexを運びながらchunkまたはelement間を移動する
  moving-index accessorへ置換する。
- stack flag列、global anchor、array境界のsentinel scanを短縮する。
- dispatcherの現在位置とcounterを一緒に隣のslotへ送る。

任意の0値要素をsentinelと誤認しないよう、走査対象はABIがmarker/flagと定めたcellに限る。
配列payloadの内容には依存させない。

### 多段分岐とdispatcher

[条件分岐の多段化](https://zenn.dev/angel_p_57/articles/b409d3e760d99b)は、判定値を1ずつ減らし、
共通のelse用flagを使い回してswitch相当を構成する。continuation IDやarrayのpage/slotは
密な非負整数なので、次を比較する。

- 現在のcontinuationごとの照合。
- IDを減らしながら進む1段countdown。
- high byte/pageを選んでからlow byte/slotを選ぶ2段countdown。
- 頻度の高いcontinuationへ小さいIDを割り当てる配置。

一般のswitch loweringにも使えるが、疎な値では減算回数が増える。密度とprofileを見て
照合型、countdown型を選択する。

## 次段階の候補

### 二進多cell値

同じ[効率のよいアルゴリズム](https://zenn.dev/angel_p_57/articles/2d6f2f36eb235a)には、carry/borrowを
「最初に見つかる0または1までのslide」として処理する二進increment/decrementがある。
16-bitを固定したPCには現在の2-cell表現で十分だが、次の場合に再検討する。

- 256を超える配列indexや、より広いstack pointerをruntime値にする。
- 任意長整数を言語へ追加する。
- increment/decrement主体のcounterで、基数256のcarry処理より二進cell列が有利になる。

[BCD演算の記事](https://zenn.dev/angel_p_57/articles/0dabf335693675)は、任意長decimal値や
decimal I/Oを言語機能へ追加した場合の候補とする。version 0の8-bit `cell`には導入しない。

### 入出力と定数生成の融合

[AtCoderでの頻出処理](https://zenn.dev/angel_p_57/articles/7c8c9b9127fb29)には、複数の出力候補を
互い違いに配置して分岐結果のpointer位置から直接走査する方法、複数定数の共通部分をloopで
一括生成する方法、delimiter判定と十進入力の蓄積を融合する方法がある。

これらは標準libraryまたはpeephole optimizationの候補にする。特に定数文字列出力は、文字ごとに
独立生成する方式、共通baseから差分生成する方式、分岐候補をinterleaveする方式をcode sizeと
step数の両方で比較する。

### BF上のruntime VMを作る場合

[セルフインタプリタ](https://zenn.dev/angel_p_57/articles/ba6f70caa7ce41)では、命令列を0から7の
数値として1cellおきに置き、IPを命令領域とdata領域の間で運び、括弧探索用の可変長counterを
静的領域と反対方向へ伸ばしている。

現在のABIは全continuationをcompile-timeに生成するので、この構成を直接採用しない。将来、生成BFの
code sizeを抑えるためにbytecode interpreter型backendを追加する場合だけ、次を参考にする。

- code/dataを伸長方向の異なる領域へ置くlayout。
- IP位置を継続flagとして兼用する走査。
- 命令cellの間のzero cellをscratchとして再利用する表現。
- 対応する括弧を事前対応表なしで探す場合の可変長nest counter。

## cost model上の注意

[AtCoderでの頻出処理](https://zenn.dev/angel_p_57/articles/7c8c9b9127fb29)と
[処理限界の記事](https://zenn.dev/angel_p_57/articles/d7e2f3529c801d)では、対象処理系が連続する
同種の`+ - < >`をまとめて1 stepとして数えることを利用し、code sizeを増やして長距離移動の
step数を減らしている。この性質はBrainfuck全体の保証ではなく、target依存である。

最適化の評価値は混同せず、少なくとも次を別々に記録する。

- 生成BFのsource bytes。
- repositoryのinterpreterによる実行命令数。
- 連続する同種命令を1命令とするRLE型targetの推定step数。
- 使用した最大tape位置と追加scratch cell数。
- entry/exitのpointer位置と、終了時にzeroであることを要求するcell集合。

code sizeとstep数のPareto frontierを残し、特定処理系にだけ速いvariantはtarget optionで選ぶ。

## 実装・benchmark順序

1. 比較templateを独立probeにし、全8-bit入力で結果、元値の保存、終了pointer位置を検証する。
2. `Branch`のbackup版と非同期版を、条件0/nonzeroおよびbodyの距離を変えて比較する。
3. 固定除数divmodをarray probeへ追加し、`D = 8/16`、配列長16/100/256で測定する。
4. moving-index accessorとpage/slot countdown dispatcherを個別に導入し、組合せ爆発を避ける。
5. 実programのprofileが得られてからcontinuation ID配置とtarget別cost modelを追加する。

最適化templateは、入力cellのlive/dead、必要な初期zero cell、破壊されるcell、各経路の終了pointer
位置を型またはmetadataとして宣言する。文字列断片だけを登録する方式にはしない。
