# Brainfuck実時間profiling設計

## 文書の位置づけ

この文書は、`bf-compiler`が生成したBrainfuckについて、実時間を主眼としたprofilingを行うための
artifact、compiler provenance、interpreter、CLI、report、検証方法を定める。

最適化の優先順位と導入手順は[BF_OPTIMIZATION_PLAN.md](BF_OPTIMIZATION_PLAN.md)に定める。
この仕様のversion 1は実装済みである。未実装のsource span、self-host compiler自身からのmarker生成、
profile比較toolは本文で明記する。wire formatを変更する場合はformat versionと本文を同時に更新する。

## 目的

- self-host testとbootstrap verificationのwall timeを支配する処理を特定する。
- BF sourceのload/parse、fast IR build、executeを分離して測る。
- source function、continuation、FrameInstruction、ABI templateへ実行costを帰属する。
- compiler最適化前後およびABI version間で同じ分類のprofileを比較する。
- profilingを無効にした通常BFの実時間を最適化の採否値として維持する。
- Rust compilerで得たtemplate単位の知見をBFC製compilerへ移植できるようにする。

## 非目的

- 任意の外部Brainfuck処理系で共通に使えるdebugger protocolを標準化しない。
- cycle-accurateなhardware performance counterを初期versionで提供しない。
- 各raw BF命令の実行前後でclockを読む方式をdefaultにしない。
- profiling実行のwall timeを、通常実行の性能値として扱わない。
- 初期versionではsource-level breakpoint、step execution、tape viewerを実装しない。

後からdebuggerを追加できるようにprofile siteとsource locationは保持するが、最初の目的は時間の
attributionである。

## 測定の二系統

### Baseline measurement

profilingを無効にし、markerを含まない通常BFを実行する。最適化の合否はこの結果で決める。

次の時間を記録する。

| 項目 | 範囲 |
|---|---|
| `source_read` | CLIがBF artifactを読み込む時間 |
| `profile_map_read` | sidecarを読む時間。baselineでは0 |
| `parse` | BF 8命令の抽出とbracket対応表の構築 |
| `fast_ir_build` | RLE、clear、scan、linear transfer等への変換 |
| `execute` | inputを受け取った状態からBF終了まで |
| `output_write` | 実行結果をCLI stdoutへ書く時間 |
| `process_total` | CLI開始から正常終了直前までの内部計測 |
| `external_wall` | benchmark runnerが観測するprocess全体のwall time |

`external_wall`を最終的な主指標とし、内部区間は差の原因説明に用いる。repository testの実際の実行形態が
複数processやpipeを含む場合は、そのscript全体のwall timeも別に測る。

### Attribution profiling

profile mapまたは埋め込みmarkerから各BF命令のprofile siteを解決し、site別counterおよび時間分布を
収集する。attribution実行にはoverheadがあるため、baselineとは別runにする。

profiling modeは次の三つとする。

| mode | 内容 | 用途 |
|---|---|---|
| `counters` | site別operation counter | exact countが必要な調査 |
| `sample` | wall-time samplingのみ | 大きなself-host workload、低overhead |
| `exact` | profile block境界でclockを読む | microbenchmark、短いprobe |

## Profile site model

### ProfileSiteId

`ProfileSiteId`は一つのcompilation artifact内だけで有効な非負`u32`とする。ID 0はartifact rootに予約する。
異なるcompile間で同じ数値IDが同じ意味を持つとは限らない。比較にはsite kindとstable keyを使用する。

```rust
struct ProfileSiteId(u32);
```

### Site hierarchy

各siteは0個または1個のparentを持つ。root以外のsiteは必ずparentを持ち、cycleを含んではならない。
一つのBF命令は一つのleaf siteへ属し、report時にparent chainへinclusive costを集計する。

標準階層は次を基本とするが、存在しない層は省略してよい。

```text
artifact root
  -> source file
    -> source function
      -> continuation
        -> FrameInstruction or Terminator
          -> ABI operation
            -> BF template variant
```

compiler生成の初期化やdispatcherのようにsource functionへ属さない処理は、root直下のABI siteへ置く。

### Site fields

```text
id             u32
parent         u32 or null
kind           stable ASCII string
stable_key     stable ASCII string
label          human-readable UTF-8 string
source         optional SourceSpan
attributes     string-to-string map
```

