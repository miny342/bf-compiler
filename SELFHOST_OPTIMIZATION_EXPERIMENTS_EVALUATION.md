# Selfhost 最適化実験の評価

> 現在のABIはD=16のみをサポートする。以下のD=8への言及は実験当時の記録である。

## 検証済みの採否

| 案 | 判断 | 根拠と制約 |
|---|---|---|
| selfhostのbounded SLIDE nibble increment | 採用（既存nibble flag内、既定OFF） | 4 bitの左右zero guardでincrementし、右guard経由でhigh carry。frame幅不変、flag ONのstatic scratchだけ6→7 cells。arena nibble有効でRLE−0.961%、native−4.942%、圧縮BF−1.913%。全byte・dirty guard・再帰・両flagの実stage2・RLE-only counter・出力形式を照合。通常BFと公開CIR不変、IR VMに明らかな悪化なし。[評価](optimize_logs/SELFHOST_DEADLINE_LOOP_20261010.md)。 |
| selfhostの静的local returnを直接payloadへ | 採用（既定ON、BF・公開CIR共通） | localの名前・固定field/indexをReturnへ直接渡し保存temporaryを省略。2c9f57c比arena RLE−3.675%、compilerのarena生成RLE−3.797%、公開CIR→Rust−1.136%、IR VM20.8秒を維持。global／動的place／計算式は従来materialize。全byte・field/index・fallback・再帰・nibble・CIR・limitsを照合、wire追加なし。[評価](optimize_logs/SELFHOST_DEADLINE_LOOP_20261010.md)。 |
| selfhostのcallee header／caller inboxの既知zero | 採用（既定ON、BF backend） | 空きframeのActive・PCはAddだけで設定し、resumeで消費済みValue/outboxへの配送前clearを省略。d817ceb比arena RLE−0.04618%、生成stage2 BF−2.492%、IR VM約21秒。深さ255相互再帰・返値長・global保存・nibble・再帰portal・CIRを照合。return全域cleanupは維持。[評価](optimize_logs/SELFHOST_DEADLINE_LOOP_20261010.md)。 |
| selfhostの逆順SLIDE比較でswap省略 | 採用（既定ON、BF backend） | operand順とresult destinationを指定し、`>`／`<=`前の3 transferを除去。全65,536 pairの四比較kernelでRLE−13.252%、圧縮BF−5.36%。arenaは不変、compilerのarena生成RLE−0.080%、IR VM約21秒。逆順result・scratch・alias・実RLE-onlyを照合、frame幅・公開CIR不変。[評価](optimize_logs/SELFHOST_DEADLINE_LOOP_20261010.md)。 |
| selfhostのValue/outbox受信move | 採用（既定ON、BF backend） | resumeで一度だけ読む返値を所有temporaryへ消費し、保存・復元を省略。cfaa654比arena RLE単独−3.334%、call前clear省略併用−3.636%。深さ255の相互再帰・重複・global保存・32セル配列・nibble・CIR・実RLE-onlyを確認。公開CIR byte不変、stage3全段未測定。[評価](optimize_logs/SELFHOST_RECEIVE_ZERO_FRAME_EVALUATION_20261010.md)。 |
| selfhostのcall前全域clear省略 | 採用（既定ON、BF backend） | 初回tape0／return全域cleanup／portalが空きframeを汚さない契約により省略。cfaa654比arena RLE単独−0.302%、受信move併用−3.636%、生成stage2 BF−40.995%、全ON kernel execute中央値−6.759%、自己入力IR VM約21秒。frame幅維持、再帰portal・301関数・255-cell frameを検証。return cleanupは維持、stage3全段未測定。[評価](optimize_logs/SELFHOST_RECEIVE_ZERO_FRAME_EVALUATION_20261010.md)。 |
| selfhostの引数snapshotを消費するcall搬送 | 採用（既定ON、BF backend） | 独立した所有temporaryをcalleeへmoveし、source保存・復元を省略。113f140比arena RLE単独−8.69%、直接return併用−13.19%、生成stage2 BF−4.87%、自己入力--run-ir約21秒を維持。重複・後続global変更・再帰・32セル配列・nibble・公開CIR・実RLE-only端点を検証。wire/ABI不変、stage3全段未測定。[評価](optimize_logs/SELFHOST_CALL_RETURN_MOVES_EVALUATION_20261010.md)。 |
| selfhostのlocal返値をcallerへ直接消費 | 採用（既定ON、BF backend） | local→FRAME_VALUE→callerの中継・保存copyを除去。113f140比arena RLE単独−4.51%、call併用−13.19%。scalar／aggregate／global返値・再帰・source保存を検証。戻りPC・全frame cleanupを維持し、受信側Value/outboxの保存copyは残る。公開CIR生成物はbyte不変、stage3全段未測定。[評価](optimize_logs/SELFHOST_CALL_RETURN_MOVES_EVALUATION_20261010.md)。 |
| selfhostのknown-zero low／stride1 offset | 採用（既定ON、BF・公開CIR共通） | projection間でlow=0を証明し、byte indexを所有temporaryから直接IR_ADDで加算。arena RLE79.10→28.43億（−64.06%）、公開CIR→Rust60.79→9.95億（−83.64%）。unknown low／他strideは従来carry、ABI・wire形式不変、自己入力--run-ir約21〜22秒。全byte／nested／field carry／call／CIR／nibble／RLE-onlyを検証、stage3全段未測定。[評価](optimize_logs/SELFHOST_OFFSET_COMPARE_ADOPTION_20261010.md)。 |
| selfhostのSLIDE大小比較 | 採用（既定ON、frame拡張なし） | scratch 11〜14へoperandを一度だけ消費し、二つの出口をzeroへ合わせてboolを返す。zero-low採用後arena RLE28.43→26.29億（追加−7.53%）、圧縮BF微減、--run-irに明らかな悪化なし。四関係・dirty scratch・離れたoperand・aliasを全pair、RLE-only端点も検証。早期終了できたゼロpairの局所悪化は残る、公開CIR比較は不変、stage3全段未測定。[評価](optimize_logs/SELFHOST_OFFSET_COMPARE_ADOPTION_20261010.md)。 |
| Rust CIR版3／weight256の既定化 | 採用（既定ON、selfhost未移植） | 閉じたcallee・末尾wrapperと再帰SCC停止点を割り当て前CIRで展開、B1 root増大を棄却。従来の汎用方式は`--enable-generic-function-inline`で選択。自己入力stage1→stage2の論理RLE−16.55%、native126.972秒（従来127.280秒）、Rust生成込み137.382秒、IR VMとstage2 byte一致。nibble等3optionは既定OFF、identity v7へ更新。[評価](optimize_logs/CIR_FORWARDING_DEFAULT_EVALUATION_20261009.md)。 |
| Rust regionの単一終端selector省略／非末尾wrapper／entry prefix | 検証ブランチ更新（selectorは既定ON、他は既定OFF、selfhost未移植） | 版3/weight256・三option比compiler RLEはselector−0.00346%／非末尾wrapper＋1.22539%／live prefix＋3.44948%。selectorは圧縮BF−0.10664%、frame/dispatch先不変。wrapperはroot非増大でもdispatcher費用増、prefixはcopy減より引数搬送等の増加が大きく既定化しない。小prefix相殺例はp=255でRLE−8.00%。481テスト・CLI native/RLE-only4,224実行・公開CIR48条件・fmt/clippy成功。[評価](optimize_logs/REGION_CANDIDATES_EVALUATION_20261009.md)。 |
| Rust割り当て前inlineの末尾wrapper・再帰SCC停止点 | 検証ブランチ更新（既定OFF、selfhost未移植） | `BFC_EVAL_CLOSED_INLINE=3`でscalar/void結果をそのまま返すwrapperを展開し、内側Callのresult/resumeを既存callerへ転送。各再帰SCCの代表を残し他メンバーを条件付き展開。weight256／三optionの既定inline比compiler RLE−3.5663%／素BF−21.2524%／圧縮BF＋15.8537%、dispatch先2,617→2,406、arena RLE−0.9119%。weight128はRLE＋3.3890%だが圧縮BF−27.0457%。非末尾再帰・aggregate返値は対象外。474テスト・CLI native/RLE-only1,728実行・全256入力SCC・旧経路10artifact／公開CIR48artifact一致・fmt/clippy成功。[評価](optimize_logs/FORWARDING_INLINE_SCC_EVALUATION_20261009.md)。 |
| Rustの中断しないcalleeの割り当て前inline | 検証ブランチ更新（既定OFF、selfhost未移植） | `BFC_EVAL_CLOSED_INLINE=2`／weight128でCall・portalのないcalleeだけをvirtual CIRへ取り込み、計算グラフ・DSE・allocationを実行。関数inline OFF基準比compiler RLE−27.07%／圧縮BF−7.54%、arena RLE−19.63%／圧縮BF−2.37%、compiler dispatch先3,555→2,652。既定の汎用inline比ではcompiler RLE＋16.79%で代替未達。weight無制限のBF約6倍増を記録し上限を導入。全256byteの相殺fixture・CLI native/RLE-only640実行・467テスト・fmt/clippy・公開CIR96artifact不変を確認。[評価](optimize_logs/CLOSED_CALLEE_INLINE_EVALUATION_20261009.md)。 |
| Rust共有call regionのscalar引数共有・header初期化省略 | 検証ブランチ更新（既定OFF、selfhost未移植） | `BFC_EVAL_DIRECT_BINDINGS=1`で閉じた固定calleeの未変更parameterをcallerセルへ結び付け、全帰路が直接実行される場合はheader初期化を省略。従来の展開候補に限定したa51fb60比compiler RLE−0.04125%／圧縮BF−1.08455%、arena RLE不変／圧縮BF−2.40606%。書き込みまで共有する版2はRLE＋0.24005%で比較用のみ。dispatch ID集合不変、native/RLE-only9,280実行・公開CIR96条件・460テスト・fmt/clippy成功。固定storage予約は維持、全段／実時間未測定。[評価](optimize_logs/DIRECT_PARAMETER_BINDINGS_EVALUATION_20261009.md)。 |
| Rust共有continuation call展開のPC・返値・帰路上限削減 | 検証ブランチ更新（既定OFF、selfhost未移植） | `experiment/shared-call-regions`でentry PC・既知returnのPC transfer・閉じたentryのReturnPC保存を省略し、固定callerへの返値直書きと帰路の独立上限を追加。同じ入口上限2,048・深さ4の前試作比compiler論理RLE−6.12%／arena−5.88%、圧縮BF＋6.43%／＋17.17%。関数inline OFF基準比compiler−12.22%だが既定inlineの代替性能には未到達。dispatch先3,555／128を維持、CLI1,440実行・公開CIR48条件・455テスト・fmt/clippy成功。[評価](optimize_logs/DIRECT_CALL_PC_RETURN_EVALUATION_20261009.md)。 |
| Rustのglobal context別自動inline予算 | 採用（source経路、既定ON） | 前回truth guard採用tree比で自己入力の論理RLE−3.6711%／native op−7.1613%、次段BF 3,976,080 bytes完全一致。global関数ごとに初期IR weight×8・最低1,024の予算を持ち、動的callerは共有予算、frame増大禁止は維持。large64併用RLE−3.2594%／生成compiler圧縮BF＋12.5034%・素BF＋25.8867%、小入力の後退あり。frame増大版と二段階版は不採用。450テスト・fmt/clippy、production4artifact一致。selfhost未移植、stage2→stage3／RLE-only全段未測定。[評価](optimize_logs/GLOBAL_CONTEXT_INLINE_BUDGET_EVALUATION_20261009.md)。 |
| Rust固定truth bank・小aggregate padding・固定region slide | 採用（既存直接比較フラグ内） | 97f51c1比compiler併用RLE−1.2869%／arena−4.2361%、compiler素BF＋0.3382%／圧縮＋0.0762%。通常BF不変。固定scalarへのprivate guard予約とn=1〜5の静的local aggregate paddingを使い、frame portal対象は除外。inline緩和はcompiler約40%悪化で不採用。448テスト・公開CIR48条件・実RLE-only counter・fmt/clippy確認。selfhost未移植／全段未測定。[評価](optimize_logs/FIXED_TRUTH_GUARDS_EVALUATION_20261009.md)。 |
| Rust固定global contextの既定化 | 採用（既定ON） | 12ac995の共有portal・兄弟storage共有を含む閉じた非再帰global関数群の固定配置を、source／公開CIR／APIの既定に変更。従来動的ABIとの全ON native交互5組のexecute中央値はcompiler通常−9.42%（1.069→0.969秒）／三option−38.81%、arena通常−4.19%／三option−51.22%。三optionは既定OFFを維持。`--disable-static-frames`で動的ABI、旧experimental名は互換alias。容量／hidden ID不足時は動的方式へ再計画。444テスト・fmt/clippy・公開CIR48条件・旧経路8artifactの同一性確認。selfhost未移植、全段時間未測定。[評価](optimize_logs/GLOBAL_CONTEXT_DEFAULT_EVALUATION_20261009.md)。 |
| Rust experimental固定contextのportal request/return共有 | experimental更新（既定OFF） | 関数・global・load/store別の共有returnでABI Valueまで配送し、site別resumeは局所配送だけにする。複数leafの往路も共有、単一leafはinline。8033312比compiler三option ONで素BF−47.45%、圧縮BF−0.59%、RLE＋4.17%、全ON execute交互5組中央値＋6.47%。通常ABI比は素BF＋3.66%、RLE−17.17%。arena三option ONは素BF−52.31%／RLE＋6.05%。442テスト・実RLE-onlyのpage境界batch／再帰caller・公開CIR24条件・通常ABI4artifact byte一致・fmt/clippy。selfhost未移植、stage2/3未測定。[評価](optimize_logs/GLOBAL_CONTEXT_SHARED_PORTAL_EVALUATION_20261008.md)。 |
| Rust experimental固定contextの近接配置・cleanup共通化・兄弟frame共有 | experimental更新（既定OFF） | 小globals／scratchをanchor隣へ戻し、local cleanupをreturn側へ移し、祖先と子孫を分離して兄弟のframe＋private返値bufferを共有。735f1fe比compiler三option ONで固定領域22,923→1,145セル、素BF−33.56%、圧縮BF−13.16%、RLE／native op完全一致。通常ABI比の素BF増加を1.97倍へ縮め、RLE−20.48%を維持。440テスト・実RLE-only diamond回帰・公開CIR24条件・通常ABI4artifact byte一致・fmt/clippy。selfhost未移植、stage2/3未測定。[評価](optimize_logs/GLOBAL_CONTEXT_LAYOUT_EVALUATION_20261008.md)。 |
| Rust experimental固定contextの生成量削減 | experimental更新（既定OFF） | 固定portal PC定数設定・callee近傍scratch・逆順配置・void/aggregateの既知0 Value配送省略。d3f8636比compiler三option ONで素BF−30.96%、圧縮BF−4.40%、RLE−0.84%。全ON execute中央値＋1.89%で時間短縮は未確認。逆順はarena素BF＋0.095%、callee優先は不採用。438テスト・公開CIR24条件・通常ABI4artifact byte一致・fmt/clippy確認。selfhost未移植、stage2/3未測定。[評価](optimize_logs/GLOBAL_CONTEXT_SIZE_EVALUATION_20261008.md)。 |
| Rust experimentalのcall/return nibble・Anchor16併用・固定context直接比較 | experimental更新（既定OFF、既存option内） | S→G引数とG→S返値を分解しlive/重複引数を保存、global引数はstatic側でcopy。bankを固定領域の後ろへ移し併用可能にし、固定contextでも比較guardを実使用。同じ三optionの通常ABI比compiler large64 RLE−19.81%、全ON execute交互5組中央値−43.32%だが圧縮BF＋65.97%・素BF4.30倍。437テスト・24公開CIR条件・実RLE-only counter照合・fmt/clippy確認。配置の費用判定・selfhost移植・全段測定は未実施。[評価](optimize_logs/GLOBAL_CONTEXT_STATIC_FRAMES_EVALUATION_20261008.md)。 |
| Rust experimental固定frameのglobal context化と直接返値配送 | experimental更新（既定OFF） | 閉じた非再帰globalアクセス関数群を固定配置し、共通return inboxを廃止、caller専用resumeで配送。通常ABIのBF不変。arena RLE通常−56.36%・圧縮BF＋2.41%だが素BF2.45倍、compilerは通常RLE−58.06%／nibble＋直接比較時＋39.06%、全ON execute中央値−16.16%／−50.16%。旧flag比RLE悪化も記録。435テスト・公開CIR・counter照合・fmt/clippy、selfhost未移植・全段未測定。[評価](optimize_logs/GLOBAL_CONTEXT_STATIC_FRAMES_EVALUATION_20261008.md)。 |
| Rustの小さい定数strideによるword offset計算 | 採用（既定ON、係数2〜8） | inline後にlowの定数倍Transfer＋最大8 carry閾値へ置換し、0と最初の閾値未満をskip。公開binary CIR対応、unknown low／9以上／alias／生存scratchは汎用経路。4e60088比NodeId[256] wholeのRLE通常−89.76%／併用−86.09%、圧縮BF＋0.41%／＋0.30%。固定compilerはほぼ横ばい（併用large64＋0.0524%）、selfhost未移植・全段未測定。432テスト・全byte・公開CIR・counter・fmt/clippy確認。[記録](optimize_logs/SMALL_SCALED_OFFSET_EVALUATION_20261007.md)。 |
| Rust global portalのcaller復帰PC保持＋局所global復帰選択 | 採用（既定ON） | 4 PC byteのglobal搬送を省き、共有accessor末尾の局所表でcallerへ戻る。route16cells・CIR・inline判断を維持、固定frame／257globals／hidden ID不足は汎用経路。基準40bf36bのlarge64 RLE通常−7.3623%／併用−3.8526%、素BF−28.0017%／−28.3590%、圧縮BF＋1.9676%／＋1.4560%、native op＋8.4771%／−5.9192%。全ON execute交互5組の中央値は通常−1.32%（範囲重なり）／併用−7.68%。425テスト・fmt/clippy・公開CIR28条件・全256global番号を検証。別dispatch復帰／accessor全面複製は撤回、selfhost未移植・全段未測定。[記録](optimize_logs/GLOBAL_PORTAL_PC_EVALUATION_20261007.md)。 |
| Rust global portal offset highのnibble搬送 | 採用（既存nibbleフラグ内） | 既存route scratchでhighも2桁に分解し、大arenaの値依存往復を抑える。PC保持単独比large64 RLE−4.6941%／medium＋0.3791%、native＋0.5353%、素BF＋13.7260%。今回全体では基準40bf36b比併用large64 RLE−8.3658%、scan−39.6058%、素BF−18.5256%、圧縮BF＋1.7360%、native−5.4156%、全ON execute交互5組中央値−6.8802%。通常BFはPC保持単独とbyte不変。全byte輸送・profile・425テスト・fmt/clippy・公開CIR28条件を確認、selfhost未移植・全段未測定。[記録](optimize_logs/GLOBAL_PORTAL_PC_EVALUATION_20261007.md)。 |
| Rustのregion分岐条件のslide消費 | 採用（既存直接比較フラグ内） | private比較guardを持つconditionで分岐を開始し、body後はflag=0で閉じて`>[>]`で位置を合わせ、専用then gateへの退避moveを省く。条件slotの後継再利用・入れ子guard再利用を保護し、frame幅追加なし。基準fb7e184のlarge64併用RLE−0.03782%、transfer反復−214,244、native op−187,422、圧縮BF−9,474 bytes（−0.24860%）。通常BF／counter不変。全byte・soft cycle・全ON/RLE-only論理counter・公開CIR28条件・424テスト・fmt/clippy確認、実時間／全段未測定・selfhost未移植。[記録](optimize_logs/BRANCH_SLIDE_EVALUATION_20261007.md)。 |
| RustのCFG境界を越えるcall引数／returnのcopy参照 | 採用（既定ON） | caller localの未変更sourceを合流・loop・call/portal resume越えで追跡し、scalar／連続16field以下のstaging copyをDSE。globals・ABIセル・書込み・破壊的guardを保護、sourceと公開binary CIRに適用、inline判断は維持。基準66959e5のlarge64 RLE通常−1.2452%／nibble＋直接比較＋Anchor16−0.8105%、transfer反復−2,456,979／−2,470,759、native op−0.2823%／−0.0938%、圧縮BF−1.1623%／−1.0371%。広いbody/portal forwardはscan増で撤回。全byte差分・公開CIR28条件・424テスト・fmt/clippy確認、実時間／全段未測定・selfhost未移植。[記録](optimize_logs/BOUNDARY_COPY_EVALUATION_20261007.md)。 |
| Rustの静的local aggregateのfield分解・寿命・配置 | 採用（既定ON） | 16cells以下でportal・aggregate ABIを使わないlocal regionをscalarへ分解し、dead field書込みと中間計算を除去、graphと割当を適用。全field copyでの完全上書きを認識し、inline判断後のみ分解。基準c47d7f6のlarge64 RLEは通常−9.2283%／nibble＋直接比較＋Anchor16−0.9777%、transfer反復−245,900／−322,404、native op＋1.4958%／＋0.6055%、圧縮BF＋0.7537%／＋0.4908%（展開量は減少）。全ON execute中央値は通常＋2.97%／併用＋0.365%、時間短縮未確認。420テスト・fmt/clippy・公開CIR28条件成功。frame幅だけのfallbackは併用RLE悪化で撤回、selfhost未移植・全段未測定。[記録](optimize_logs/AGGREGATE_FIELDS_EVALUATION_20261007.md)。 |
| Rustの直後aggregate returnへのsource直接搬送 | 採用（既定ON） | FrameへのAggregateCopy／公開binary CIRのCopyOutbox leaf列が直後returnでだけ使われる場合、中継copyを省く。部分範囲・self-copyに対応し、source変更・hole・重複・同regionの重なりでは従来経路。CIR/frame/inline判断を変えず、region/direct/fixedで共通適用。基準46779faのlarge64でtransfer反復−103,298、論理RLEは通常−0.002556%／nibble＋直接比較＋Anchor16−0.014807%、native−12,404、圧縮BF−1,987 bytes。graphへのaggregate拡張三案は悪化して撤回。417テスト・fmt/clippy・公開CIR28条件・全byte differentialを確認。実時間・全段時間未測定、selfhost未移植。[記録](optimize_logs/AGGREGATE_COPY_EVALUATION_20261007.md)。 |
| Rust page往復selectorの直接設定 | 採用（既定ON） | high byteを消費するcountdownの到達levelで往復4桁を一度だけ設定し、各levelの桁更新と帰路copyを除去。ABI/frame/scratch追加なし。基準b598b4dのlarge64でtransfer反復−1,975,459、論理RLEは通常−0.06391%／nibble＋直接比較＋Anchor16−0.36912%、native−2.40530%／−0.95804%。圧縮BF＋10,620 bytes（＋0.31%／＋0.28%）、通常BF＋31,314命令（＋0.0044%／＋0.0037%）。全ON execute各5組の中央値は通常0.943→0.922秒／併用1.850→1.811秒（約−2%、範囲は重なる）。全256値・dirty scratch・65536-cell page境界・固定frame・公開CIR28条件、415テスト・fmt/clippy確認。selfhost未移植・全段時間未測定。[記録](optimize_logs/PAGE_SELECTOR_EVALUATION_20261007.md)。 |
| Rustの定数等値判定・portal high判定のslide化 | 採用（定数比較は既存直接比較フラグ内、portalは既定ON） | 生存Frame sourceを一時調整して既存guardで判定しbody前に復元、portalはhighを保存して隣接scratchで直接判定。frame/scratch追加なし。基準83adfabのlarge64で論理RLEは通常−0.2269%、nibble＋直接比較＋Anchor16で−1.3733%（portal単独−1.2953%、定数比較単独−0.0780%）。併用のnative−0.1064%、圧縮BF−0.0696%、通常BF−0.0467%。全ON execute各5組の中央値は通常＋0.1087%／併用＋0.2362%で改善未確認。全byte/value pair・guard・alias・公開CIR28条件、413テスト・fmt/clippyを確認。selfhost未移植・全段時間未測定。[記録](optimize_logs/RUST_SLIDE_TRUTH_ADOPTION_20261007.md)。 |
| Rustのnibble byte分解をslide型incrementへ置換 | 採用（既存nibbleフラグ内） | route要求・global copy・portal返値の旧bit→carry退避を`[>]++[-<]`で置換し、既存9-cell scratchを維持。固定compilerのlarge64・nibble＋直接比較＋Anchor16でRLE op−5.01%、native op−13.69%、圧縮BF−3.16%；全ON execute中央値（各5回）は1.951→1.783秒（−8.61%）。255の分解＋packは5,559→3,846 RLE命令、通常設定のBFはbyte不変。全256値・guard・再帰・公開CIR、410テスト・fmt/clippyを確認。selfhost移植・stage3全段時間は未測定。[記録](optimize_logs/OFFSET_SLIDE_EVALUATION_20261007.md)。 |
| Rustのglobal portal返値nibble搬送 | 採用（既存nibbleフラグ内） | 返値を消費して既存static scratchで2桁搬送。固定compilerのlarge64・nibble＋直接比較＋Anchor16で単独RLE op−5.53%、native op＋1.42%、圧縮BF＋0.433%。source/scratchのゼロ、全256値・再帰・複数leaf・公開CIRを照合。通常設定のBFは不変。409テスト・fmt/clippy成功、stage3時間未測定。[記録](optimize_logs/PORTAL_RETURN_OFFSET_EVALUATION_20261006.md)。 |
| Rustのportal offset field直接参照 | 採用（既定ON） | single-cell Globalアクセス直前の死んだfield→temporary Copyを除去し、aliasしないfieldをbackendで消費。native・公開binary CIRの両経路。固定compilerの32 continuationで64 Copy除去、通常large64のRLE op−0.548%、native op−0.352%。返値nibbleとの併用は基準比RLE−8.27%、native＋1.30%、圧縮BF＋0.40%、素BF＋0.0103%。live値・payload/Frame alias・コピー依存を保護。NodeId 3field共有は素BF＋35.74%等のため未採用。409テスト・fmt/clippy成功。[記録](optimize_logs/PORTAL_RETURN_OFFSET_EVALUATION_20261006.md)。 |
| Rustの保存carrierへのscalar値配置引継ぎ | 採用（既定ON） | graph後の`Transfer B→…,T; Copy T→B`に限定して復元先を新しい論理slotにし、後続の読み書きをcarrierへ向ける。比較/borrow/制御guardは固定、CFG境界ではlive値だけ元へ戻す。native・公開binary CIRに適用し、追加DSEと割当後の自己Copy除去を行う。小例のRLE op−19〜22%；固定compilerでは通常large64−0.00177%、nibble＋直接比較＋Anchor16−0.00921%で、削減は追加DSEだけと同程度。定義全般・Copy全般を分ける版は採用しない。本体のBF・全counter・出力は試作に一致、402テスト・fmt/clippyを確認。selfhost未移植・全段時間未測定。[記録](optimize_logs/VALUE_PLACEMENT_EVALUATION_20261006.md)。 |
| Rustの局所計算グラフによるscalar搬送再構成 | 採用（既定ON） | Frame scalar/static fieldのCopy・定数・加減算を区間全体で追跡し、相殺とfan-outを集約。source・公開binary CIRの両経路。固定compilerのmedium/large64で論理RLE opは通常−3.12%/−2.40%、nibble＋直接比較＋Anchor16で−10.61%/−11.03%；inline固定でもlarge64−11.00%。併用の圧縮BF量−2.74%、native op−0.69%、Rust生成中央値＋2.88%。128命令/16項の上限とscratch再利用を保持。本体のBF・counter・出力は試作に一致、399テスト・fmt/clippyを確認。selfhost未移植・全段時間未測定。[記録](optimize_logs/COMPUTATION_GRAPH_EVALUATION_20261006.md)。 |
| Rustの隣接定数Transfer畳み込み | 採用（既定ON） | localのSet→Transferをsource=0とwrap付きAddConstへ置換する軽いCIRパス。source・公開binary CIRの両経路、構造化bodyに適用。portal最適化採用後の固定compiler BFで、medium/large64の論理RLE opは通常−0.254%/−0.082%、nibble＋直接比較＋Anchor16で−0.934%/−0.371%。併用の圧縮BF量−0.854%。全256定数・fan-out・wrap・公開CIR 12条件・profile・393テスト・fmt/clippyを確認。selfhostには未移植。[記録](optimize_logs/RUST_CONSTANT_TRANSFER_TRUTH_COPY_20261006.md)。 |
| Rustの分岐用copyを既存guardで非破壊判定 | 採用（直接比較フラグ内） | 直後の構造化Branchが消費するFrame→Frameの保存copyのみ。比較slotの既存guardを再利用し、frame幅を追加しない。定数Transfer単独から併用のmedium/large64でRLE−1.232%/−0.175%、両変更の合計はportal採用後比−2.155%/−0.545%。圧縮BF量−1.121%、native opは＋0.154%/＋0.029%。default・flat aggregate・固定frame・死ぬsourceは従来経路。全byte・入れ子・guard再利用・自己copy・condition上書きを照合。native実時間・全段時間は未測定。[記録](optimize_logs/RUST_CONSTANT_TRANSFER_TRUTH_COPY_20261006.md)。 |
| Rustのportal最終使用消費＋page digit一括jump | 採用（既定ON） | 死ぬFrame scalar添字・store値とprivate bufferを消費し、page間搬送を共有accessorの定数距離jumpへ変更。配列幅で分岐表を縮め、表外はwalkへ戻す。寿命最適化採用後の固定compiler BFでmedium/large64の論理RLE opは通常−3.60%/−4.50%、nibble＋直接比較＋Anchor16で−12.08%/−17.60%。large64の全ON execute中央値（各3組）は通常1.048→1.007秒、併用2.229→2.183秒。併用の素BF量＋3.11%、圧縮量＋0.092%。公開CIR 12条件・profile・390テスト・fmt/clippyを確認。selfhost移植・全段時間は未測定。[取り込み記録](optimize_logs/RUST_PORTAL_TRANSFER_ADOPTION_20261006.md)。 |
| Rustの寿命に基づくcopy保存省略＋全体aggregate copyの領域共有 | 採用（既定ON） | scalar/aggregate copy・call引数・returnで死んだlocalを消費し、同幅の全体コピーを安全に共有。global・部分的なlive値・重複引数は保存。固定compiler BFのmedium/large64で通常設定の論理RLE op−20.09%/−21.32%、native op−21.32%/−26.04%、全ON execute中央値（各3回）−18.93%/−22.50%、BF量−18.58%。nibble＋直接比較＋Anchor16でもRLE−38.98%/−41.63%で試作と全counter一致。公開CIR・profile/embedded・388テスト・clippyを確認。加算の並び専用変換は採用しない。selfhostへの移植・全段時間は未検証。[取り込み記録](optimize_logs/RUST_LIFETIME_COPY_ADOPTION_20261006.md)。 |
| Rustの比較slot限定直接比較＋Anchor16 | 採用（独立フラグ・既定OFF） | `--enable-inplace-compare`/`--enable-anchor-bank`、nibbleと併用可。同じCIRのmedium/large64で併用の論理RLE opは通常56.40%/61.65%、nibble35.63%/45.90%減、通常BF量は2.47%/2.01%増。全ONのnative時間は悪化。公開CIRにはAnchor16を適用し、flat aggregateの直接比較は従来方式。全byte pair・alias・再帰位相・配列/call・出力形式・容量境界を検証。[本体取り込み](optimize_logs/RUST_COMPARE_ANCHOR_ADOPTION_20261005.md)。 |
| selfhostの選択式nibble搬送 | 採用（既定OFF） | concatの`--enable-nibble-transfer`でglobal copy・portal offset low/payloadを分解。frame幅を保ち、static scratchを6 cell追加し、load結果はpage headerで直接分解。全256値・再帰・BF/CIR・出力形式を照合。RLE op数を減らす選択肢で、全ONの時間改善は前提にしない。 |
| interpreterの非同期比較idiomの一括化 | 採用 | 全256×256入力で出力・raw/RLE命令数を照合。実行時の補助セルと境界を検査し、不成立時は通常実行。full28と同じcompiler BFの短いwide-globalコンパイルでsample実行時間9.28%減。 |
| clear loopの命令数を逆元で計算 | 採用 | 全256入力×128奇数増分で従来の反復数と一致。比較一括化との合計で短いwide-globalコンパイルを12.28%短縮（profileなし）。full自己入力は再測定していない。 |
| interpreterの進捗処理分離とsite遅延取得 | 採用 | 進捗有無・countdown判定をinline化し、時計・通知処理をcold関数へ分離。非profile実行ではpoll時だけsiteを取得し、16,384回のpoll間隔・中断snapshot・raw/RLE/native counterを維持。通常release・圧縮BFの各3組の試作比較で全ON・2,788-byte入力のexecute中央値6.403→4.316秒、RLE-only・583-byte入力20.366→15.410秒。本体でも通常BF/圧縮BF・RLE-only/全ONの出力と全counter一致、workspace 375テストpass。[perf測定と取り込み記録](optimize_logs/INTERPRETER_PERF_RESULTS.md)。stage3全体の時間は未測定。 |
| interpreterのcounter配置変更とBF decoder inline化 | 採用 | privateなMachineのraw/RLE counterをoptimization statsで離し、Runs::next/run_atをinline化。配置の理由はコードコメントに保存。通常release・各3組の併用比較で圧縮BFのRLE-only execute中央値15.846→11.243秒、全ON・2,788-byte入力4.627→4.061秒、通常BFのparse 4.167→2.106秒。本体でも通常BF/圧縮BF・RLE-only/全ONの5実行で出力と全counter一致、workspace 375テストpass。CPU/LLVM依存の改善で、stage3全体は未測定。[測定と取り込み記録](optimize_logs/INTERPRETER_NEXT_OPTIMIZATIONS.md)。 |
| interpreterの通常BF一括parse・RLE-only専用実行・Move countの距離利用 | 採用 | 通常BFの連続同一命令を一括読みし、物理offsetとprofile分割を維持。非profile・RLE-onlyでは24-byte payloadとjump先で再帰call・命令の二重参照を省き、局所counterをpoll前と終了時に保存。実行ループのinlineを抑制。通常経路のMoveも同方向の距離からraw countを求める。通常release・各3組の併用比較で通常BF/RLE-onlyのexecute中央値11.686→7.674秒、parse 2.105→0.213秒、全ON・2,788-byte入力4.209→4.011秒。BF出力・全counter一致、workspace 381テストpass・clippy成功。生成BFは不変、追加実行IRで圧縮RLE-onlyのpeak RSS 96.6→162.4MiB。stage3全体は未測定。[測定・取り込み記録](optimize_logs/INTERPRETER_RLE_EXECUTION_AND_PARSER.md)。 |
| emit_move_toの同一位置での早期return | 試作のみ・不採用 | 4入力で出力一致したがnative operationsが0.85〜1.09%増加。追加の等値比較・制御の費用が上回った。 |
| emit_hex_byteのwhile化 | 試作のみ・保留 | 4入力で出力一致、native operations削減は0.024〜0.182%。速度改善は確認できずproductionに残していない。emit_repeat_characterの既存whileは維持。 |
| 空Goto threading・到達不能除去・ID compaction | 採用 | source/CIRで出力一致、helloのprocess wall短縮。主効果はdispatcher往復削減。full selfhost改善は未確認。 |
| continuation除去のみでIDを詰めない案 | 不採用 | 疎なpageがequality scanを選び、生成BFが増大した。密なID配置を維持する。 |
| arena_advanceの桁上がり付き加算 | 不採用・試作撤回 | 境界とselfhost検証は通過したが、helloのraw/RLE換算命令とprocess wallが悪化。native operation削減だけでは採用しない。 |
| 同一関数の非空Branch後継inline化（2c） | 再評価後に採用 | 当初の少数wall測定による不採用をpaired比較で見直した。source/CIR複数ケースでBF execute短縮を確認。生成BF増加を伴う。全入力で差を識別できたわけではなく、end-to-end/full selfhost改善は未確認。 |
| 局所CFG構造化（2d） | 採用 | source/CIRの複数入力でBF execute短縮と出力一致を確認。call・portal等の境界を維持し、通常loweringで有効化。frame guard追加とID配置変化を伴うため、削減全体をloop化だけの効果とは解釈しない。full selfhost時間の改善は未確認。 |
| selfhostのlocal if/while保持 | 採用 | BF出力でdispatcher往復を省略し、call・return・portal・239-cell上限は従来経路へ戻す。試作の通常BF・583-byte入力でexecute中央値はRLE-only 29.24→19.01秒、全ON 4.95→1.24秒、compiler BFは約0.09%増。compressed＋全ONのstage2＋stage3は20分49→20分38秒（各1回で改善断定不可）。公開CIR v2でもLoopへ変換し、旧v1は読込可能。通常BF・RLE-onlyの全段時間は未測定。 |
| selfhostの構築時Goto省略 | 採用（Goto単独） | ifの生きた終端・空の終端とwhileの空入口を再利用し、tailを復元して後続命令を追加。全CIR走査・命令複製なし。BF/公開CIRでreturn・空の枝・入れ子・loop入口を検証。固定compiler入力のcontinuationは5,948→5,792、empty Gotoは219→63、命令数23,420は不変。通常BF・583-byte入力のexecute中央値（各3回）はRLE-only 18.806→18.668秒、全ON 1.226→1.179秒、動的RLE−0.64%、compiler BF＋0.020%。追加実装の自己コンパイル費用を踏まえ、semantic定数畳み込みと定数条件除去は外した。[Goto単独の取り込み記録](optimize_logs/SELFHOST_GOTO_ONLY_ADOPTION.md)。Goto単独の全段時間は未測定。 |
| IR継続・遷移・終端・phase・portal集計 | 採用 | 出力・通常counter一致とaccounting、source/CIR測定を確認。計測費用があるためオプトイン。BF hidden dispatch/navigation費用は測れていない。 |
| BFCRLE v1テキスト圧縮 | 採用（オプトイン） | Rust版出力のSSD書込量を削減。通常BF互換を維持し、profileの展開後ordinal/identityとinline markerを保つ。セルフホスト版にもcompressedエントリを追加し、通常mainとbinary CIRを維持。 |
| interpreter RemoteTransfer | 採用 | BFの意味から経路不変性を実行時検証してScan往復を一括転送へ置換。source由来BFの複数入力で出力・論理counter一致とexecute短縮を確認。不成立時は通常実行。CIR/full selfhost時間比較は未実施。 |
| frame/global byte搬送のunary既定化 | 採用 | RemoteTransfer有効時のコンパクトケースでexecute短縮を確認。nibbleはBF命令数を抑える選択肢として`--enable-nibble-transfer`に保持。offset/window移動・ABI配置は維持。full selfhost速度は未測定。 |
| stage2 BF最適化器のcount読み書き削減 | 撤回（最適化器ごと撤去） | token kindに応じたlaneだけを読み書きし、相殺時の書き戻しを除去。最適化規則と生成出力は維持。小入力のbyte単位portal要求削減とIR/BF照合を確認。 |
| HIRの局所Frame lowering | 採用 | 単純なローカルwhileの非ゼロ条件・Output・定数更新を直接Frame命令にし、割当て前に不要な一時セルと条件の0/1化を除く。emit_repeat_256の反復は通常BFの最小形となり出力ベンチを短縮。複雑な制御は従来経路、多箇所inlineは未拡張。 |
| Frame Compareと非同期分岐による比較 | 採用 | source/binary CIRの比較をABI loweringまで保持し、反復内のoperand複製を省く。全8-bit入力・両ABI配置を検証。比較ベンチの時間短縮を確認したが、付随する既存inline/CFG簡約も含む。一部生成BFサイズは増加。full selfhost時間は未測定。 |
| 連続copyのrestore共有とzero destination | 採用 | aggregate、aggregate引数/戻り値、portalの連続copyではrestoreを列の先頭で一度だけclearする。新規callee frameの引数領域と、直前にclearしたportal protocol fieldではdestination clearも省く。32-cell引数/戻り値fixtureでnative 20.2%、RLE 21.7%、raw executed 23.0%減、出力一致。full selfhost全体の速度向上率は未測定。 |
| dead sourceのCopyを破壊的Transferへ変換 | 不採用・撤回 | 後続で読み取りなしに上書きされる局所Copyだけを変換。compiler artifactは983 bytes減ったが、代表compiler実行の統計は不変だった。 |
| boolean Compareのzero scratch再利用 | 採用 | countdown後にzeroの`E`をfalse値のmaterializeへ再利用し、`false=0`ではless側のsetも省く。全8-bit入力・ABI配置と代表selfhost入力の出力一致を確認。artifact 1,144 bytes減、代表native削減は0.028–0.033%で、比較コスト全体を解消するものではない。 |
| wide serializerの補数側比較 | 採用 | `K>128`の`x<K; x-=K`をwrap後の小さい補数比較へ変形。0〜999のserializer fixtureで出力一致、native 10.70%、RLE 12.81%、raw 5.04%減。代表stage2例の統計差はなく、wide経路限定の改善。 |
| global copy内のdecimal digit再利用 | 保留 | 距離桁を6往復で共有する候補。出力一致、wide-global合成例でnative 0.76%減だが、代表例は±0.04%程度でartifact約37KB増。 |
| transfer内部の既知位置展開 | 不採用・撤回 | source/destination往復の`emit_move_to`を直接展開。stage8出力は一致したが、native 0.88%、RLE 1.07%、execute 5.3%悪化。 |
| portal windowのstage-aware zero lane | 採用 | D=16の往路・復路で消費済みnibble counterを後続stageのzero laneへ追加。異なる値を持つ4096セル相当のglobal配列を複数chunk・長距離offsetで検証して出力一致。global-largeではBF 185,487→184,673 bytes、同一入力の1 requestあたりnative 16.2%、RLE 11.9%減。load/storeのpayload laneは不変条件が不足するため追加していない。 |
| global copyのstatic-side restore scratch | 不採用・撤回 | global→frame scalar copyのrestoreだけをD=16 static scratchへ移した。出力は一致したが、global-triple BF 35,330→36,674 bytes、global-large 184,673→221,538 bytesとなり、static navigationが増加分を上回った。 |
| portal nibbleの固定距離dispatch | 不採用・撤回 | nibble値ごとのlocal equality dispatchで、値ごとに一回の`value*stride`交換を試した。長い異値配列の出力は一致したが、heterogeneous fixtureのBF 21,613→83,003 bytes、global-large 184,673→307,453 bytes。4-bit popcount分解はこの比較表を使わない別実装が必要で、productionには残していない。 |
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

