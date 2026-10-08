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
| [src/cir/vm/](src/cir/vm/mod.rs) | IRの直接実行。実行統計・phase集計は `metrics.rs`。CLIの `--run-ir` に対応。 |
| [src/backend/](src/backend/mod.rs) | フレームとグローバル領域の物理配置、BF出力計画。 |
| [src/backend/codegen/](src/backend/codegen/mod.rs) | dispatch、call/return、動的配列アクセス、値搬送などのBF生成。 |
| [src/bf/](src/bf/mod.rs) | BF命令、出自情報、局所最適化、通常・圧縮形式の出力。 |
| [src/cell/](src/cell/mod.rs) | 静的セル番号を使う独立した低水準API。`compile(&Program)` / `lower(&Program)` が入口。 |
| [src/cli/](src/cli/mod.rs) | `bfc` の引数処理、入力読み込み、コンパイル・実行、計測ファイルの入出力。 |

[src/lib.rs](src/lib.rs) が公開APIを再公開し、[src/main.rs](src/main.rs) がCLIを起動する。
モジュールの階層は内部構成であり、利用側は従来どおり `bf_compiler::lower_source` などを使う。

BF生成は `backend/codegen/mod.rs` を入口に、`portal_plan.rs`（portal計画）、
`dispatch.rs`（実行ブロック選択）、`instructions.rs`（命令と局所制御）、
`control.rs`（call/return）、`portal.rs`（動的配列アクセス）、
`transport.rs`（アドレスと値搬送）、`provenance.rs`（出自情報）に分かれる。
各モジュールは同じemitterの状態を使い、pointer位置とcontextの規約を共有する。

CLIは `cli/mod.rs` が処理順を組み立て、`options.rs`（引数と検証）、
`input.rs`（ソース／CIR読み込み）、`output.rs`（BF・CIR出力）、
`execution.rs`（IR実行と進捗）、`metrics.rs`（計測設定とレポート）、
`identity.rs`（成果物ID）を呼ぶ。入力後の実行・出力処理は両入力経路で共通である。

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
ソース入力全体の処理順は [frontend/pipeline.rs](src/frontend/pipeline.rs) に集約している。
[cir/pipeline.rs](src/cir/pipeline.rs) の `optimize_and_allocate` は、仮想CIRの最適化から
スロット割り当てまでを受け持ち、インライン候補の試行評価からも利用する。
`frontend/lowering/` はHIRから仮想CIRへの変換のみを担当する。

[cir/scaled_offset.rs](src/cir/scaled_offset.rs) は、配列offsetの小さい定数倍を
インライン選択後に最適化する。係数2〜8で初期low byteが分かる場合、lowへの定数倍Transferと
最大8回のcarry閾値比較へ置換する。sourceが0なら処理を飛ばし、最初の閾値未満では後続比較も省く。
公開binary CIRの `OffsetAddScaled` にも適用し、9以上・unknown low・alias・生存scratchは従来経路に戻す。
既定ON。公開CIR形式とABIは維持する。


共通の呼び出しグラフ解析（再帰判定・callee順序）は [cir/analysis/](src/cir/analysis/mod.rs)、
フレーム配置とコスト見積もりは [backend/layout_plan.rs](src/backend/layout_plan.rs) に置く。
インライン化の採否判定とBF生成はこの配置計算を共有し、BF emitter内部には依存しない。

`--cir-input` は [cir/input/format.rs](src/cir/input/format.rs) で外部形式を読み、
[cir/input/lowering.rs](src/cir/input/lowering.rs) で内部IRへ変換する。
外部CIRはselfhost compilerも出力するが、入力元をselfhostに限定しない。
既存の `SelfhostCir*` / `lower_selfhost_cir*` という公開API名とバイナリ形式は維持している。
この入力では既に共有されているフラットな記憶領域の関係を保存するため、ソース入力と同じ
インライン化・スロット再割り当ての経路には通さない。

どちらの入力も、`--run-ir` ではBF生成の代わりに [cir/vm/](src/cir/vm/mod.rs) で実行する。
`cell/` の低水準APIは `Cell IR → cell::codegen → BF IR` という独立した経路である。

## 選択式のBF生成option

以下は全て既定OFFで、独立に選択できる。通常/圧縮BFとprofile付き出力に共通である。

| CLI | `AbiCodegenOptions` | 動作 |
|---|---|---|
| `--enable-nibble-transfer` | `nibble_transfer` | global搬送のbyteをnibbleへ分解する。 |
| `--enable-inplace-compare` | `inplace_compare` | 比較に使うFrameSlotにzero/flagを予約し、ABI Scratchへのoperand搬送を省く。 |
| `--enable-anchor-bank` | `anchor_bank` | 16 anchorsを使い、stackのglobal往復を272 cells刻みで走査する。 |
| `--experimental-static-frames` | `static_frames` | globalへアクセスする閉じた非再帰関数群を、関数ごとの固定contextで実行する。 |

```sh
cargo run --release -p bf-compiler -- \
  --enable-inplace-compare --enable-anchor-bank --enable-nibble-transfer program.bfc > program.bf
```