`stable_key`はcompile間の比較に使用する。連番だけを含めず、可能な場合は次のような構造的名称にする。

```text
abi.dispatcher
abi.dispatch.case.low-byte
abi.portal.divmod.fixed-16
abi.portal.window.right
function.compile_program_stage5
frame_instruction.transfer
terminator.call
```

continuationのようにsource変更で対応が変わりうるsiteは、function stable key、source span、terminator kind
などをattributeへ持たせる。完全なcompile間対応が取れない場合はkind単位で集計する。

### SourceSpan

```text
file_id        u32
start_byte     u64, inclusive
end_byte       u64, exclusive
```

line/columnはmap生成時またはreport表示時にsource file tableから計算する。UTF-8 byte offsetを正とし、
Unicode scalarまたは表示columnをartifactのidentityに使用しない。

## Compiler内のprovenance

### Annotated BF IR

ABI backendはprofile siteを持つ内部BF IRを生成する。既存のpublic `BfInstruction` APIを直ちに変更せず、
内部wrapperまたは平行するinternal typeを導入してよい。

概念上は次の形である。

```rust
struct AnnotatedBfInstruction {
    site: ProfileSiteId,
    operation: AnnotatedBfOperation,
}

enum AnnotatedBfOperation {
    Move(isize),
    Add(u8),
    Input,
    Output,
    Loop(Vec<AnnotatedBfInstruction>),
}

struct AnnotatedBfProgram {
    instructions: Vec<AnnotatedBfInstruction>,
    sites: ProfileSiteTable,
}
```

profilingを出力しない通常compileでもannotated IRを使ってよいが、profile table構築を無効化できることが
望ましい。どちらの場合も最終的な8種類のBF命令列は同一でなければならない。

### Emission context

`AbiEmitter`は現在のsite stackを持つ。dispatcher、portal、callなどのhelperへ入るとchild siteをpushし、
終了時にpopする。生成された命令にはstack topのsite IDを付ける。

site作成と命令生成を混ぜすぎないため、次の操作を用意する。

```text
with_profile_site(kind, stable_key, attributes, emit)
current_profile_site()
intern_profile_site(parent, kind, stable_key, attributes)
```

同じparentとstable keyを持つsiteはtable内で再利用してよい。source上の個別operationを区別する必要が
ある場合はsource spanまたはlocal ordinalをkeyへ含める。

### Optimizerの規則

profile boundaryはoptimization barrierにしてはならない。profiling有無でBF命令列が変わるのを防ぐため、
peephole optimizerはsiteを付帯情報として扱う。

- 同じsiteの命令を統合した場合はそのsiteを維持する。
- 異なるsiteの命令を統合した場合は、両siteのlowest common ancestorを使用する。
- 命令が完全に相殺または上書きされた場合、その命令のsite rangeも消える。
- loopの`[`と`]`はloop node自身のsiteに属する。
- loop bodyの命令はそれぞれのsiteを維持する。
- native clear/scan/transferへ変換されたloopの実時間はloop nodeのsiteへ帰属する。
- optimizer由来の命令を新設する場合は、元命令のlowest common ancestorまたは専用の
  `optimizer.generated` child siteへ属させる。

lowest common ancestorへ昇格した命令が多い場合はreportに`mixed_provenance_commands`として数え、
attribution精度を確認できるようにする。

## Sidecar profile map

### 主形式

markerなしの`program.bf`と、対応する`program.bfmap.json`を生成する。sidecarはprofiling時だけ必要で、
通常のBF処理系は`program.bf`だけを実行できる。

初期formatはhuman-readableでtoolingを作りやすいJSONとする。大きさが問題になった場合は同じ論理schemaの
binary encodingを追加し、JSONをdebug/export formatとして残す。

### 座標系

rangeはsource byte offsetではなく、BF命令ordinalを使用する。

- `><+-.,[]`のいずれかを1 instructionと数える。
- BF以外のbyteはordinalを進めない。
- ordinalは0始まり。
- rangeは`[start, end)`のhalf-open interval。
- rangeはstart順で重ならない。
- adjacentかつ同一siteのrangeは一つにまとめる。
- すべてのBF命令をちょうど一つのrangeがcoverする。

この座標系により、埋め込みmarkerや通常のBF commentの有無で対応が変わらない。

### Artifact identity