### 大小比較をFrame Compareとして保持

sourceのHIR loweringとbinary CIR adapterで、大小比較の意味を保持する
`FrameInstruction::Compare`を生成する。入力2値のsnapshotをunsigned比較し、
両operandを0にしてから指定した結果値をdstへ書く。operand/dstのaliasを許す。
CIR VM、validation、slot allocationのread/write集合とremappingを対応させた。

BF backendは既存ABI Scratch0..3の4連続セルを使う。比較反復の非ゼロ判定を、
入口と出口のpointer位置が異なる非同期分岐で実装し、各反復のoperand複製・復元を除く。
作業セルは出口ですべて0、context originへ復帰。D=8/D=16の両配置を扱う。
通常BFの意味だけを使用し、interpreterやBFCRLE形式は変更しない。
binary CIRでは逆向き比較のswapと、結果を反転する追加boolean化も省く。

Tape IR、汎用非破壊Branch、十進変換の定数特殊化、多箇所inlineは追加していない。
BF/mapは再生成が必要。IR artifact identityをv4へ更新し、phase設定も再生成する。

### 検証と測定

- `comparison_lowering.rs`: sourceの全256×256組の4種大小比較、元値保存、
  local CFG構造化ON/OFF、D=8/D=16でIR/BF結果を確認。
