# hls-transmux ROADMAP

> 更新日期：2026-09-30
> 研究基线：v0.2.1，commit `11188ae`
> 状态：阶段 A 已随 v0.3.0 发布；阶段 B 已随 v0.4.0 发布；C1–C3 纳入 patch v0.4.1，工作区实现及验收完成，本次不发布。C4 与持续建设待办保留。历史研究基线与风险判断保留供追溯。

下一阶段按 **可靠性修复 → 全流程低内存 → 媒体正确性与输入兼容性** 推进。
项目已具备 TS/fMP4 输入、并发预取、流式 writer、取消接口和断点续传，优先补齐这些能力在慢网络、崩溃恢复和长视频场景下的边界。

## 1. 原 v0.2.1 研究验证基线

本轮研究检查了核心源码、集成测试、历史设计文档及 CI，未修改运行时实现。

| 检查 | 结果 | 限制 |
| --- | --- | --- |
| `cargo test --offline` | 68 项测试通过，包含 doctest | 默认配置不覆盖 `ffmpeg-finalize` |
| `cargo test --offline --no-default-features --features serde` | 通过 | 验证 feature 组合，不等于覆盖 checkpoint 持久化后的崩溃恢复 |
| `cargo check --offline --target wasm32-unknown-unknown --no-default-features` | 通过 | 编译验证，不是浏览器运行验证 |
| `cargo fmt --all -- --check` | 未通过 | 存在已有格式差异 |
| `cargo clippy --offline --all-targets -- -D warnings` | 未通过 | 存在已有 lint 问题 |

集成测试主要使用 `tests/fixtures/h264_aac_fhd.ts`；部分多分片测试重复使用同一份媒体数据。
测试通过可以确认现有场景没有回归，不能证明真实连续时间线、竞态、异常输入或进程崩溃场景已正确处理。

## 2. 优先级与依赖

工作量为相对规模，不代表工期承诺。**阶段 A/B 已分别随 v0.3.0/v0.4.0 发布；C1–C3 纳入 v0.4.1**。C4 暂不绑定发布版本。

| 阶段 | 方向 | 收益 | 相对工作量 | 依赖 |
| --- | --- | --- | --- | --- |
| A | 并发等待、取消与任务生命周期 | 避免挂起，改善暂停响应 | 小—中 | 确定性竞态测试 |
| A | checkpoint、临时文件与收尾恢复 | 避免损坏输出和下载成果丢失 | 中 | 故障注入与恢复状态约定 |
| B | Native finalize 与续传扫描流式化 | 降低长视频峰值内存 | 大 | A 阶段恢复边界、媒体正确性基线 |
| B | 64 位偏移与大文件输出 | 解除经典 MP4 的 4 GiB 限制 | 中—大 | 文件流式 mux 能力 |
| C | 时间戳、sample duration 与 fMP4 边界 | 提高时长、音画同步和解码正确性 | 中—大 | 独立媒体验证工具与样本 |
| C | HLS、HTTP 与 track 组合兼容 | 提高真实资源成功率 | 中—大 | 按功能补齐媒体测试 |
| 持续 | CI、媒体样本、fuzz 与性能基线 | 提供可重复的验收依据 | 中 | 随每项改造交付 |

## 3. 阶段 A：可靠性优先（v0.3.0）

**实现记录（2026-09-30，v0.3.0，已发布）：**