sidecarの取り違えを検出するため、BF以外のbyteを除いた命令列について次を保存する。

- `instruction_count`。
- FNV-1a 64-bit hash。

FNV-1aは暗号学的identityではなく、開発中のartifact取り違えを検出するために使用する。interpreterは
countまたはhashが一致しないmapをerrorとして拒否する。将来、外部から与えられるuntrusted mapを扱う
場合は強いhashへversionを上げる。

### JSON schema version 1

概略は次のとおりである。

```json
{
  "format": "bfc-bf-profile-map",
  "version": 1,
  "bf": {
    "instruction_count": 123456,
    "fnv1a64": "8f4f0d7a0b2c1234"
  },
  "files": [
    { "id": 1, "path": "selfhost/stage2/compiler/02_lexer.bfc" }
  ],
  "sites": [
    {
      "id": 0,
      "parent": null,
      "kind": "artifact",
      "stable_key": "artifact.root",
      "label": "stage2-tests",
      "source": null,
      "attributes": {}
    }
  ],
  "ranges": [
    { "start": 0, "end": 128, "site": 3 },
    { "start": 128, "end": 192, "site": 7 }
  ]
}
```

整数はJSON numberで表すが、hashは16桁のlowercase hexadecimal stringとする。unknown fieldはreaderが
無視してよい。同じmajor `version`内でrequired fieldの意味を変更しない。

## BF埋め込みmarker

### 用途

- BFとprofile情報を一つのstreamで渡す。
- BFC製self-host compilerがsidecar fileを扱えない段階でprofile siteを出力する。
- compiler outputをpipeで直接interpreterへ渡す。
- profile map生成器のdebug。

marker入りartifactは8種類のBF命令列がmarkerなしartifactと完全一致するため、通常のBF interpreterでも
同じ意味で実行できる。ただしfile sizeとparse負荷が増えるのでbaseline measurementには使用しない。

### 安全文字

marker自身とpayloadはASCIIの`><+-.,[]`を一切含んではならない。function名、path、JSONをrawで埋め込まない。
文字列payloadが必要な場合はRFC 4648 base32のuppercase alphabet `A-Z2-7`とpadding `=`でencodeする。

### Version 1 grammar

次のrecordを定義する。

```text
@BFCDBG1;                 stream header
@S42:BASE32PAYLOAD;       site 42のUTF-8 JSON record
@P42;                     以降のBF命令をsite 42へ切替
@P0;                      root siteへ戻す
@ENDDBG;                  metadata recordの終了
```

`BASE32PAYLOAD`にはsidecarのsite object一個分に相当するUTF-8 JSONをencodeする。source file tableが必要な
場合は、同様に`@F` recordを将来追加できる。unknown uppercase recordは末尾`;`まで安全文字だけで構成
される場合に限り無視してよい。

version 1の埋め込みmarkerだけを使用するartifactでは、siteの`source`を`null`とする。source spanを含む
profileにはsidecarを併用する。`@F` recordを正式に追加するときはmarker format versionを上げるか、
version 1 readerが明示的に認識できるoptional recordとして仕様を更新する。

interpreterはheaderがない`@P`文字列をprofile markerとして解釈せず、通常の無視されるcommentとして扱う。
headerを認識した後にgrammar違反、duplicate site、unknown site selectionがあればprofile artifact errorとする。
BFの実行自体だけを要求された場合にerrorとするかmarkerを無視するかはCLI optionで選択できるが、profiling
modeでは必ずerrorとする。

### Markerとsidecarの一致

両方が与えられた場合は次を検証する。

- BF命令列のidentityがsidecarと一致する。
- markerから得たsite切替rangeがsidecar rangeと一致する。
- site IDのparent、kind、stable keyが一致する。

不一致時はどちらかを優先せずerrorにする。

## Interpreterの内部表現

### Parse結果

parserはBF sourceを一度走査し、source byte offset、BF命令ordinal、profile site IDを解決しながら直接fast IRを
構築する。raw BF命令ごとの中間配列は作らない。

```text
operation
source_byte_offset
instruction_ordinal
profile_site_id
```

sidecar rangeはordinalが進む順に一度だけ走査して適用し、命令ごとのbinary searchを避ける。これにより巨大な
self-host artifactでもprofile付きparseの空間量をfast IRとsource sizeに抑える。profilingなしではsite IDを
常に0とし、追加処理を最小化する。