- binary CIRも全256×256組・4比較・D=8/D=16を確認。
- public Compareのframe/global/AbiValue間alias、任意の真偽結果値、
  operandのclearとdst書込み順を確認。
- `cargo test --release --workspace`成功（286件）。
  ログ: `tmp/compare-tests-final.log`。
- `scripts/verify-stage2-selfhost.sh`成功。
  ログ: `tmp/compare-stage2.log`。
- `scripts/verify-selfhost-compressed.py`成功。全18例の展開後命令列一致と、
  反復数・十進境界のIR/BF結果一致を確認。
  ログ: `tmp/compare-compressed-verification.log`。
- 比較ベンチ: `scripts/comparison-lowering/run.py`。
  変更前9884870のbinaryとcandidateを同じinterpreterでAB/BA交互5組測定。
  全ペアの4比較とoperand出力を含む。時間はstats有効のprocess wall（execute専用ではない）。

| 比較ベンチ | 変更前 | 変更後 |
|---|---:|---:|
| process wall中央値 | 2.579 s | 0.962 s |
| native operations | 567,770,227 | 284,373,322 |
| raw BF命令換算 | 55,662,509,210 | 12,779,247,956 |
| RLE命令換算 | 32,086,307,245 | 1,864,472,662 |
| 通常BF source bytes | 5,970 | 18,321 |
| max pointer | 93 | 59 |