公開CIRはflat frameを連続aggregateとして保つため、直接比較は従来方式へ戻す。
Anchor16とnibbleは`--cir-input`でも有効である。Anchor16と`--experimental-static-frames`も併用できる。
これらのoptionはCIR出力・inline判断を変更せず、BFを生成する際だけ使う。
論理RLE op数の削減を目的とし、全最適化ONのinterpreterの実時間では遅くなる場合もある。
配置とfallbackの詳細は[Rust ABI](../../ABI-rust.md#選択式の直接比較とanchor16)を参照。

通常の動的global portalは、accessor PCをglobal側で設定し、siteの復帰PCをcaller frameに
保持する。共有accessorの局所選択表でcallerへ戻るため、4 PC byteのcross-stack搬送を省く。
公開CIR入力にも同じbackendを適用する。固定frame・global数/hidden ID容量のfallbackは汎用経路。
詳細と評価は[global portalのPC保持](../../optimize_logs/GLOBAL_PORTAL_PC_EVALUATION_20261007.md)を参照。

`--experimental-static-frames`は既存フラグを再利用した実験経路で、到達先に再帰があるcallerは
動的stackに残す。固定context間のcall/returnは定数距離で移動し、calleeの返値をcaller専用resumeで
直接配送する。portal accessorは共有し、固定contextへの配送をsite別resumeへ置く。
nibble有効時は動的callerとの引数・返値搬送も分解し、liveな引数は元cellへ復元する。
Anchor16は固定領域の後ろに置き、動的stackとの境界だけに適用する。固定contextでも直接比較を使える。
固定portalの既知PCは搬送せず定数設定し、返値分解はcallee近傍のroute scratchを再利用する。
void／aggregate返値のABI Valueはcaller側で直接clearする。
local領域のcleanupはreturn側へまとめ、小さいglobalと共通scratchをanchorの隣に保つ。
固定frameは親子を分離し、同時に生きない兄弟のstorageを共有する。
[生成量の比較](../../optimize_logs/GLOBAL_CONTEXT_SIZE_EVALUATION_20261008.md)と
[配置・共有の評価](../../optimize_logs/GLOBAL_CONTEXT_LAYOUT_EVALUATION_20261008.md)を参照。
通常ABIのCIR・inline判断・言語仕様は変更しない。素BFが大きくなり、nibble併用のcompiler入力では
論理RLEが増える条件もある。[仕様](../../ABI-rust.md#実験的static-frame)と
[評価](../../optimize_logs/GLOBAL_CONTEXT_STATIC_FRAMES_EVALUATION_20261008.md)を参照。

## 名前の近い処理

- `cir/structure.rs`: Continuation IRを書き換え、局所的な分岐・ループを構造化する。
- `backend/regions.rs`: IRの意味を保ち、複数ブロックをまとめてBF出力する計画を作る。
- `cir/arithmetic_fusion.rs`: 比較・減算などの演算を融合する。
- `cir/frame_allocation.rs`: 生存期間に応じてスロットやaggregate領域を再利用する。
- `backend/frame_layout.rs` / `static_layout.rs`: フレーム／グローバル領域の物理配置を計算する。
- `backend/codegen/static_frames.rs`: 非再帰関数のフレームを固定配置する実験的バックエンド方式。

## テストと設計資料

各モジュール内のユニットテストに加え、[tests/](tests/) に機能別の統合テストがある。

- `tests/language/`: グローバル変数・配列・型・マクロなどの言語仕様。
- `tests/optimizations/`: 比較・減算の融合、局所フレーム、構造化制御などの最適化。
- `tests/cli/`: 出力形式、バックエンドのオプション、計測、入力保護、複数ソース。共通ヘルパーは `support.rs`。
- `tests/public_api.rs`: 公開IRの構築・コンパイル・プロファイルAPI。
- `tests/selfhost.rs`: 明示実行するselfhost全体の互換性検証。

[backend/codegen/tests/](src/backend/codegen/tests/mod.rs) はBF生成の回帰テストを
dispatch、call/return、命令、portal、region出力、インライン化に分けている。
`support.rs` は共通fixtureとVM/BF差分測定、`measurements.rs` は明示実行の成果物出力を担当する。
`cell/continuation_adapter.rs` もテスト専用である。[examples/](examples/) には比較・計測用ドライバがある。

リポジトリルートから通常の検証を実行する。

```sh
TMPDIR="$PWD/tmp" cargo test -p bf-compiler
cargo fmt -p bf-compiler -- --check

# 機能を絞る例
TMPDIR="$PWD/tmp" cargo test -p bf-compiler --test cli
TMPDIR="$PWD/tmp" cargo test -p bf-compiler --test language globals_and_arrays
TMPDIR="$PWD/tmp" cargo test -p bf-compiler --lib backend::codegen::tests::regions
```

明示実行が必要なselfhost全体のテストや測定は、通常のテストではignoredとなる。
詳細は [IR設計](../../IR.md)、[Rust ABI](../../ABI-rust.md)、
[インライン化](../../CIR_INLINE.md)、[領域出力](../../CIR_REGION_EMISSION.md)、
[スロット割り当て](../../FRAME_ALLOCATION.md) を参照。