### Fast IR

fast IR operationもsite IDを持つ。同じsiteの隣接operationは可能なら`ProfileBlock`へまとめる。

```rust
struct ProfileBlock {
    site: ProfileSiteId,
    instructions: Vec<FastInstruction>,
}
```

generic loop bodyは複数blockを持てる。native clear、scan、linear transferはloop siteに属する一つの
FastInstructionとして扱い、raw/RLE換算metadataは従来どおり保持する。

複数siteのraw operationから一つのnative operationを作る場合は、compiler側と同じlowest common ancestor
規則を用いる。この件数も`mixed_provenance_native_operations`としてreportする。

## Counter profiling

site IDをindexとする`Vec<SiteCounters>`を使用し、実行hot pathで`HashMap`を更新しない。

```text
fast_operations
raw_bf_instructions
rle_instructions
loop_entries
loop_iterations
rle_operations
clear_loops
scan_loops
scan_steps
transfer_loops
transfer_iterations
input_operations
output_operations
pointer_distance
maximum_pointer_observed
```

連続して同じsiteを実行している間はthread-localまたはexecutor-local accumulatorへ加算し、site切替時に
`Vec`へflushしてよい。profilingなしの既存global `RunStats`と合計値が一致しなければならない。

## Sampling profiler

### 方式

実行threadは現在のprofile siteを共有atomicへ公開する。同じsiteの`ProfileBlock`へ入る時だけ更新し、raw BF
命令ごとには更新しない。`u32::MAX`をinactive sentinelとして予約し、execute開始前と終了後はinactiveにする。
samplerはexecute開始barrierを通過してから既定1 ms間隔でmonotonic clockに基づいて現在siteを読み、activeな
siteだけへsample数を加算する。execute終了時はinactiveへの更新後にsamplerを停止してjoinする。

sampling intervalはCLIで変更可能にする。短くすると分解能とoverheadが増え、長くすると短時間siteの誤差が
増える。大きなself-host workloadでは1 msをdefaultとする。

### 集計

siteの推定exclusive timeは次で求める。

```text
site_samples / total_samples * measured_execute_time
```

parentのinclusive timeはdescendantのexclusive sampleを加算する。reportには必ずsample数とintervalを表示し、
sample数が20未満のsiteには`low_confidence`を付ける。

### 制限

- sampler threadのschedulingにより短時間runでは誤差が大きい。
- system loadやCPU migrationの影響を受ける。
- sampling自体がprocessのwall timeを変える。
- sampling runの時間をbaseline性能値に使用しない。
- input/outputは事前にbufferされるため、通常は外部I/O待ちをsiteへ帰属しない。

sampler threadのoverheadが無視できない環境向けに、将来Linuxのprocess CPU timerまたは外部profilerとの連携を
追加してよい。初期versionではportableな実装とcounterの一致を優先する。

`sample` modeはsite別operation counterを更新しない。global `RunStats`は維持するが、site別のexact countが
必要な場合は`counters` modeを使用する。samplingとfull counter更新を同時に行うと、大きなself-host workloadで
samplingの低overheadという目的を失うためである。

## 実行中snapshotとinterrupt

CLIはprofileの有無にかかわらずSIGINTを捕捉し、VM execution threadが安全な位置で最後のsnapshotをstderrへ
出してexit code 130で終了する。signal handler自身はatomic flagの更新だけを行い、allocation、I/O、profile
集計は行わない。`--progress-interval 10s`を指定すると同じ形式を周期的にも出す。
snapshot待ちの間にもう一度SIGINTを受けた場合は即座にexit 130とする。

snapshotにはelapsed、現在siteとその親context、input/output byte数、native/RLE instruction数、pointer、RSSを
含める。profile付きの場合はsample数またはfast operation数による上位10 siteも含める。実行hot pathでは
16,384 native operationごとにだけ時刻とinterrupt flagを確認し、通常実行への影響を抑える。

## Exact profiler

exact modeは`ProfileBlock`の実行開始と終了でmonotonic clockを読み、elapsed timeをsiteへ加算する。generic loop
では反復ごとに実行されたblockを計測し、native operationではnative operation全体をloop siteへ計上する。

clock read回数、profile block実行回数、計測された合計時間をreportする。blockが短い場合のoverheadが大きいため、
full self-host workloadのdefaultにしない。microbenchmarkでtemplate variant間の相対比較を行う用途とする。