- A1：在检查 slot 状态前创建通知 future；屏障控制检查后、等待前的完成时机，覆盖 worker/consumer slot 的成功与失败。
- A2：playlist/init/media、write/flush 取消竞争；Source 新增默认 `create_session`/`stop_session` 接口，内置 Source 每任务隔离，取消监视器主动停止后台任务；退出与丢弃 future 时清理任务。Native/FFmpeg 收尾在 blocking worker 内协作检查取消。
- A3：checkpoint schema v1，SHA-256 绑定解析后的清单位置、range 和 codec 初始化，记录输出配置与阶段；旧 schema 明确拒绝。校验文件长度、fragment 边界、数量、sequence、初始化及时间戳基准后才截断尾部。flush 后发布 checkpoint，可选 SyncAll；checkpoint 原子持久化由调用方负责。
- A4：失败保留 partial；新增 `finalize_partial_mp4_async`，以及原入口 Finalizing checkpoint 的零网络收尾路径。独立同目录临时输出成功后 rename 替换目标，不先删除目标；Completed 回调在目标提交后发布。

验证用例见 `tests/cancellation.rs`、`tests/progress_cancel_resume.rs`、`tests/concurrent_download.rs` 以及 source/transmux 单元测试。版本与兼容说明见 README 中英文 v0.3 可靠性章节。

**验证结果（macOS，2026-09-30）：**