出力393,216 bytesは期待値と一致。ログ: `tmp/compare-pairs-bench.log`。
比較loweringによるframe縮小で既存の単一使用void inlineが成立し、
このfixtureは2関数7continuationから1関数1continuationにも変化する。
したがって約2.68倍の速度は比較template単体の効果ではない。
同時にsource bytesは約3.07倍へ増えるため、サイズが必ず縮むとは主張しない。

full9と同じセルフホストsourceから実行用BFを生成し、
hello / stage8_aggregates入力を同じinterpreterでAB/BA交互4組比較した。
生成結果は両variantでbyte一致した。process wall中央値はhello 0.839→0.730 s、
aggregates 1.598→1.573 s。特に後者は小差であり強い速度改善の根拠にはしない。
実行用compressed BFは6,295,086→6,289,632 bytes。
ログ: `tmp/compare-bench.log`。比較以外の配置・continuation変化も含む。

full selfhostを再完走しての速度測定は未実施。
セルフホストBFC側のBF生成アルゴリズムは変えていないため、
full9の出力命令列が5.5 TB相当になる問題を解決した変更ではない。

次のコミットではこの作業結果節を置き換え、採否・理由・制約を保持する。

## Portal詳細profile・コンパクト再現（2026-09-15）

**診断基盤を採用。ABI、nibble搬送、RemoteTransferの実装は変更していない。**
現状のproductionコードから短時間の再現を作れることと、搬送前の分解・要素選択・window交換を
別々に観測できることを確認した。profile付き生成BFは変更前のRust compilerとbyte単位で一致した。