exact modeのreportには次を含める。

- `clock_reads`。
- `profile_block_executions`。
- `measured_execute_time`。
- site別exclusive duration。
- duration合計とexecute timeとの差。

## Public API

既存の`run`、`run_with_stats`、`run_unbounded` APIは互換性のため残す。新しいoption付きAPIを追加する。

```rust
pub struct RunOptions {
    pub unbounded_tape: bool,
    pub collect_stats: bool,
    pub collect_timings: bool,
    pub profile: Option<ProfileOptions>,
}

pub enum ProfileMode {
    Counters,
    Sample { interval: Duration },
    Exact,
}

pub struct ProfileOptions {
    pub map: ProfileMap,
    pub mode: ProfileMode,
}

pub struct Timings {
    pub parse: Duration,
    pub fast_ir_build: Duration,
    pub execute: Duration,
}

pub struct ProfileResult {
    pub sites: Vec<SiteProfile>,
    pub total_samples: u64,
    pub sampling_interval: Option<Duration>,
    pub clock_reads: u64,
}
```

CLI固有のsource readとoutput writeはlibraryの`Timings`へ含めず、CLI側のreport fieldとして追加する。

## CLI

```text
bf-interpreter [options] program.bf

--stats
--timings
--progress-interval 10s
--no-progress
--profile-map PATH
--profile-mode counters|sample|exact
--profile-sample-interval 1ms
--profile-output PATH
--profile-format text|json
--accept-embedded-profile
--unlimited-tape
```

規則は次のとおりとする。

- `--profile-map`または`--accept-embedded-profile`がprofiling sourceを指定する。
- profile sourceが指定された場合のdefault modeは`counters`。
- `sample`のdefault intervalは1 ms。
- `--profile-output`未指定時、text reportはstderrへ出す。
- BF program outputは従来どおりstdoutへ出し、profile reportと混在させない。
- machine-readable resultはJSONを初期形式とする。
- `--timings`だけではsite provenanceを読み込まない。
- SIGINT時はprofileの有無にかかわらず最後の`bf-progress` snapshotをstderrへ出す。
- `--progress-interval`はprofileなしでも使用できる。profile付きなら現在siteとhot sitesも表示する。
- 純粋なbaselineを取る場合は`--no-progress`でSIGINT pollも無効化できる。

compiler CLIには次を追加する。

```text
bfc --profile-map-output program.bfmap.json sources...
bfc --cir-input program.cir --profile-map-output program.bfmap.json
bfc --embed-profile sources...
bfc --profile-granularity abi|continuation|instruction|source
```

`--profile-map-output`はsource入力とself-host CIR入力の両方で使用できる。stdoutへBF、指定pathへsidecarを書く。
同じpathやstdoutを両方へ指定することはerrorにする。

## Granularity

profile site数とruntime overheadを制御するため、compilerはgranularityを選択できる。

| granularity | 含むsite |
|---|---|
| `abi` | dispatcher、portal、call/return等のABI templateのみ |
| `continuation` | `abi` + function、continuation |
| `instruction` | `continuation` + FrameInstruction、Terminator |
| `source` | `instruction` + source span |

defaultは`continuation`とする。最初のbottleneck調査では`abi`または`continuation`を使用し、site切替とmap sizeを
抑える。特定functionを調査するときだけ`instruction`または`source`を使う。

将来、include/exclude filterを追加してよい。

## Report

### Text report

少なくとも次を表示する。

```text
artifact
profile mode and overhead metadata
phase timings
global RunStats
profile site tree sorted by estimated exclusive time
top leaf sites
top stable kinds aggregated across instances
mixed/unknown provenance counts
```

各site行には次を含める。

- estimatedまたはexact exclusive time。
- inclusive time。
- total execute timeに対する割合。
- exact modeでclock readなどの計測overheadを除いて比較するため、siteへ帰属できたexclusive time合計に
  対する割合。
- samplesまたはclock reads。
- fast/raw/RLE operation数。
- call countに相当するprofile block entry数。
- source locationがある場合はfile、line、column。

### JSON report

JSON reportにはraw counter、duration nanoseconds、sample count、site table、phase timings、artifact identity、
interpreter build情報を含める。wall time基準と帰属時間基準の割合も便宜上含めるが、割合や表示用丸め値
だけを保存せず、再集計可能な整数値を正とする。