| 命令 | 结果 |
| --- | --- |
| `cargo test --offline` | 通过 |
| `cargo test --offline --features serde` | 通过 |
| `cargo test --offline --no-default-features` | 通过 |
| `cargo test --offline --no-default-features --features serde` | 通过 |
| `cargo test --offline --all-features` | 87 项通过，含 doctest 和 FFmpeg 9 后端 |
| `cargo check --offline --target wasm32-unknown-unknown --no-default-features` | 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --offline --all-targets --all-features -- -D warnings` | 通过 |
| `cargo publish --dry-run --allow-dirty` | 通过，仅验证、未发布 |

格式/lint 清理独立提交：`93349b7`。行为实现提交标题：`feat: deliver v0.3.0 reliability and crash recovery`。

**保证范围与剩余限制：** 进程崩溃恢复依赖调用方保留最后 checkpoint；断电保护依赖文件同步、checkpoint/目录持久化和文件系统。通用 writer 不支持恢复或 SyncAll。文件提交及 FFmpeg header/trailer 的取消为协作式，完成当前操作后返回。v0.3.0 的 Native finalize 和续传扫描仍全量读取；已在下述 v0.4.0 阶段 B 中替换。平台替换行为由新增 Linux/macOS/Windows CI 覆盖，本轮本地运行环境为 macOS。


### A1. 消除并发消费的丢失唤醒窗口

**v0.2.1 依据：** [`read_from_slot`](src/source.rs) 在检查 `InFlight` 状态并释放锁后才创建 `notified()`；生产者使用 `notify_waiters()`，不保存通知 permit。
根据 [Tokio Notify 文档](https://docs.rs/tokio/latest/tokio/sync/struct.Notify.html)，该顺序存在消费者错过通知后持续等待的窗口。
这是原 v0.2.1 的竞态风险；v0.3.0 已用受控交错回归测试验证修复。

- [x] 先创建通知 future，再检查共享状态；或改为能保存状态的通知机制。
- [x] 用同步屏障控制生产者完成时机，补充确定性回归测试。
- [x] 覆盖 worker 创建 slot、consumer 自建 slot、失败通知等路径。

**验收：** 在“检查状态与进入等待之间完成下载”的受控交错下，消费者始终能返回；成功和失败路径均不永久等待。

### A2. 接入实际可中断的取消等待

**v0.2.1 依据：** [`CancelToken`](src/cancel.rs) 和选项文档描述了读取与取消竞争，但 [`demux_segment`](src/transmux.rs) 直接 await 资源读取，当前流水线没有调用 `CancelToken::cancelled()`。
取消主要在分片开始前检查，无法保证正在等待慢请求时迅速返回。

- [x] 在 playlist、init segment、media segment 读取处接入取消竞争。
- [x] 取消后停止本次任务的预取请求，包括 worker 和 consumer 自建 fetch。
- [x] 明确复用同一 `Source` 时的任务边界，避免依赖外部 `Arc` 被释放才结束后台工作。
- [x] 为阻塞 writer、finalize 定义取消行为；部分写入恢复与 A3 一起设计。
- [x] 明确 native CPU 密集工作是否需要 `spawn_blocking`，保留 wasm 兼容路径。

**验收：** 永不返回的 Source 不阻止取消；阻塞 sink 的取消行为可验证；任务退出后没有持续发起的后台请求。
测试使用明确超时阈值，避免依赖公网和机器速度。

### A3. 将正常暂停续传升级为崩溃恢复

**v0.2.1 依据：** [`transmux_fragmented_async`](src/transmux.rs) 以 append 模式打开已有文件。
历史索引扫描限制在 checkpoint 字节范围内，但没有按 checkpoint 截断残留，也没有完整校验 checkpoint 与文件、playlist 的一致性。
因此，文件尾部存在未提交 fragment 时可能影响恢复结果；该场景已在 v0.3.0 用未提交完整尾部、半 fragment 尾部和非法边界故障注入验证。

- [x] 文件短于 `bytes_written` 时明确拒绝恢复。
- [x] 文件长于 `bytes_written` 时，校验 checkpoint 后截断未提交尾部，再继续写入。
- [x] 验证 checkpoint 位于完整 fragment 边界，并核对 fragment sequence 等必要状态。
- [x] checkpoint 绑定输入清单、初始化配置及输出配置，避免跨任务误用。
- [x] 设计 checkpoint schema 版本和兼容策略，评估新增字段对公开 API 与 serde 数据的影响。
- [x] 定义写入完成、flush、可选持久化同步与进度回调的顺序，明确正常暂停和崩溃恢复的保证范围。

**验收：** 覆盖文件过短、尾部半个 fragment、已写完整 fragment 但 checkpoint 落后、错误 playlist 和错误配置。
可恢复场景与完整运行的媒体内容及索引一致；不可恢复场景返回明确错误，不继续污染文件。

### A4. 保留失败成果并允许单独重试 finalize

**v0.2.1 依据：** [`StreamingMp4` 文件入口](src/transmux.rs) 在非取消的 stage-1 错误后删除临时文件，stage-2 返回后也删除临时文件，即使 finalize 失败。
同时，`completed_segments >= segments.len()` 的续传校验拒绝“下载完成、尚未收尾”的状态。

- [x] 定义 `Downloading → Finalizing → Completed` 状态及恢复入口。
- [x] 默认保留可恢复的临时数据，为显式清理提供清晰约定。
- [x] 下载已完成时可只重试 finalize，不重新下载媒体分片。
- [x] 最终输出先写独立临时文件，完成后再替换目标文件。
- [x] 核查平台上的替换行为，以及已有目标文件的保护策略。

**验收：** 网络失败和 finalize 失败后临时成果仍可恢复；finalize 重试不读取网络分片；失败不覆盖已有完整目标文件。

## 4. 阶段 B：全流程低内存与大文件（v0.4.0）

**实现记录（2026-09-30，已随 v0.4.0 发布）：**

- B1：checkpoint 范围内的 `Read + Seek` 扫描跳过 mdat payload；续传只保留 tracks/tfra，Native finalize 保留源偏移、大小和时间信息，再以固定 1 MiB 缓冲复制。共享 mux 布局用于 bytes 与文件路径；不同 timescale 的 chunk 按实际时间排序，保留 partial 中的 duration/config。init 缓存借用、预取结果尽可能转移所有权、streaming sample 构建消费 packet 缓冲。
- B2：按 track 自动选择 stco/co64，迭代处理 moov 增大后的偏移；支持 64 位 mdat header、长 duration 的 mvhd/tkhd/mdhd/elst，以及必要的 cslg version 1。尺寸、偏移和时间换算增加溢出校验。
- 兼容性：公开入口及 checkpoint schema v1 不变，实际 v0.3.0 checkpoint/partial fixture 验证下载续传与零网络收尾。保留取消、失败成果与原子目标替换约定。修正 streaming 和历史 tfra 的 traf/sync sample 指向。
- 验证：连续三分片 TS 与外部生成 fMP4 B 帧样本通过 FFprobe 的 DTS/PTS/duration/payload hash 对照与 FFmpeg 解码/seek；4.40 GB 稀疏输出由 FFprobe seek 到 32 位边界之外。RSS、耗时、吞吐和磁盘测量见 [BENCHMARKS.md](BENCHMARKS.md)。小型独立媒体验证已接入 FFmpeg CI job。
- 本地检查：默认、serde、无默认 feature、无默认 feature + serde、all-features（FFmpeg 9）测试，以及 wasm check、fmt、Clippy、publish dry-run。跨平台结果由已有 CI matrix 验证，本地环境为 macOS arm64。

**剩余限制：** 内存随 sample/fragment 索引增长，下载仍受当前分片及预取结果体积影响；bytes API/batch Mp4 保持内存输出。稀疏大文件验证只覆盖结构和 seek，真实媒体解码使用小型连续样本。完整 C1/C2、字节预算和新增输入兼容性留待阶段 C。

### B1. 将 Native finalize 改为分块处理

**v0.3.0 实现边界：** Native finalize 整文件读取临时 fMP4，完整 demux 后重新 mux；经典 MP4 mux 构建完整 `mdat` 缓冲。
v0.3.0 的 `StreamingMp4` 只有下载阶段逐片写盘，Native 收尾阶段内存随媒体体积增长，续传重建历史索引也使用整文件读取。上述路径已由 v0.4.0 流式扫描与文件 mux 替换。

- [x] 建立 sample 元数据索引，保存源文件偏移、大小、track、时间戳和 duration，避免保存全部 sample payload。
- [x] 第一遍扫描 fragment 构建索引，第二遍生成 `moov` 并分块复制媒体数据。
- [x] 明确多 track 交错布局与 faststart 偏移计算。
- [x] 续传重建 `tfra` 使用流式 box 扫描，不读取全部媒体 payload。
- [x] 保留 bytes API 的内存输出语义，新增或重构文件路径的流式 mux 能力。
- [x] 减少 init 缓存、预取结果和 sample 构建过程中的不必要复制。
- [x] 修正文档中的全流程低内存描述，直到基准验证完成。

**验收：** 对递增体积的长视频记录峰值 RSS；增长主要来自 sample 索引而非媒体 payload。
记录 finalize 耗时、复制吞吐和临时磁盘占用，并验证输出媒体内容、时间戳及 faststart 布局。

### B2. 支持超过 4 GiB 的经典 MP4

- [x] 支持 `co64` chunk offsets 和大尺寸 `mdat` box。
- [x] 核查 box size、sample size、偏移转换及所有溢出边界。
- [x] 核查长时长情况下 `mvhd`、`tkhd`、`mdhd` 的版本选择。
- [x] 增加稀疏文件或元数据级边界测试，避免每次 CI 都生成巨量媒体数据。

**验收：** 覆盖 32 位边界上下的偏移、box size 和时长；大文件由独立工具读取并完成 seek 验证。

## 5. 阶段 C：媒体正确性与兼容性

### C1. 完善 fMP4 sample 解析与输出配置一致性

**v0.4.1：** 处理所有 `trun`，检查 mdat 边界；支持显式 base、moof-relative offset 和后续 run 连续偏移。AVC/HEVC 的 1/2/4 字节前缀统一为四字节；稳定完整的 avc3/hev1 转为 avc1/hvc1，移除已验证的 in-band 参数集。隐式跨 traf 布局明确拒绝。

- [x] 支持每个 `traf` 的多个 `trun`，正确处理 run 数据偏移；尚未支持的布局明确拒绝。
- [x] 输入 NAL 长度前缀统一转换为输出格式，或保留并正确声明原始宽度。
- [x] 验证 `avc1/avc3`、`hvc1/hev1` 的参数集语义及输出转换策略。
- [x] 统一 batch 与 streaming 路径的编码参数变更检查，避免使用旧初始化配置写入新数据。
- [x] 核查 `tfra` 的 track、traf、trun 与 sync sample 指向。

**验收：** 多 `trun`、不同 NAL 前缀宽度和参数集变化样本由独立工具正确读取；不支持的组合在写入损坏结果前报错。

### C2. 保留媒体时间信息并统一报告

**v0.4.1：** fMP4 保存原 timescale/DTS/PTS/duration；TS 在排序前跨分片解包回绕，视频末帧使用下一分片首帧，EOF 使用最近有效间隔，孤立帧默认 3000/90000 秒。共用 decode 起点和配置校验；恢复扫描重建 track 统计和终点，经典 MP4 使用 edit list 与 composition 表表示展示时间。

- [x] 保留 fMP4 输入的 sample duration 和必要 timescale 信息，减少不必要的往返 rescale。
- [x] 区分“已知 duration”与“TS 需要推算 duration”，处理跨分片末帧的边界。
- [x] 覆盖 B 帧 composition offset、VFR、非零初始时间戳和 TS 33 位时间戳回绕。
- [x] 统一音视频时间线归零、负时间偏移和 discontinuity 的策略。
- [x] 完善分片输出的 track 信息与 duration 报告，明确续传后的下载字节计数语义。
- [x] 为下载、处理、finalize 阶段提供可区分的进度，避免分片完成被误解为任务已完成。

**验收：** 与参考工具对照 sample 数量、DTS/PTS、末帧 duration、总时长和音画起始偏移；容差依据 timescale 定义。

### C3. 优先完成低成本 HLS 与 HTTP 边界修复

**v0.4.1：** 元数据及未知标签跳过，已知未支持的媒体语义拒绝；range 检查同 URI 连续性、非零长度和溢出。HTTP 检查 206、单位、起止、总长及实际长度。`HttpRequestPolicy` 覆盖 playlist/init/串行/预取的完整请求超时、有限指数退避和单资源上限，默认不额外重试或限制。错误保留原分类并清除 URL 用户信息、query、fragment。

- [x] 支持不影响转封装的元数据标签，并按规范处理未知标签；影响解密或媒体语义的已知未支持功能仍明确报错。
- [x] 隐式 BYTERANGE offset 验证前一个分片属于同一资源，且确实为 byte range。
- [x] init 缓存键包含解析后的 location 和 ByteRange。
- [x] 校验 HTTP `Content-Range` 的区间以及实际响应长度。
- [x] 提供明确的超时、有限重试与退避配置，覆盖串行和并发请求。
- [x] 为错误增加资源、分片序号和阶段上下文，兼顾签名 URL 等敏感参数的处理。
- [x] 评估预取的字节预算和单资源大小限制，避免仅按 slot 数量控制内存。

规范依据：[RFC 8216 §4.3.2.2、§6.3.1](https://www.rfc-editor.org/rfc/rfc8216.html)。

**验收：** 元数据标签不阻断普通 VOD；非法隐式 range、错误 206 区间和短响应明确失败；超时与重试可用本地服务确定性验证。

### C4. 按真实输入需求扩展功能

以下是候选顺序，不表示承诺全部实现：

1. **纯音频 / 纯视频**：解除 TS 与经典 MP4 路径对音视频同时存在的要求，形成统一 track 组合支持。
2. **Alternate audio**：解析 rendition 选择，协调两套 playlist 的时间线和输出 track。
3. **同配置 discontinuity**：先处理时间戳重置；编码配置变化单独设计。
4. **AES-128**：根据真实输入需求决定，作为可选能力，明确 key、IV、缓存和取消行为。
5. **Live / EVENT / LL-HLS**：独立规划 playlist 刷新、去重、滑动窗口、停止条件及 checkpoint 模型。

**验收：** 每项功能交付明确支持矩阵、独立真实样本、端到端验证和未支持边界。

## 6. 持续建设：测试与性能基线

- [ ] 新增真实连续多分片 TS、HEVC、外部生成 fMP4、B 帧、VFR、纯音频和纯视频样本。
- [ ] 使用 FFprobe 和 FFmpeg 做独立的结构、时间线及解码验证；需要时增加目标播放器验证。
- [ ] 为取消、并发、恢复、短写和 finalize 失败增加故障注入。
- [ ] 针对 HLS、TS、ISOBMFF 和 codec parser 建立 fuzz；限制异常 sample count 等输入引起的资源分配。
- [ ] 建立下载吞吐、首字节时间、峰值 RSS、finalize 耗时和取消响应基准。
- [ ] CI 加入 fmt、Clippy、无默认 feature、serde、wasm 和 FFmpeg 后端组合；FFmpeg 环境单独配置。
- [ ] 核心媒体改造完成前，先建立外部参考输出与测试断言，避免 parser 与 muxer 共享错误却相互验证通过。

## 7. v0.3.0 交付与下一轮范围

v0.3.0 已实现阶段 A，交付顺序如下：

1. 丢失唤醒修复及确定性回归测试。
2. 下载读取取消接入及预取任务退出验证。
3. checkpoint 文件长度、边界校验与尾部截断。
4. finalize 失败保留临时文件，以及仅重试收尾的入口。

v0.4.0 已发布。v0.4.1 工作区完成 C1–C3，本次不实际发布。下一轮按真实输入需求推进 C4。

## v0.4.1 验证记录与剩余边界

- 默认、serde、无默认 feature、无默认 feature + serde、all-features 测试；wasm 无默认 feature 编译、fmt、Clippy 和 publish dry-run。
- 旧公开类型完整字面量与调用仍编译；schema v1 不变，released v0.3 fixtures 和从 v0.4.0 commit `20f1593` 生成的 checkpoint/prefix 均恢复成功。
- 多 run、NAL 宽度与 AVC/HEVC 参数变化、负 CTS、TS 回绕和恢复、init 同 URI 不同 range，以及本地 HTTP range/超时/重试/限额/取消/session 隔离测试。
- 独立 FFprobe 对照九种 TS/fMP4 场景的输入与 fragmented、Native、batch 输出：sample 数量、DTS/PTS、duration、音视频起始偏移、规范化 NAL/AAC 内容；容差一个输出 tick。FFmpeg 完整解码及 seek 通过。
- `downloaded_bytes` 仅累计本次成功取得的媒体分片，包括恢复校验与 lookahead；不含 init、历史或失败尝试。四类 `*_with_runtime` 入口提供独立阶段事件，Completed 仅在最终提交成功后触发；旧回调只报告已提交 checkpoint。
- 单资源上限已实现；总预取字节预算本次仅交付测量和背压设计，见 [BENCHMARKS.md](BENCHMARKS.md)。并发 slot 限制不是整个调用的字节上限。
- 不支持复杂 edit list、不同配置参数集、多 sample description、隐式跨 traf 布局；不连续 decode 时间线明确拒绝。仅一次换算造成的一 tick 间隙可修正。C4 的加密、discontinuity、alternate audio、live 保持拒绝。
- 跨平台由既有 CI matrix 验证；本轮本地 macOS。两个手工大文件/RSS 测试保持 ignored，可按基准文档单独运行。
格式和 lint 清理单独提交，便于审阅行为变更。

## 8. 维护约定

- 完成待办时附上对应 PR 或 commit、验证结果及剩余限制，再勾选状态。
- 待复现风险在加入回归测试后更新为已复现、已排除或已修复，保留判断依据。
- 性能结论附上输入规模、运行环境和测量方法，不用单个样本替代长视频基准。
- 新功能按实际接入需求重新排序；公开 API 或 checkpoint 变化说明兼容影响。