### 測定条件

`tmp/portal-profile-evaluation`に12ケースの結果、`tmp/portal-profile-values`に搬送値を変えた
6ケースの結果を保存した。各directoryの`manifest.json`、`runner.py`、source/input、BF/map、
raw profile、`summary.json`と`report.md`が再現情報である。
通常測定はprofileなし、1回warmup後5組のON/OFF交互実行の中央値。前者は遅い設定で約2秒、
後者は約0.5秒を目安に校正した。各processは60秒timeout。
Rust/interpreterはrelease build、D=16、unlimited tape、同一BF・入力を使用。
sample/countersは別実行。compile・parse・RSSと全runの値も記録した。
長時間のcompiler自己入力は実行していない。

### 得られた内訳

| ケース | 圧縮BF bytes | RemoteTransfer ONの主なexclusive sample |
| --- | ---: | --- |
| 7byte要求搬送、padding 16セル | 1,263 | bit分解93.4%、搬送3.0% |
| 7byte要求搬送、padding 256セル | 1,368 | bit分解92.7%、搬送3.1% |
| 16要素×3セルのglobal配列 | 35,330 | 分解23.3%、要素選択20.2%、window交換15.0% |
| 16要素×3セルのframe配列 | 30,676 | 要素選択32.3%、window交換20.8%、offset12.3% |
| 大きいglobal配列・chunk境界 | 183,678 | window交換43.4%、分解23.3%、offset10.4% |
| production optimizerの抜粋 | 99,517 | dispatch38.5%、分解17.6%、要素選択9.6%、window交換4.7% |