compile間比較toolは`stable_key`とkindで集計し、site IDをjoin keyに使用しない。

## 実時間benchmark protocol

repositoryの公式baselineを更新するときは次を記録する。

- git revision。
- release/debug profile。
- Rust compiler version。
- OS、architecture、CPU model。
- interpreter option。
- ABI versionと`D`。
- input artifact identity。
- input data identity。
- warm-up回数とmeasurement回数。
- 各runの値と中央値。
- peak RSS。

可能であれば同一machineでCPU governorとbackground loadを安定させる。CPU pinningを利用した場合はcommandを
記録する。長いself-host verificationは最低3回、短いmicrobenchmarkはより多く測る。

性能比較表では次を混同しない。

- compilerがBFを生成する時間。
- interpreterがBFをload/parseする時間。
- fast IR build時間。
- BF execute時間。
- self-host compilerが入力programをcompileする時間。
- 生成されたprogramを実行する時間。
- verification script全体のwall time。

## Error handling

次をprofile artifact errorとする。

- unsupported map version。
- BF instruction countまたはhashの不一致。
- rangeの重複、逆順、gap、範囲外。
- unknown site ID。
- site parent cycle。
- duplicate site ID。
- malformed embedded marker。
- unsafe marker payload。
- sidecarとembedded markerの不一致。

profiling artifact errorはBF runtime errorと区別したerror variantにする。profilingを要求していない通常実行では、
BF以外のbyteは従来どおり無視する。

## Correctnessとoverheadの検証

### Artifact invariants

- markerあり/なしのBF 8命令列がbyte-for-byteで一致する。
- annotated/unannotated compileのBF 8命令列が一致する。
- sidecar rangeが全BF命令を一度ずつcoverする。
- sidecar、embedded marker、compiler内annotated IRから得るsite列が一致する。
- optimizer前後で消えた命令を除き、provenanceが規則どおり維持される。

### Interpreter invariants

- profiling有無でprogram outputとruntime errorが一致する。
- site counter合計がglobal `RunStats`と一致する。
- bounded/unbounded tapeで既存のerror offsetを維持する。
- fast IR optimization有無でraw/RLE換算counterが一致する。
- sample/exact modeでもBF semanticsが変化しない。

### Overhead measurement

同じartifactについて次を測る。

1. profilingなし。
2. `counters`。
3. `sample`。
4. `exact`。

reportには1に対するwall time倍率を記載する。初期目標は、大きなself-host workloadで1 ms samplingを15%以内の
overheadに抑えることとする。`counters`はexact countのための高overhead modeとして別に評価する。達成できない
場合もcorrectnessを優先し、baselineとprofiling結果を混同しない。exact modeには一律のoverhead上限を設けない。

2026-09-02の小型continuation workload（3回平均）では`--no-progress` baseline 0.764秒、既定のSIGINT
snapshot待受0.774秒（+1.2%）、`counters` 1.311秒（+71.6%）、1 ms `sample` 0.846秒（+10.7%）だった。
full compiler BFの2 ms `sample`では同じ6秒時点の入力消費がprofileなし1610 bytes、sample 1556 bytesで、
概算overheadは3〜4%だった。

## 導入順序

1. `--timings`とphase timingを追加する。（完了）
2. profile site tableとannotated BF IRを追加する。（完了）
3. serializerからsidecar rangeを生成する。（完了）
4. interpreterがsidecarを検証し、site別counterを収集する。（完了）
5. ABI granularityでcurrent self-host workloadをprofileする。（完了）
6. continuation、instruction granularityを追加する。（完了。source granularityのsource spanは未実装）
7. sampling profilerを追加する。（完了）
8. exact profilerをmicrobenchmarkへ追加する。（完了）
9. embedded numeric markerを追加する。（完了）
10. base32 site dictionaryとself-host compilerからのmarker生成を追加する。
    （Rust compilerのdictionary生成は完了。BFC製compilerからの生成は未実装）
11. profile比較toolと継続的なbaseline保存を追加する。（未実装）

最初の最適化判断には手順5まででも十分な情報が得られる。source-level debuggerに近い機能の完成を待たず、
ABI category別counterとphase timingが利用可能になった時点でdispatcherとportalの調査を開始する。
