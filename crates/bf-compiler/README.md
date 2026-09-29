# bf-compiler のコード構成

BFCソースと外部バイナリCIRを受け取り、Brainfuckへコンパイルする。
開発用に、Continuation IRを直接実行するVMも含む。

## 探す場所

| ディレクトリ | 責務・入口 |
|---|---|
| [src/frontend/](src/frontend/mod.rs) | BFCソースの字句解析、構文解析、マクロ展開、型検査。`lower_source` / `lower_sources` が入口。 |
| [src/frontend/lowering/](src/frontend/lowering/mod.rs) | 型付きHIRから仮想スロットを持つContinuation IRへの変換。 |
| [src/cir/](src/cir/mod.rs) | Continuation IRの定義・検証、共通解析、インライン化、制御フロー整理、記憶領域の割り当て。 |
| [src/cir/input/](src/cir/input/mod.rs) | 外部バイナリCIRの形式と検証、内部Continuation IRへの変換。 |
| [src/cir/vm.rs](src/cir/vm.rs) | IRの直接実行、実行統計。CLIの `--run-ir` に対応。 |
| [src/backend/](src/backend/mod.rs) | フレームとグローバル領域の物理配置、BF出力計画。 |
| [src/backend/codegen/](src/backend/codegen/mod.rs) | dispatch、call/return、動的配列アクセス、値搬送などのBF生成。 |
| [src/bf/](src/bf/mod.rs) | BF命令、出自情報、局所最適化、通常・圧縮形式の出力。 |
| [src/cell/](src/cell/mod.rs) | 静的セル番号を使う独立した低水準API。`compile(&Program)` / `lower(&Program)` が入口。 |
| [src/cli/](src/cli/mod.rs) | `bfc` の引数処理、入力読み込み、コンパイル・実行、計測ファイルの入出力。 |

[src/lib.rs](src/lib.rs) が公開APIを再公開し、[src/main.rs](src/main.rs) がCLIを起動する。
モジュールの階層は内部構成であり、利用側は従来どおり `bf_compiler::lower_source` などを使う。

BF生成は `backend/codegen/mod.rs` を入口に、`planning.rs`（配置とportal計画）、
`dispatch.rs`（実行ブロック選択）、`instructions.rs`（命令と局所制御）、
`control.rs`（call/return）、`portal.rs`（動的配列アクセス）、
`transport.rs`（アドレスと値搬送）、`provenance.rs`（出自情報）に分かれる。
各モジュールは同じemitterの状態を使い、pointer位置とcontextの規約を共有する。

## 処理の流れ

通常のソース入力は次の順で処理する。

```text
BFCソース
  → lexer → parser / AST → macro_expansion → semantic / HIR
  → frontend::lowering（到達可能な関数を仮想スロットへ変換）
  → cir::inline（既定で有効。仮想領域の整理も行う）
  → cir::pipeline（CFG整理 → 演算融合 → スロット割り当て → 保守的なCFG整理）
  → backend（物理配置・出力計画 → BF生成）
  → bf::optimizer → Brainfuck
```

割り当て前後は同じ `ContinuationProgram` 型を使うが、スロット再利用後に適用できる変換は異なる。
パスの順序は [cir/pipeline.rs](src/cir/pipeline.rs) と
[frontend/lowering/mod.rs](src/frontend/lowering/mod.rs) にある。

`--cir-input` は [cir/input/format.rs](src/cir/input/format.rs) で外部形式を読み、
[cir/input/lowering.rs](src/cir/input/lowering.rs) で内部IRへ変換する。
外部CIRはselfhost compilerも出力するが、入力元をselfhostに限定しない。
既存の `SelfhostCir*` / `lower_selfhost_cir*` という公開API名とバイナリ形式は維持している。
この入力では既に共有されているフラットな記憶領域の関係を保存するため、ソース入力と同じ
インライン化・スロット再割り当ての経路には通さない。

どちらの入力も、`--run-ir` ではBF生成の代わりに [cir/vm.rs](src/cir/vm.rs) で実行する。
`cell/` の低水準APIは `Cell IR → cell::codegen → BF IR` という独立した経路である。

## 名前の近い処理

- `cir/structure.rs`: Continuation IRを書き換え、局所的な分岐・ループを構造化する。
- `backend/regions.rs`: IRの意味を保ち、複数ブロックをまとめてBF出力する計画を作る。
- `cir/arithmetic_fusion.rs`: 比較・減算などの演算を融合する。
- `cir/frame_allocation.rs`: 生存期間に応じてスロットやaggregate領域を再利用する。
- `backend/frame_layout.rs` / `static_layout.rs`: フレーム／グローバル領域の物理配置を計算する。
- `backend/codegen/static_frames.rs`: 非再帰関数のフレームを固定配置する実験的バックエンド方式。

## テストと設計資料

各モジュール内のユニットテストに加え、[tests/](tests/) に公開API、言語機能、CLI、最適化の回帰テストがある。
`backend/codegen/*_probe.rs` はテスト時のみ組み込まれ、差分実行や測定を行う。
`cell/continuation_adapter.rs` もテスト専用である。[examples/](examples/) には比較・計測用ドライバがある。

リポジトリルートから通常の検証を実行する。

```sh
TMPDIR="$PWD/tmp" cargo test -p bf-compiler
cargo fmt -p bf-compiler -- --check
```

明示実行が必要なselfhost全体のテストや測定は、通常のテストではignoredとなる。
詳細は [IR設計](../../IR.md)、[Rust ABI](../../ABI-rust.md)、
[インライン化](../../CIR_INLINE.md)、[領域出力](../../CIR_REGION_EMISSION.md)、
[スロット割り当て](../../FRAME_ALLOCATION.md) を参照。