共有祖先へ融合した命令の費用は親へ残した。表はsubsetであり、残余を子へ推測配賦していない。
また、異なるケースのsample割合は分母が違うため、速度比として比較しない。

7byteの搬送fixtureでは各要求がnibble 6本＋unary 4本の計10回のRemoteTransferとなり、
全256値・両paddingでnative site帰属をテストした。測定した全ケースでfallbackは0だった。
これは「RemoteTransferになっていないため遅い」という説明を支持しない。
分解loopは転送とは別に残り、準備が主な費用になる条件を約1.3KBのBFで再現できた。

要求byteを7本とも同じ値にした場合の、profileなしのwhole-fixture平均は次のとおり。
初期化・観測・後処理も含むため、routerだけのlatencyではない。

| byte値 | ON ns/要求 | OFF ns/要求 |
| --- | ---: | ---: |
| 0 | 639 | 613 |
| 1 | 1,154 | 933 |
| 15 | 2,995 | 5,742 |
| 16 | 3,369 | 4,916 |
| 127 | 19,350 | 34,887 |
| 255 | 37,715 | 69,707 |

大きい値ではRemoteTransferの改善後も分解費用が残る。小さい値ではprobeの固定費用もあり、
必ずONが速いとは限らない。この表は当該runの中央値であり、実際の要求値分布を代替しない。

### 判断と限界

- この時点では次の独立実験としてrouterのbit分解を省く搬送のA/Bを優先した。
  単純転送のnative化との優劣はこの測定だけでは確定せず、2026-09-16の独立比較で判断した。
- 小配列では16通りの要素選択も費用が大きい。小配列専用の選択やaggregate一括転送を
  次の候補として維持する。大配列ではwindow交換の比重が高く、別の条件で評価する。
- frame配列にはglobal routerがない。stackに置いても同じ費用になる、という説明は不正確。
  一方で要素選択・window交換は残るため、関数の固定領域化だけで配列の費用が消えるわけでもない。
- optimizerの抜粋はfull14とdispatch規模・PC値・call stack・仕事の割合が異なる。
  primitive fixtureの制御値はPCとしてdispatchされない。full14と同じ比率や改善率は主張しない。
- 全source fixtureをIR出力と照合し、配列のIR load/store数、全初期化payloadとcaller sentinelの保存を確認。
  ON/OFFの出力・logical BF/RLE count・max pointerも一致した。
- `cargo test --workspace`、全targetのClippy、profile追加前とのBF一致が通過した。

## Byte搬送をunary既定へ変更（2026-09-16）

**採用。Rust backendのnibble搬送を`--enable-nibble-transfer`で選択する方式にした。**
global scalarからframeへのcopy、およびglobal routerのoffset/accessor/resume low byteが対象。
source/CIR、通常BF/圧縮BF、profile付き出力で共通に指定する。
offset分解、base-16 window移動、frame/static配置とscratch予約は維持した。
stage2のBFC製backendは変更していない。

### full15の観測と判断

以下は変更前のfull15を読んだ時点の記録。後の実験で`full15.metrics`が上書きされたため、
現在の同名ファイルとは一致しない。独立A/Bのartifactは別directoryに保存している。

`full15.metrics`の210〜220分の差分では、routerの複数のnibble/unary siteが
それぞれ約2.7〜3.0万sample増え、global navigationも約1.7万増えている。
序盤のdecompose/exchangeだけを改善して終わる説明では不十分である。
ただし保存されているのは累積top10表示で、最終profile JSONはない。
ここから全siteの割合やnibble有無の実行時間差を確定してはいけない。

RemoteTransferはunary loopも一括実行できる。一方、nibble化は分解処理を追加し、
対象byteの転送を二つのloopに分ける。global要求7byteでは計10本から7本へ減らせる。
各loopに経路探索が必要なため、分解だけでなく長いstackでの搬送も比較した。
RemoteTransferのない実行では値による往復回数の削減が有利になる条件が残る。

### 独立したA/B測定

次のコマンドで測定した。

```sh
python3 scripts/portal-profile/compare-transfer.py tmp/nibble-transfer-evaluation \
  --baseline-compiler tmp/nibble-transfer-baseline/bfc
```

release build、unlimited tape、profileなし、warmup後5組のAB/BA交互実行。
RemoteTransfer ON/OFFを別々に校正し、それぞれの遅いvariantを約1秒にする。
同じ組の入力・反復数は同一。表は実行時間の中央値を入力record数で割った値であり、
初期化・観測・後処理も含む。長いstackは有効なframeに4,064個のlive caller chunkを追加したfixture。

| ケース | ON: nibble ns/record | ON: unary ns/record | ON時間変化 | OFF時間変化 |
| --- | ---: | ---: | ---: | ---: |
| global scalar snapshot | 13,744 | 874 | -93.6% | +16.7% |
| production optimizer抜粋 | 14,307,067 | 10,839,705 | -24.2% | -8.7% |
| global 16要素×3セル | 45,419 | 31,799 | -30.0% | -8.4% |
| 同配列・再帰8段 | 49,100 | 35,412 | -27.9% | -3.2% |
| 大きいglobal配列・chunk境界 | 42,125 | 30,885 | -26.7% | -9.1% |
| 搬送単体・padding 16 | 20,453 | 987 | -95.2% | -20.1% |
| 搬送単体・長いlive stack | 77,129 | 47,762 | -38.1% | +52.1% |

変化はnibbleを基準とし、負がunaryの短縮。全7ケースのONでpaired差のbootstrap 95%区間も
負になった。全source fixtureのnibble有効BFは変更前7367a75のcompilerとbyte単位で一致した。
通常のportal単体は1要求7本/10本のRemoteTransferとなることを全256値で検証。
長いfixtureは両方式ともrunあたり1回のfallbackを含み、それも時間・counterへ含めた。
生成BFサイズはoptimizerが99,517→94,952 bytes、小さい搬送単体が1,263→436 bytes。

### 検証と制約

- `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`が通過。
- source/CIRのglobal snapshot・動的配列アクセスを両方式・両RemoteTransfer設定で照合。
  通常/圧縮BF、profile/embedded mapのBF identity一致と、flagで生成BFが変わることを検証。
- 搬送単体の全256値、短い/長めのframe、native site帰属を両方式で検証。
- 測定は毎runの出力をIRまたは制御byte列と照合した。同じBFのRemoteTransfer ON/OFFで
  pilotのlogical BF/RLE命令数と最大pointerも一致した。
- `tmp/nibble-transfer-evaluation`に全pair、BF/map、入力/期待値、binary/source hash、script snapshot、
  bootstrap区間を保存。sample/countersの別診断は`tmp/nibble-transfer-diagnostics-unary`と
  `tmp/nibble-transfer-diagnostics-nibble`に保存した。診断の短い測定時間は速度判断に使わない。
- RemoteTransferがなくても、小さい値・短い距離では分解費用が勝つ場合があるため、
  「なしならnibbleが常に速い」とはしない。BF命令数とnative実行時間も区別する。
