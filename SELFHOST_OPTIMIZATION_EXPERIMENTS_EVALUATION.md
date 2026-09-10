# Selfhost 最適化実験の評価

## 検証済みの採否

| 案 | 判断 | 根拠と制約 |
|---|---|---|
| 空Goto threading・到達不能除去・ID compaction | 採用 | source/CIRで出力一致、helloのprocess wall短縮。主効果はdispatcher往復削減。full selfhost改善は未確認。 |
| continuation除去のみでIDを詰めない案 | 不採用 | 疎なpageがequality scanを選び、生成BFが増大した。密なID配置を維持する。 |
| arena_advanceの桁上がり付き加算 | 不採用・試作撤回 | 境界とselfhost検証は通過したが、helloのraw/RLE換算命令とprocess wallが悪化。native operation削減だけでは採用しない。 |
| 同一関数の非空Branch後継inline化（2c） | 再評価後に採用 | 当初の少数wall測定による不採用をpaired比較で見直した。source/CIR複数ケースでBF execute短縮を確認。生成BF増加を伴う。全入力で差を識別できたわけではなく、end-to-end/full selfhost改善は未確認。 |
| 局所CFG構造化（2d） | 採用 | source/CIRの複数入力でBF execute短縮と出力一致を確認。call・portal等の境界を維持し、通常loweringで有効化。frame guard追加とID配置変化を伴うため、削減全体をloop化だけの効果とは解釈しない。full selfhost時間の改善は未確認。 |
| IR継続・遷移・終端・phase・portal集計 | 採用 | 出力・通常counter一致とaccounting、source/CIR測定を確認。計測費用があるためオプトイン。BF hidden dispatch/navigation費用は測れていない。 |
| BFCRLE v1テキスト圧縮 | 採用（オプトイン） | Rust版出力のSSD書込量を削減。通常BF互換を維持し、profileの展開後ordinal/identityとinline markerを保つ。セルフホスト版にもcompressedエントリを追加し、通常mainとbinary CIRを維持。 |
| interpreter RemoteTransfer | 採用 | BFの意味から経路不変性を実行時検証してScan往復を一括転送へ置換。source由来BFの複数入力で出力・論理counter一致とexecute短縮を確認。不成立時は通常実行。CIR/full selfhost時間比較は未実施。 |
| HIRの局所Frame lowering | 採用 | 単純なローカルwhileの非ゼロ条件・Output・定数更新を直接Frame命令にし、割当て前に不要な一時セルと条件の0/1化を除く。emit_repeat_256の反復は通常BFの最小形となり出力ベンチを短縮。複雑な制御は従来経路、多箇所inlineは未拡張。 |
| phase/portal計測のレビュー修正 | 採用 | 設定入力上書きを拒否し、portalなしの別phaseを挟む隣接も切断。実入力・options identityを照合し、未知CIRへ固定ID mappingを流用しない。 |
| 高い再訪率を根拠にportal batchを優先する判断 | 撤回 | 開始chunk再訪はphase全体の履歴指標。直前要求の近さやcall/I/O/aliasを跨ぐbatch安全性を示さない。候補を調べる根拠までに限定する。 |

sourceのphase別IR集計では`arena_advance`が熱い関数として確認され、局所構造化の候補に残る。
CIRの関数名は推測しない。frame/global regionと再帰activationを区別し、
同phaseラベルの再帰はphase境界として扱わない。

## 未検証の案

頻度に基づくPC配置、portal連続処理・専用lane、PC多分割は未採用。
局所構造化は単一入口の直列・分岐合流・then側back-edgeを扱う。
else側back-edge、複雑な非構造CFG、call/portal/abortを含む外側while全体の構造化は未対応。
BF hidden dispatch/navigation計測とfull selfhost時間比較も未完。
後続実験は`SELFHOST_OPTIMIZATION_EXPERIMENTS.md`を参照する。

## このコミットの作業結果

### セルフホスト版の可逆な短縮出力

`concat-stage2-compiler.sh compressed`を追加。先頭に`@BFCRLE1;`を出し、
反復出力と局所pointer移動を回数付きで直接生成する。
通常BFの`main`とbinary `cir`は維持。モードはentrypointの定数で選び、
圧縮前の巨大な出力bufferは作らない。

24-bit反復は固定8桁の十進数で直接出す。先頭ゼロはBFCRLE仕様で許される。
隣接runの最大合併はしない。
各runの総数から通常BFのサイズ・命令列を復元できる。
これは保存形式の変更であり、生成BFの論理命令数削減ではない。
interpreter、profile形式、inline marker形式は変更していない。
sourceが変わるため、コンパイラ実行用BFとmap・phase設定は再生成する。

### 検証

- `scripts/verify-selfhost-compressed.py`成功。
  短縮版コンパイラをBFとして実行し、全exampleについて通常版のIR実行が
  出したBFと展開後の命令列を比較。通常版のデータはディスクに保存しない。
- 全cell反復数、十進境界、256、65536、最大24-bit反復数を検証。
- 境界テストの生成コードをBFとしても実行し、IR実行と一致。
- `cargo test --workspace`成功（283件）。
- `scripts/verify-stage2-selfhost.sh`成功。通常出力と既存stage-12動作を維持。
- binary CIR経路もhelloを生成・Rust backendでBFへ変換・実行し、`A!`改行を確認。
- ログ：`tmp/selfhost-rle-verification-complete.log`、
  `tmp/selfhost-rle-plain-verification.log`。
- workspaceログ：`tmp/selfhost-rle-workspace-tests.log`。
- `logs/full-selfhost-20260910-190148/stage2-compiler.bfc`を入力にした
  IR実行は途中で区切った。`tmp/selfhost-rle-partial-output.bf`は未完の生成物であり、
  実行用artifactではない。fullサイズ・速度の測定結果としては扱わない。
- full selfhostのBF実行完走時間は未測定。

次のコミットではこの作業結果節を置き換え、採否・理由・制約を保持する。
