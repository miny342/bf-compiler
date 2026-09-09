# Selfhost 最適化実験の評価

## 検証済みの採否

| 案 | 判断 | 根拠と制約 |
|---|---|---|
| 空Goto threading・到達不能除去・ID compaction | 採用 | source/CIRで出力一致、helloのprocess wall短縮。主効果はdispatcher往復削減。full selfhost改善は未確認。 |
| continuation除去のみでIDを詰めない案 | 不採用 | 疎なpageがequality scanを選び、生成BFが増大した。密なID配置を維持する。 |
| arena_advanceの桁上がり付き加算 | 不採用・試作撤回 | 境界とselfhost検証は通過したが、helloのraw/RLE換算命令とprocess wallが悪化。native operation削減だけでは採用しない。 |
| 同一関数の非空Branch後継inline化（2c） | 再評価後に採用 | 当初の少数wall測定による不採用をpaired比較で見直した。source/CIR複数ケースでBF execute短縮を確認。生成BF増加を伴う。全入力で差を識別できたわけではなく、end-to-end/full selfhost改善は未確認。 |
| IR継続・遷移・終端・phase・portal集計 | 採用 | 出力・通常counter一致とaccounting、source/CIR測定を確認。計測費用があるためオプトイン。BF hidden dispatch/navigation費用は測れていない。 |
| phase/portal計測のレビュー修正 | 採用 | 設定入力上書きを拒否し、portalなしの別phaseを挟む隣接も切断。実入力・options identityを照合し、未知CIRへ固定ID mappingを流用しない。 |
| 高い再訪率を根拠にportal batchを優先する判断 | 撤回 | 開始chunk再訪はphase全体の履歴指標。直前要求の近さやcall/I/O/aliasを跨ぐbatch安全性を示さない。候補を調べる根拠までに限定する。 |

sourceのphase別IR集計では`arena_advance`が熱い関数として確認され、局所構造化の候補に残る。
CIRの関数名は推測しない。frame/global regionと再帰activationを区別し、
同phaseラベルの再帰はphase境界として扱わない。

## 未検証の案

局所if/while構造化、頻度に基づくPC配置、portal連続処理・専用lane、PC多分割は未採用。
BF hidden dispatch/navigation計測とfull selfhost時間比較も未完。
優先順位と必要な検証は`SELFHOST_OPTIMIZATION_EXPERIMENTS.md`を参照する。

## このコミットの作業結果

記録方針を変更し、両文書から過去の測定表・ログパス・artifact hashと完了済みの
セッション指示を除去した。採用・不採用・撤回の理由、評価上の制約、後続実験と
検証条件を残した。過去の実測詳細はGit履歴に保存されている。

コード・生成物の変更はないため性能再測定とテスト再実行は行っていない。
既存実装と旧評価を照合して採否を整理し、`git diff --check`で文書差分を検証した。
次の実装コミットではこの節をその作業の条件・結果・ログ・必要なhashへ置き換え、
今回を含む過去の作業結果を追記しない。

再現用の既存scriptは`scripts/selfhost-2c/`にある。新しい`./tmp` run rootを使い、
artifact生成、source/CIR比較、IR計測、phase/portal集計、各overhead測定を必要に応じて行う。