- full15のphase分布とfull selfhost改善率は未検証。長時間のcompiler自己入力は実行していない。
  navigationやwindow交換自体は残り、portal往復削減・専用laneは引き続き別の課題である。

## stage2 BF最適化器の不要なcountアクセス削減（2026-09-16、撤去前の測定）

`09_bf_optimizer.bfc`で、合併する分岐に入ってから必要なcountを読むよう変更した。
加算・奇偶判定はlow byteだけ、pointer移動は3byte、括弧等はcountを使わない。
書込みも同じkind契約に限定し、相殺されたtokenと変化しないkindへの書き戻しを省く。
最適化規則、16 tokenの容量、生成BFの形式は維持する。slotの未使用laneには古い値が残りうる。

`tmp/portal-next-review`に変更前/後のoptimizer抜粋sourceと生成BF、入力・出力、IR metricsを保存した。
production fixtureの4 blockで、実行されたIR load/storeのcellsを合計すると、
byte単位portal要求はload 2,008→1,020、store 1,324→764、合計3,332→1,784（46.5%減）。
この入力の最適化済み出力184 bytesは一致した。
80 blockを同じRemoteTransfer有効のinterpreterで、profileなし・warmup後5組AB/BA比較した参考値は、
execute中央値840→688 ms（約18%短縮）。全runの出力一致を確認した。
ログは`io-benchmark.*.log`、全測定値は`io-benchmark.json`、source/BF/binary/input hashは`io-manifest.json`。
長時間のcompiler自己入力やfull phaseの速度比較は実行していない。

`verify-stage2-bf-optimizer.py`は580ケースのIR/BF・通常/圧縮の照合と50プログラムの意味比較が通過。
全count laneを使った後にring slotを再利用する3ケースも、期待する命令列と照合する。
logは`tmp/portal-next-review/verification.log`。
二段portal配置・小配列accessor・interpreterの変更は本変更に含めない。

## 素朴なBFCRLE出力への復帰（2026-09-16）

full16について、ユーザーから生成コードの削減量に対してコンパイル時の負担が大きいとの報告。
移動run圧縮換算で約300→225 MiBという削減と、約26 MBの出力進行を根拠に、
最適化導入前の即時出力へ戻す。完走時間が約10倍という予想は実測で確認した値ではない。
kind/count配列とflush/resetを撤去し、反復回数の可逆な圧縮のみを行う。
整数幅の修正・診断・Rust側の最適化は維持する。full selfhostの再実行は行わない。
`portal-profile`の`optimizer`というcase名は既存コマンドとの互換用に残すが、
現在は即時serializerを実行するため、旧測定とは処理内容・出力が異なる。

検証結果:
- `verify-selfhost-compressed.py`: 全サンプルの通常/圧縮命令列一致、全byte反復数・24-bit境界のIR/BF一致。
- `verify-stage2-selfhost.sh`: 小入力のコンパイル・生成BF実行を含むstage2回帰検証成功。
- `verify-stage2-limits.py`: 301関数、255/256境界、frame上限、overflow拒否、16-bit offset加算の検証成功。
- `portal-profile`の出力fixture: 4 blockのIR/BF出力一致（420 bytes）。
- serializer本体は`0400bc4`の実装との一致を確認。helloは通常848 bytes、圧縮276 bytes。
ログは`tmp/simple-output-{compressed,stage2,limits}.log`。

## full28後のinterpreter比較・clear最適化（2026-09-21）

### 結果と測定範囲

**interpreterの2変更を採用。短いwide-globalコンパイルでexecuteを12.28%削減した。**
full28の自己入力による約48分の実行は、ユーザーの追加指示に従い再実行していない。
したがってfull全体が10%以上速くなったという実測結果ではない。
`metrics.sh`は評価設定の参照に使い、同じcompiler BFに短い入力を与えて比較した。

baselineは`abe7863`から別directoryへ取り出してrelease buildしたinterpreter。
candidateも同じtoolchainのrelease build。compiler BFは両者とも
`logs/full-selfhost-20260921-025357/tmp.bf`で、FNV-1aは`1441e285160166f4`、
展開後命令数は5,602,933,113。BFCソース、Rust compilerの生成物、ABIは共通である。
各入力をAB/BA交互4組、他のbenchmarkと重ねずに測定し、内部execute時間の中央値を比較した。
下表はprofileなし・stats有効・unlimited tape・progress無効。独立したwarmupは設けていない。

| コンパイル入力 | baseline execute | candidate execute | 削減 | process全体 baseline → candidate |
|---|---:|---:|---:|---:|
| hello | 0.044437 s | 0.041698 s | 6.16% | 0.645271 → 0.587052 s |
| stage7_globals | 0.566581 s | 0.522368 s | 7.80% | 1.100812 → 1.034080 s |
| stage8_aggregates | 0.509171 s | 0.475530 s | 6.61% | 1.068841 → 0.973956 s |
| wide-globals | 7.054007 s | 6.187741 s | 12.28% | 7.555080 → 6.705397 s |

wide-globalsは16個の`cell[255][256]`配列に、各8組の定数indexによるstore/load/outputを行う
7,256-byteの合成ソース。full28で熱いwide距離のBFシリアライズを短く再現する。
実際のcompiler自己入力とはphase・値・関数の頻度分布が異なる。
短い3例ではparseの約0.4〜0.5秒がprocess時間の大部分を占めるため、
process時間の差をそのまま長時間selfhostへ外挿しない。

全32 runで出力byte列、raw BF命令数、RLE命令数、最大pointerが一致した。
wide-globalsのnative operationsは1,775,302,461 → 1,558,333,357（12.22%減）。
clearの改善はnative operationの内部処理を減らすため、native数には現れない。
全run・入力・出力・binary/BF SHA-256は`tmp/full29-compare/combined/`に保存した。

`metrics.sh`と同じfull28 map・sample mode（1 ms）でもwide-globalsを別途4組交互測定した。
execute中央値は**8.284547 → 7.253367秒（12.45%減）**。
各pairの削減率は10.54%、12.85%、13.40%、9.72%で、常に10%以上という意味ではない。
process全体は13.675600 → 12.667579秒。約30 MBのprofile JSON生成等の固定費用がある。
短時間評価のためprogressは無効にした。ログは`tmp/full29-compare/combined-profile/`。

比較一括化だけの独立評価では、同じfull28 profile mapを使うsample modeで
hello 2.45%、stage7 3.50%、stage8 2.66%、wide-globals 9.28%のexecute削減だった。
最後の中央値は8.326135 → 7.553816秒。
ログは`tmp/full29-compare/bench.jsonl`、`tmp/full29-compare/wide/`。
このsample測定と上表のprofileなし測定の差を、clear単独の効果として差し引かない。

### 採用した実装

1. `compare_loop.rs`でBF idiom
   `[>>+<[-<->>-]>[-<<[-]>>>]<<<]`を認識する。
   入口の4セルが`L,R,0,0`なら、出口は`0,max(R-L,0),0,0`、pointerは入口と同じ。
   左右を同時に減らす回数と残余clear回数から、raw/RLE命令数・loop回数・移動量も正確に計算する。
   補助セルが非ゼロ、または必要なセルが未確保なら、テープを変更せず通常実行へ戻す。
   絶対アドレス・ABI field・profileのstable keyによる認識は行わない。
   通常BFとBFCRLE1/2を扱い、複数profile rangeにまたがるものは融合しない。
2. clear loopで残っていた`iterations_to_zero`のRust反復を除く。
   奇数増分`d`のloop回数は`initial * (-inverse(d)) mod 256`。
   読み込み時に逆元を計算しておき、実行時はwrapping乗算1回で求める。
   `[-]`、`[+]`だけでなく、認識対象だった全128種類の奇数増分を扱う。
   zeroへの書込み自体は以前から一括化されていたが、診断用の正確な計数には反復が残っていた。

これらは現interpreter上での改善である。生成するBF命令列は変わらず、
他のBF interpreterでも同じ速度向上が得られるという意味ではない。
差分も返す比較の別templateは、今回の比較idiom認識には含めていない。

### 探索した案と判断

| 層・案 | 確認したこと | 判断 |
|---|---|---|
| BFC: emit_repeat_character | 既存whileが固定展開より速いというユーザーの実測と過去の記録を確認 | whileを維持 |
| BFC: emit_hex_byteの4段比較をwhileへ変更 | 4入力の生成出力はbyte一致。native削減は0.024〜0.182%、単回のexecuteに改善なし | 試作を`tmp`に保持、productionは変更しない |
| BFC: emit_move_toで現在位置と同じならreturn | global書き戻しを省けるが、4入力のnative数は0.85〜1.09%増加。出力は一致 | 不採用。追加比較・制御の負担を避ける |
| Rust backend: 定数比較・比較template変更 | full28の比較は約20.3%。現templateを汎用BFとして一括実行できる。過去のCompareConst試作はnative削減がほぼなかった | 今回はinterpreter側を採用。生成BF削減とは分ける |
| ABI: scalar globalの配置変更 | `StaticLayout`の実装は既にaggregateの後、anchor近傍へscalarとremote-copy scratchを配置している。ABI文書冒頭の概略図の順序だけから「巨大配列の向こうにscalarがある」と判断できない | 単なるscalar並べ替えでは動的stackのscan往復を解消できない |
| ABI: D変更、専用lane、portal batch | D=16のoffset分解・window交換・frame予約と結び付いている。近接要求や同じchunkへの再訪だけでは、call・alias・再帰を跨ぐまとめ処理の安全性を示さない | 今回は保留。原理的に不可能とは結論しない |
| dispatcherのさらなる一括化 | 現在もcountdown chainを認識するが、case後のtailではpointerとguardが動的に変わりうる | tailを単に削除することはできない。runtime guard付きの省略は将来候補 |
| navigationの結果cache | stackのpush/popや任意値を持つauxの更新でscan経路が変わる | 固定アドレスだけのcacheは不正。書込みによる無効化か経路検証が必要 |

BFCの2試作は`tmp/full29-compare/{hex-while,move-equal}.{bfc,bf}`、
観測は`tmp/full29-compare/bfc-bench.jsonl`。単回時間は採用根拠にせず、
追加実装に見合うnative削減がなかったことと、出力が一致したことを記録する。
残るABI案には別の安全性検証・代表負荷での評価が必要であり、今回一括導入しない。

### 検証と再現

- `cargo test --release --workspace`: 296件成功、既存のignore 1件。
  比較の全65,536入力、clearの全32,768入力/増分、テープ端・unbounded growth・
  不正な補助セルからのfallback・near miss・profile境界・各profile modeと計数一致を含む。
- `cargo clippy --release --workspace --all-targets -- -D warnings`: 成功。
- 比較一括化を入れた段階の`verify-stage2-selfhost.sh`: 成功。
  最終版のclear変更も全workspaceテストと全奇数増分照合で確認した。
- 最終版の`verify-selfhost-compressed.py`: 全18例の展開後命令列、全byte反復数・
  24-bit境界のIR/BF出力が一致。
- ログ: `tmp/full29-compare/{final-workspace-tests,final-clippy,selfhost-tests,final-compressed-tests}.log`。

短い同条件の再測定コマンド（`--out`には未使用directoryを指定する）:

```sh
python3 scripts/compare-loop/run.py \
  --baseline tmp/full29-compare/bf-interpreter-baseline \
  --candidate target/release/bf-interpreter \
  --program logs/full-selfhost-20260921-025357/tmp.bf \
  --profile-map logs/full-selfhost-20260921-025357/tmp.bfmap.json \
  --case wide-globals --pairs 4 --out tmp/compare-loop-recheck
```

`--profile-map`を省くとprofileなし、`--case`を省くと上表の4入力を測定する。
各processは60秒でtimeoutし、compiler自己入力は生成しない。
runごとに出力と論理counterを照合し、fixture、manifest、時間、SHA-256を保存する。

## 比較idiom自体を短縮する追試（2026-09-21）

ユーザーの指摘を受け、`BF_OPTIMIZATION_NOTES.md`の「破壊的な大小比較と差分の同時計算」に
リンクされた[記事](https://zenn.dev/angel_p_57/articles/2d6f2f36eb235a)の短いcoreを試し、採用した。
前節では、このcompiler側の置換を比較せずinterpreterの認識を先に追加していた。

旧Compareのcoreは`[>>+<[-<->>-]>[-<<[-]>>>]<<<]`。
新形はscratchを`右operand,flag=1,zero=0,左operand`とし、coreを`[>>>[-<]<<-]`へ変更した。
両counterが非ゼロの1反復はraw 18→11、RLE 14→8命令となる。
新形は大小によって出口pointerが異なるため、その後の合流、boolean化、任意のtrue/false値、
差分の符号、scratch全消去まで実装している。core単体の時間比較ではない。
左右を入れ替えて「右>左」でstrict lessを求めることで、等値を区別する追加判定を省いた。
差分は従来と同じwrappingの「左−右」、全operand/output aliasの契約を維持する。

まず両者を同じ比較認識追加前のinterpreter
`tmp/full29-compare/bf-interpreter-baseline`で実行した。
RLE・clear・transfer等の既存最適化は有効だが、旧形・新形ともcompare専用認識はない。
profileなし・stats有効・unlimited tape・progressなし、AB/BA交互4組のexecute中央値:

| 入力 | 旧template | 新template | 短縮率 |
|---|---:|---:|---:|
| 全256×256 pairの4比較とoperand出力 | 0.932838 s | 0.557576 s | 40.23% |
| stage7_globalsのコンパイル | 0.609192 s | 0.601861 s | 1.20% |
| stage8_aggregatesのコンパイル | 0.541557 s | 0.529636 s | 2.20% |
| wide-globalsのコンパイル | 7.349030 s | 6.989854 s | 4.89% |

全32 runで出力byte列は一致。比較前後の移動・合流も変わるため、coreの短縮率を
プログラム全体のraw命令数や時間へ適用しない。生成compiler BFは6,026,016→6,024,932 bytes。

interpreterも新しいcoreを一括化できるようにし、旧形の認識も既存artifact用に保持した。
新形はflag=1とzero=0、必要セルの確保を実行時検査し、不成立時は通常BF実行に戻す。
入口counterがzeroなら補助セルを参照しない。終了pointerと残余もそのまま保持し、
raw/RLE命令数・loop回数・移動量を正確に集計する。ABIアドレスやprofile keyには依存しない。

旧compiler＋旧形認識VM（今回の開始時点）と、新compiler＋両形認識VMも別途4組で比較した:

| 入力 | 変更前 | 変更後 | 観測 |
|---|---:|---:|---|
| 全pair・4比較 | 0.048015 s | 0.051593 s | 7.45%遅い |
| stage7_globals | 0.551029 s | 0.546628 s | 0.80%短縮 |
| stage8_aggregates | 0.500438 s | 0.497858 s | 0.52%短縮 |
| wide-globals | 6.396115 s | 6.333606 s | 0.98%短縮 |

専用認識ありでは短いコンパイル全体の差は小さく、ほぼ横ばいと解釈する。
旧認識は残余Lのclearまで包含する一方、新形は終了pointer・残余を保存して出口処理へ渡すため、
周辺処理の費用が残る。比較ベンチの後退を隠さず、汎用BFとしての改善とのトレードオフを保持する。
full自己入力は実行していない。

最終版の`cargo test --release --workspace`は298件成功、既存ignore 1件。
`cargo clippy --release --workspace --all-targets -- -D warnings`も成功。
比較・SubWithBorrowの全unsigned pair、source/CIR、operand保存、入力/出力alias、
任意の真偽値、borrow chain、call frame、および新旧idiomのraw VM照合、終了pointer・残余、
profile counters/sample/exact、圧縮形式、境界/growth/fallbackを検証した。
`verify-selfhost-compressed.py`も全18例と反復数・24-bit境界でIR/BFの出力一致を確認した。
新compilerで生成したcontinuation mapを付けてstage7を実行し、mapなしと出力・論理counter・
native operationsが一致することも確認した。profile範囲によって新形の認識が外れていない。

ログと再現用scriptは`tmp/compare-template/`:
- `final-no-native-bench.jsonl` / `final-no-native-summary.json`: 認識なし。
- `final-native-bench.jsonl` / `final-native-summary.json`: 認識あり。
- `final-workspace-tests.log` / `final-clippy.log`: 最終版のテスト・lint。
- `final-compressed-tests.log` / `map-check.json`: 圧縮出力と実際のcompiler profile mapの照合。
- `final-no-native-bench.py` / `final-native-bench.py`: 短い入力の交互測定。

旧形の`LOOP`定数はinterpreter認識器のfixtureであり、compilerが将来も同じコードを出すという
契約ではない。compilerの出力が変わって認識に一致しなければ通常実行へ戻る。
今回の新形は`SLIDE`のfixtureで別途検証し、旧形の回帰検証も維持する。

### 専用認識のbypass（2026-09-21）

上記の認識なし測定は保存済みの旧interpreterを使用したもの。
その後、同一バイナリで比較認識だけを切り替えられる`--disable-compare`を追加した。
旧形`Compare`・新形`CompareSlide`の両方をparse時に無効化する。
既存の`--disable-remote-transfer`とは独立して指定でき、両方とも既定では認識有効。
RLE・clear・scan・通常のtransfer等の汎用最適化は有効のまま残る。

```sh
# 比較の専用認識だけを無効化
target/release/bf-interpreter --disable-compare --stats --no-progress program.bf

# 比較とRemoteTransferの専用認識を両方無効化
target/release/bf-interpreter --disable-compare --disable-remote-transfer --stats --no-progress program.bf
```

APIでは`RunOptions { disable_compare: true, disable_remote_transfer: true,
..RunOptions::default() }`。実行時の安全条件によるfallbackと異なり、入力値によらず
該当する専用認識を無効にできる。raw/RLE命令数などの論理counterは同じになり、
実際のnative operationsや時間は変わる。

検証: `cargo test --release -p bf-interpreter`の49件が成功。
新旧idiomについて、profileなし/counters/exact/sample・BF/BFCRLE1/BFCRLE2・
RemoteTransfer ON/OFFで比較bypass前後の出力と論理counterを照合した。
CLIでも新旧比較とRemoteTransferの3 fixture×4設定を実行し、出力・論理counter一致と
各flagの独立性を確認した（`tmp/compare-bypass/cli-check.json`）。
`cargo clippy --release --workspace --all-targets -- -D warnings`も成功。full自己入力は未実行。
