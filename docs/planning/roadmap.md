# hls-transmux → hls-engine ROADMAP

更新时间：2026-10-06。当前候选版本：v0.9.0（实现与验收见本版记录，尚未发布到 registry）；上一版本基线 v0.8.0，commit `8d6c54f`，此前 v0.7.0 为 `24c5ac4`。原始基线为 v0.5.0，commit `6a2cb3d`。

本轮根据 hls-downloader v4.0 的下游改造需求，规划在 **v0.6–v0.10 五个 minor**
逐步交付协议与解密、范围与 epoch、sample 解密、持续 session、多轨与字幕，
随后在 **v1.0.0** 交付多输入恢复、GCM 实验及完整组合验收，稳定正式契约，
并正式更名为 **hls-engine**，完成从转封装库到完整 HLS 媒体处理引擎的交付。
版本号表达交付顺序，不承诺日历工期。v0.6–v0.9 的本地实现与验收分别见各版记录，v0.9.0 尚未发布；v0.10 及后续版本仍为规划；未发布或未完成项不能作为已发布能力声明。

需求来源为 [hls-transmux M0 上游改造需求](hls-transmux-m0-requirements.md)
（2026-10-04 草案，位于下游工作区）。下文保留 U01–U10 编号方便双方追踪，
同时独立记录范围与验收门槛，阅读本 roadmap 无需取得下游文件。
草案中的 trait 名称、输入入口、字幕格式和策略是设计建议，需经契约评审和原型验证后定稿。

## 当前基线和目标边界

v0.5.0 已实现双路 prepared session、共同时间轴映射、TS/fMP4 混合合流、
独立纯轨输出、结构化输入错误、无 checkpoint 进度及需求驱动读取。
已有 native 和 WASM/Node 契约执行；接入与限制见
[prepared-sessions.md](../prepared-sessions.md)。本次以本地版本及发布提交为基线，
不将下游锁定版本视作 registry 最新版本。

| 领域 | v0.5.0 基线 | v1.0 目标 |
| --- | --- | --- |
| 输入与解析 | 已选定的有限 VOD；primary 加可选一条外置音频 | typed playlist、稳定 input/track ID、有限与开放输入 |
| 加密 | KEY 直接拒绝，包括 METHOD=NONE | AES-128、规定 profile 的 SAMPLE-AES/cbcs/cenc；独立 GCM 实验 profile |
| 时间轴 | 精确共享 origin、双路偏移、TS wrap、简单 edit offset | 可解码范围、epoch、重置、缺口策略、配置变化拆分及映射报告 |
| 媒体与轨道 | TS/fMP4，AVC/HEVC 与 AAC-LC；外置音频替换内嵌音频 | Packed AAC、多音轨及元数据、持续字幕 sample mux |
| 生命周期 | 单次 prepared 输出；writer flush，native 临时文件发布 | 开放输入、stop/cancel/finalize、operation 内 pause/resume、长期背压 |
| 恢复 | legacy 单路 schema v1；prepared 明确不支持恢复 | native 多输入已提交前缀恢复，含范围、加密、epoch 与字幕 |
| 诊断 | role/phase/segment/resource/range；有限总量 | input/track/epoch/key/sample 上下文、未知总量、结果报告及组合描述 |

默认保留源 codec，不隐式转码。正式视频范围为 AVC/HEVC，音频为 AAC-LC；
TS SAMPLE-AES 视频仅覆盖 AVC，HEVC sample 解密通过 fMP4 提供。
Browser WASM 与 Node native 共用 parser、crypto、demux、timeline 和 mux。

DRM license/CDM、LL-HLS part/delta/blocking reload、ABR/动态换轨、其他 codec、
Browser 跨页面持久恢复不在本轮范围。精确到帧的裁切与重编码由 SDK 显式转码路径承担。
AES-256-GCM 按固定 HLS 第二版草案 -22 交付实验 profile，进入 1.0 时仍单独标记 experimental。

## v1.0 引擎定位与正式更名

**定位决策已确定：v1.0.0 正式名称为 `hls-engine`。** v0.x 继续使用
`hls-transmux`；实际包名、代码引用及发布产物迁移随 1.0 完成。

到 1.0，本库在上述支持范围内形成完整的 HLS 媒体处理引擎：从协议与资源身份、
资源和 sample 解密、demux、范围与 epoch 时间映射，到有限/开放输入的持续处理、
多音轨与字幕 mux、背压与任务生命周期、已提交前缀恢复、诊断和组合查询，
均由统一的 native/WASM 核心交付。转封装是引擎中的输出能力。

完整性以 U01–U10 和 1.0 组合验收为准；v0.8.0 的实现及验收见下文，后续实现状态不因定位决策改变。
引擎与 SDK 继续遵循下述职责分工，沿用既定 codec、DRM、LL-HLS、网络和转码边界；
GCM 在 1.0 中仍为独立实验 profile。

## 两侧职责与兼容原则

| hls-transmux（v1.0 起 hls-engine） | hls-downloader |
| --- | --- |
| 媒体 playlist 语义、不可变资源身份、demux、密码运算、时间映射与 mux | master/rendition 发现与选择、公共 API/types、capabilities 与消费者错误映射 |
| typed key request、operation 内 key 生命周期和解密策略 | identity HTTP/KID resolver 接入、headers/credentials、transport、重试与 JS signal |
| 增量输入接收、预算、epoch/缺口策略执行、输出收尾 | playlist 轮询/条件请求、网络重连、缺片/起点配置、operation handle |
| checkpoint 内容、媒体提交边界、恢复校验与重建 | checkpoint 原子持久化、授权重建、任务/文件管理与旧任务路由 |
| 时间映射、持续字幕 sample mux 和拆分报告 | WebVTT 解析与 cue 裁切、sidecar、章节、转码编排 |

SDK 管理 live 刷新，上游按需接收描述符并读取媒体。上游不使 WASM 强制依赖 Rust HTTP、
timer、线程、文件系统、FFmpeg、SharedArrayBuffer 或跨源隔离。
key transport 与媒体语义分层，两侧不各自维护一套 sample parser 或解密算法。

后续 minor 优先增加独立模型和入口，保留旧 API 的结构字面量、exhaustive error match、
`with_audio()` 的替换语义与有限 prepared 默认行为；必要的调整须有迁移说明和兼容用例。
通用 writer 保留 caller ownership，上游负责 flush，SDK 负责 close/abort。
prepared 失败 partial 不能直接当作 checkpoint；legacy schema v1 保留继续完成的路径。
正式 1.0 API 可在 RC 前整理，不能以更改版本号静默丢弃已有任务或 checkpoint。

## 版本交付总览

| 版本 | 交付主题 | 主要需求 | 依赖与下游可接入门槛 | 当前状态 |
| --- | --- | --- | --- | --- |
| v0.6.0 | typed playlist、资源身份、key provider、AES-128 有限 VOD | U01、U02、U10 基础 | M0 契约与风险原型通过；新增加密路径先不支持持久恢复 | 实现与后续修复已提交，基线至 v0.6.2 |
| v0.7.0 | 可解码范围、epoch、缺口与拆分输出 | U04；U03/U08 内部 hook | 基于 v0.6 身份/key；SDK 可对齐范围、字幕和章节，hook 原型不声明 sample 解密能力 | 实现与本地门槛完成，已提交 `24c5ac4`；未发布 registry |
| v0.8.0 | TS SAMPLE-AES 与 fMP4 cbcs/cenc 有限输入 | U03、U10 加密诊断 | hook 顺序、protection metadata 和双运行时 clear sample 对照通过 | 实现与本地验收完成，基线 `8d6c54f`；SDK 隔离联调通过 |
| v0.9.0 | Live/EVENT、持续 session、背压、stop 与 pause | U05、U01 增量、U10 开放报告 | v0.7 epoch 与 v0.8 解密进入开放输入；慢 sink、轮换、竞态和有界状态有证据 | 本地候选完成；范围与限制见 v0.9 验收记录，未发布 |
| v0.10.0 | 多音轨、持续字幕 mux、完整 Packed AAC | U06、U07、U08 | 固定多轨集合、统一映射；有限/开放/范围/解密组合和播放器证据通过 | 规划中 |
| v1.0.0 | hls-engine 正式更名与完整引擎交付；新 checkpoint、多输入恢复、GCM 实验与稳定契约 | U09、U02 实验、U10 完整组合；U01–U10 组合验收及 SDK M5 | 基于全部媒体路径的 committed prefix；全部目标交付、实验认证、RC 联调、长录制、恢复迁移、更名迁移及发布产物验证通过 | 更名决策已确定；实现与发布规划中 |

sample 解密先在有限输入中验收，再进入开放输入，降低一次引入密码、容器和生命周期变化的风险。
多输入恢复依赖最终 mux/track/epoch 契约，因此直接纳入 v1.0；v0.6 即定义提交模型并预留恢复身份。

下游 M0–M5 是 SDK 阶段，不与上游 minor 一一对应：M1 首次接入 v0.6，M2 接入 v0.7，
M3 依赖 v0.8–v0.9，M4 依赖 v0.10 与 1.0 的恢复/实验能力，M5 对应 1.0 RC 到正式发布。
**下游 M1 中 AES-128 单路恢复组合的完整验收需等待 v1.0 恢复能力。** 此前仅接入非恢复加密路径，
旧 clear 单路恢复继续走 legacy；SDK 若保持原 M1 退出条件，应等待该门槛满足再关闭 M1。

## M0 契约与原型门槛

M0 在 v0.6 正式实现前完成，设计时覆盖最终目标，避免每个 minor 重建公共模型。
原型只用于验证可行性，不代表能力交付。P0 已于 2026-10-04 完成上游契约决策与风险原型；
决策、范围限制及 native/Node-WASM/Chrome 证据见 [P0 记录](m0-decisions.md)。
下游接入及各版本完整组合验收仍按后续门槛执行。

- [x] 定稿 U01–U10 的责任边界、支持组合、fixture 来源和版本归属。
- [x] 确定增量主入口、快照原子性、source generation、input/track ID，以及 accepted 与 committed 的区别。
- [x] 确定异步 key provider 的授权作用域、候选选择、刷新、请求合并、取消与敏感数据复制边界。
- [x] 确定 raw sample 解密顺序、保护信息优先级、范围/epoch 映射及拆分 sink 的获取和交接方式。
- [x] 确定 stop/cancel/EOF 优先级、独立 EOF、writer ownership、pause 和恢复的提交/持久性边界。
- [x] 优先评估 wvtt，确定字幕 sample entry、保真限制、classic/fMP4 支持模式和目标播放器清单。
- [x] 确定新 checkpoint 版本与 legacy schema v1 继续完成路线，不先承诺自动迁移所有旧记录。
- [x] 在 native 和实际 WASM 执行 AES-128 异步 provider、fMP4 归一化前 cbcs/cenc、TS AVC/AAC hook 原型。
- [x] 验证两个 epoch 的音视频/字幕映射，以及开放输入向阻塞 writer 输出时的 stop/cancel 原型。

## v0.6 协议元数据与资源解密

交付 U01 与 U02 的有限 VOD 路径，并建立后续身份、诊断和预算模型。

实现拆分、M0 前置门槛与验收矩阵见 [v0.6.0 实现计划](v0.6.0-implementation-plan.md)（P0–P6 已完成本地验收，尚未发布）。

- [x] 新 typed parser 保留 KEY/ENDLIST/type/discontinuity sequence/PDT/GAP，解析能力与执行支持分开；旧入口保留预检行为。
- [x] Segment/MAP/key context 不可变；MAP 使用声明时的 key，NONE 清除状态，SESSION-KEY 仅作为候选校验。
- [x] 身份包含 input、原始 sequence、epoch、URI/range；逻辑槽位用于识别改写，generation 表达源重启。
- [x] sequence/offset/ticks 保持检查过的整数与无损 serde/JS 桥接，不以 JS number 静默截断。
P1 接入与验证见 [typed playlist 文档](../typed-playlists.md)。有限快照身份已实现；
跨 revision 的 KEY/MAP 返回 NeedsReconciliation，滚动窗口对齐仍属于 v0.9。

- [x] 异步 key provider 按 operation/授权隔离，返回 key 与版本/有效期或 typed unavailable/error；支持候选 KEYFORMAT、请求合并、有界缓存、失效刷新与同 URI 换 key。
- [x] 单个 key 等待者取消不影响其他活跃等待者；operation 取消终止剩余请求并丢弃迟到结果，密文/clear resource/clear sample 缓存分别绑定加密上下文。
- [x] 上游共享 AES-128 resource 解密，使用原始 sequence 推导隐式 IV；加密 MAP 要求显式 IV。
- [x] 区分完整加密资源的 BYTERANGE 与任意密文切片；未验证范围/I-frame 组合稳定拒绝。
- [x] 建立 input/key/resource typed 错误、脱敏规则和组合描述，保留 underlying typed error；资源/key URI 去除凭据、query/fragment，raw key 不进入 Debug、事件、report 或 checkpoint。

验收覆盖 TS/fMP4、显式/隐式 IV、MAP/轮换/NONE、多个 KEYFORMAT、同 URI 换 key、
大 sequence、range 继承、取消/并发隔离、错误 key/IV/padding 与解密后媒体结构。
使用独立 CBC 向量和 clear sample 对照；CBC 无认证，不承诺检出所有错误 key。
新增执行仍为有限输入，不声明 live、sample 解密或持久恢复。

## v0.7 范围与 epoch 时间轴

**状态：实现与本地验收完成，已提交 `24c5ac4`，尚未发布到 registry。**

独立 timeline API、范围/epoch/gap/split、资源级增量 sample 游标及内部 hook 已实现。
选定范围不再保存全程 sample 索引；固定 240 sample/4 resource 预算完成
8/64/256 分片的完整输出与长 GOP 回放。有限资源目录及报告成本单独公开，
首写仍等待有限扫描/验证；不承诺总内存恒定或 live 首写延迟。

已提交的验收证据：

- all-features 253 项测试通过，其中包含 39 项 timeline 集成测试；3 项原有手工测试保持 ignored。
- 148 个输出通过独立 FFprobe/FFmpeg 时间、payload 和解码对照，包含 Native 与可选 FFmpeg 收尾。
- Native、Node WASM 和真实 Chrome 的输出及报告一致；15 项预算用例与 allocator/首写/JS heap/WASM 页测量已记录。
- 真实 SDK 隔离副本复用 SourceHost/key/Promise 桥；9 组范围、双输入、AES、重置、缺口和拆分用例的 native/Chrome 报告及输出 hash 一致。
- 默认、serde、无默认 feature、无默认 feature + serde、all-features、WASM 编译与执行、fmt/Clippy、提取包示例及发布 dry-run 均通过。

实现与限制见 [v0.7.0 验收记录](../release-0.7.0.md) 和
[机器可读证据](../release-0.7.0-evidence.json)。SDK 产品 capability 开启、版本发布
及 registry 发布分别记录为后续发布活动，不把隔离联调视为 SDK 产品已上线。

交付 U04，复用 `MediaTime` 和精确整数换算，扩展为 input/track/epoch 映射。

- [x] 以公共 presentation timeline 的半开区间请求范围；报告实际可解码范围、随机访问点、preroll 和 MAP/key 依赖。
- [x] 区分原始 DTS/PTS、公共展示时间、输出时间与有依据的 PDT 映射，保留负 CTS 和各路偏移。
- [x] 支持同配置 discontinuity/时间戳重置；区分 33-bit wrap、重置和半周期歧义，不按 rendition sequence 同步。
- [x] 定义保留/显式压缩缺口策略；不可表达缺口或配置变化按策略拆分或失败，拆分边界必须可解码。
- [x] 发布有序 epoch 映射与各子输出报告，已提交映射不可被迟到输入修改；同步供字幕和章节使用。
- [x] 实现有界 sample lookahead 与有限输入 EOF 尾帧策略；验证 clear/AES-128 范围输出。持续 session 的 stop/drain 执行随 v0.9 交付。
- [x] 在内部建立 raw sample 解密 hook 和 Packed AAC/ID3 hook，完成异步 key 与字节布局原型，供 v0.8/v0.10 使用。

验收覆盖跨 segment/epoch 范围、关键帧依赖、timescale/负 CTS/音画偏移、MAP 同配置更新、
配置变化拆分/失败、wrap/重置/歧义、缺口保留/压缩、空/越界范围及跨界字幕时间映射。
本版 hook 不作为正式 SAMPLE-AES 或 Packed AAC 支持声明；范围恢复在 v1.0 组合验收。

## v0.8 Sample 解密

交付 U03 的有限 TS/fMP4 路径。解密发生在容器/elementary 结构识别后、读取 clear bytes 的
codec 检查和归一化前，避免 `is_key_sample()` / `normalize_nals()` 读取密文。

- [x] TS SAMPLE-AES 支持 AVC 与 AAC-LC，正确处理 NAL/frame 边界、clear leader/trailer、skip pattern、CBC reset 和 emulation-prevention。
- [x] fMP4 支持 AVC/HEVC 与 AAC-LC 的 cbcs/cenc，解析 encv/enca、sinf/frma/schm/schi/tenc。
- [x] 支持 senc 与 saiz/saio，以及 sgpd/sbgp 覆盖；校验 box version、offset、sample 数和 subsample 总长。
- [x] 支持 per-sample/constant IV、多 KID、轮换、pattern/subsamples 和 clear/encrypted 混合，冲突或未知 scheme 明确失败。
- [x] 输出 clear sample entries，移除不适用的 protection metadata，保留 codec/timing。
- [x] demux 等待 key 可取消并受预算限制；完善 key/sample typed 诊断及策略预检。

验收使用独立 CBC/CTR 向量、外部生成的 encrypted/clear fixture 和 sample hash，覆盖所有保护信息路径、
损坏/冲突 metadata、NAL 归一化、epoch/范围/轮换，并进行独立容器解析和实际解码。
native/WASM 的 clear sample 与时间一致。TS HEVC SAMPLE-AES、TS/Packed AAC SAMPLE-AES-CTR 稳定拒绝。
Packed AAC 的正式输入及 SAMPLE-AES 组合随 v0.10 完成交付。

本地验收包括 98 个独立 fixture 文件、21 个 native/Node/Chrome sample 场景、40 个实际解码输出、
16 个真实 SDK native/Chrome 场景及 3 个 sample key Promise 取消用例。指标区分 raw/replay payload、
总分配量、WASM 页和 JS heap。详见 [v0.8.0 验收](../release-0.8.0.md) 与
[机器证据](../release-0.8.0-evidence.json)。发布与 SDK 产品上线另行执行。

## v0.9 开放输入与持续 session

交付 U05，将 v0.6–v0.8 已验证的 clear/加密路径接入 Live/EVENT，固定 variant 和轨道集合。

- [x] 原子接收 typed snapshot（descriptor 仅内部表示），媒体由 Source/provider 按需读取；重复去重、同槽改写和 generation 校验覆盖增量更新。
- [x] 定义已接受为身份校验后进入有界队列；空窗口不是 EOF，各 input 独立 end/ENDLIST。
- [x] EOF/时长限制和幂等 stop 停止接收并完成已接受数据，finalize/flush 后返回 EOF/stop/duration limit 原因；cancel 尽快终止 read/key/demux/write/finalize。
- [x] preparing/running/paused/draining/finalizing 和唯一终态可观测；完成边界前 cancel 优先，finalize 不重复输出。
- [x] 提供 VOD operation 内 pause/resume；live pause 采集语义由 SDK 明确，不承诺暂停期间无损或跨进程恢复。
- [x] 预算覆盖 probe、在途资源/bytes、lookahead、等待 key、双路 skew 和有界事件历史；慢 sink 抑制新增读取与接收。字幕归属 v0.10。
- [x] 缺片默认失败，显式 skip/report/split；一路落后有有界等待/超时，不静默补音或截断其他路。
- [x] 支持持续 epoch/拆分 sink 交接；fMP4 默认不累计全程 mfra，公开内存 bytes 与 classic finalize 索引成本。
- [x] U10 报告未知 total、录制时长、bytes、各路 discovered/downloaded/decrypted/committed、gap 和 end reason。

验收覆盖滚动窗口/EVENT/ENDLIST、无更新后追加、改写/回退/重启、不同切片周期和独立尾部、
慢 writer/满队列/key 等待、stop/cancel/EOF 竞态、writer/flush/finalize failure 及 drop 后无迟到事件。
长录制分别测量 WASM、JS、native 媒体及索引状态；资源大小上限不能被描述为总内存上限。
SDK close 成功后才发布公共完成；本版开放输入仍不具备新持久 checkpoint。

## v0.10 多音轨字幕与 Packed AAC

交付 U06–U08，基于共同时间映射和持续队列完成固定多轨 mux。

- [ ] 新多轨入口使用稳定 input ID/output track ID；role 只表达用途，明确内嵌音频保留/排除规则。
- [ ] 至少主视频加两条 AAC-LC 音轨，保留各轨独立 EOF、偏移和尾部，以及语言/name/default/来源元数据。
- [ ] 交付 M0 评审选定的字幕 sample entry，接受持续、带 track/epoch/时间区间的 sample，无需上游 WebVTT parser。
- [ ] 定义 cue 重叠、跨 fragment/epoch、空区间、settings/style 的保留或拒绝；字幕空隙不阻塞媒体 mux。
- [ ] Packed AAC 支持 ADTS AAC-LC 和 ID3 PRIV 时间锚，复用 33-bit 映射，缺失/非法锚不自动猜测；Packed Audio 不使用 MAP。
- [ ] Packed AAC 支持主音频/外置音轨、clear/AES-128/SAMPLE-AES，以及范围、epoch、live、配置变化与尾帧策略。
- [ ] 分别公布 classic MP4/fMP4 的音轨及字幕元数据映射、支持模式和目标播放器证据。

验收覆盖内嵌/外置/单路兼容、不同 sequence/切片周期、偏移/不等长、语言/default、
跨界/重叠/空 cue、多语言、样式保真、ID3/wrap/损坏 ADTS/配置变化，以及慢 sink 和解密组合。
容器写入元数据与播放器实际切轨/显示分别验收；没有证据的输出模式预检拒绝。
恢复身份在本版随 track/字幕配置补齐，跨进程恢复正式交付在 v1.0。

## v1.0 多输入恢复与实验解密

交付 U09 与 U02 GCM 实验，完成 U10 组合描述、错误及报告。

- [ ] 新版本 checkpoint 以 mux 已提交输出前缀为准，原子关联 byte/fragment 边界与每路 segment/sample 推进。
- [ ] 保存输出格式/durability/prefix 校验、generation、track/config/MAP、epoch/wrap/映射、范围/选轨/缺口策略及 key reference/version。
- [ ] 不保存 raw key、clear sample 或授权凭据；恢复重新 resolve key，先校验身份再截断未提交尾部并有界回读重建状态。
- [ ] 参数变化、MAP/key 改写和输出损坏返回 conflict/corruption，不能覆盖现有产物；lookahead 不作为已提交位置跳过。
- [ ] live 正常滚动不因整个 manifest 变化而 conflict；所需数据淘汰执行明确 fail/skip/split 并报告缺口。
- [ ] 保留 legacy schema v1/下游旧单路任务继续完成入口，实际旧产物覆盖回归；迁移仅对有证据的记录显式提供。
- [ ] 覆盖多音轨、字幕、Packed AAC、范围、epoch、解密和拆分输出的恢复；每个子输出关联完成状态与提交前缀。
- [ ] GCM 独立开关固定草案 -22 profile：32-byte key、资源内 16-byte IV、尾部 16-byte tag、KEY 不允许 IV 属性；认证前不提交明文。
- [ ] 可查询组合描述覆盖 method/container/scheme/codec/key source/source mode/output/range/resume/multi-track/subtitle/experimental。

验收在提交前后、fragment 中途、flush/sync/finalize 注入进程中断；恢复输出与完整运行比较 sample hash、
track/config、时间及实际范围。覆盖授权重建、key 重新获取、窗口淘汰、尾部残留/损坏和旧任务继续完成。
区分 Flush/SyncAll 与调用方 checkpoint/目录持久化保证；通用 borrowed writer 不声明持久恢复。
GCM 另以独立向量验证错误 tag 无明文提交，支持与拒绝组合单独列入 experimental 矩阵。

## 需求追踪与交付状态

状态按已验收的版本范围标记；后续组合仍按对应版本交付。勾选附实现、fixture、双运行时证据和支持限制。

| 需求 | 首次交付 | 完整交付与组合验收 | 当前状态 |
| --- | --- | --- | --- |
| U01 协议与资源身份 | v0.6 有限 typed 模型 | v0.9 增量身份；v1.0 恢复校验 | v0.9 增量身份见验收记录；新恢复归属 v1.0 |
| U02 AES-128 与 key session | v0.6 有限 VOD | v0.7 范围、v0.9 live、v1.0 恢复 | 有限 VOD、范围及 v0.9 live 组合见各版验收；新恢复归属 v1.0 |
| U03 sample 解密 | v0.7 hook 原型、v0.8 TS/fMP4 | v0.9 live、v0.10 Packed AAC/多轨、v1.0 恢复 | v0.8 正式 sample 解密已验收；v0.9 开放输入见本版记录 |
| U04 范围与 epoch | v0.7 | v0.9 持续映射、v0.10 字幕/多轨、v1.0 恢复 | v0.7 有限路径验收完成，已提交 `24c5ac4`；未发布 registry |
| U05 持续 session | v0.9 | v0.10 多轨/字幕；v1.0 恢复与长录制 | v0.9 本地候选完成；支持范围见验收记录 |
| U06 多音轨 | v0.10 | v1.0 恢复、播放器及 SDK 联调 | 待交付 |
| U07 字幕 mux | v0.10 | v1.0 提交/恢复、播放器及 SDK 联调 | 待交付 |
| U08 Packed AAC | v0.7 内部 hook、v0.10 完整输入 | v1.0 恢复及组合验证 | v0.7 内部 hook 已提交 `24c5ac4`；正式输入待 v0.10 |
| U09 多输入恢复 | v0.6 预留契约、v1.0 实现 | v1.0 故障恢复与旧任务验收 | 待交付 |
| U10 诊断报告与组合描述 | 随每个 minor 增量交付 | v1.0 覆盖完整并冻结正式契约 | v0.6–v0.8 有限路径与 sample 诊断已验收；v0.9 开放报告/组合查询见本版记录 |
| U02 GCM 实验扩展 | v1.0 独立 experimental profile | v1.0 实验矩阵与认证证据齐备，保持实验标记 | 待交付 |

## 下一阶段：v0.10

下一 minor 推进固定多音轨、字幕 mux 和 Packed AAC。持续 session 的实现及本地验收见
[v0.9 记录](../release-0.9.0.md)、[机器证据](../release-0.9.0-evidence.json)和
[P0–P4 实现映射](v0.9.0-implementation-plan.md)。SDK 产品上线与 registry 发布另行执行。

## 每个 minor 的共同发布门槛（后续版本模板）

v0.6–v0.7 上游本地门槛结果见 [v0.6 记录](../release-0.6.0.md) 和 [v0.7 记录](../release-0.7.0.md)。下列清单作为各后续版本和下游接入的共同门槛保留，不表示整个路线图均已完成。

每版支持声明限定为有证据的组合。manifest 可知条件提前预检，init/sample 阶段校验剩余条件；
不能用 `aes128=true` 或 `live=true` 代表任意组合。未知或不支持路径必须稳定拒绝。

- [ ] 公开契约、最小 native/WASM 调用例、迁移说明、限制和预算齐备；API 名称以实现评审为准。
- [ ] 确定性 fixture 记录生成来源、工具/命令、许可与 hash；成功、损坏与拒绝场景都有自动化证据。
- [ ] native 与实际 WASM 比较 clear sample hash、track/config、时间映射、范围、缺口/end reason 和 typed failure。
- [ ] 背压、取消、并发 operation、drop、Promise 拒绝/迟到结果及无迟到事件验证通过；新接口支持 Browser Promise 与无需 Send 的 writer。
- [ ] 独立容器解析/FFprobe/FFmpeg 验证通过；密码路径另用独立向量和 clear sample 对照，不仅判断可播放。
- [ ] 旧 API/schema v1、默认/serde/无默认 feature、wasm 编译与执行、fmt/Clippy 和相关 FFmpeg 后端回归通过。
- [ ] SDK 按已验收组合开启 capability；运行事件不冒充 checkpoint，key/资源 URI 等诊断完成敏感字段扫描。

现有证据入口为 [prepared 契约测试](../../tests/prepared_session.rs)、
[WASM 契约执行](../../scripts/test_wasm_session.mjs)、
[独立双路验证](../../scripts/verify_multi_input.py)、
[媒体 corpus](../../tests/fixtures/media/README.md) 和 [预算测量](../benchmarks.md)。
有限 AES-128 与 v0.9 开放输入 fixture 已有本地验收；多轨、字幕和新恢复 fixture 仍需补充，现有用例不代表这些后续路径已通过。

## v1.0 hls-engine 完整引擎交付与稳定化

v0.10 之后直接进入 1.0 开发，完成多输入恢复、GCM 实验和完整诊断报告后进入 RC；
RC 处理契约整理、联调和验收发现的问题。
所有本轮目标交付完整、包名与下游迁移验收通过后，以 `hls-engine` 发布 v1.0.0；
若任何门槛未满足，继续 1.0 prerelease，
显式更新版本归属与下游影响，不能仅以版本号升级代替能力完成，也不能静默缩减正式目标。

- [ ] U01–U10 正式范围全部完成；GCM 实验 profile 交付并单独验证，不宣称成为正式标准。
- [ ] 支持矩阵覆盖 clear TS/fMP4、AES-128、TS SAMPLE-AES、fMP4 cbcs/cenc、Packed AAC 与内嵌字幕的适用路径。
- [ ] 上述路径在有限 VOD、范围/epoch、Live/EVENT、多轨、classic/fMP4 输出及 native 文件恢复中完成适用组合验收；不适用格有拒绝证据。
- [ ] Browser 实际 Promise/writable 与 Node native SDK 联调通过；WASM/Node 执行不能替代真实浏览器 bridge 验收。
- [ ] 长录制覆盖跨 epoch/key rotation、慢 sink、独立轨尾部、字幕空隙、stop/cancel/finalize 和断点重建；分别记录 JS/WASM/native 内存与索引增长。
- [ ] classic MP4 的全量 bytes/索引成本、持续 fMP4 的预算和 mfra 策略公开，性能结论附输入规模、环境与测量方法。
- [ ] 旧 SDK 任务及 schema v1 的继续完成/显式迁移有真实产物证据，故障与淘汰窗口场景不污染已有输出。
- [ ] 目标播放器音轨切换和字幕显示证据齐备；格式保真限制与 SDK 能力说明一致。
- [ ] 正式 API、serde/JS 无损桥接、typed error、report、checkpoint schema 与兼容政策冻结；experimental 与正式契约分开列明。
- [ ] 正式 Cargo 包更名为 `hls-engine`，Rust crate 引用迁移为 `hls_engine`；同步包元数据、代码与示例引用、文档、CI 和发布脚本。
- [ ] WASM/native 桥接与分发产物中的包名、加载入口及 SDK 依赖同步迁移；hls-downloader 完成新包接入与锁定产物验证。
- [ ] 发布从 `hls-transmux` v0.x 到 `hls-engine` v1.0 的迁移说明，明确旧包维护策略、依赖与 import 替换、API 调整和 checkpoint 兼容；更名不静默使旧任务或恢复记录失效。
- [ ] 新名称下的 Cargo 发布包、无默认 feature 的 WASM、native/可选 FFmpeg 后端、平台 CI 与下游锁定产物验证通过，SDK M5 完成。

维护时为每个完成项登记 PR/commit、验证记录与剩余限制。发现某个组合不可行，须说明对
版本、SDK 阶段及 1.0 门槛的影响后调整规划。下方保留原 v0.2.1–v0.4.1 研究与交付记录；
其中候选顺序、延期项和当时发布状态仅作历史依据，后续工作以本次版本规划为准。

## 历史研究与交付记录

> 更新日期：2026-09-30
> 研究基线：v0.2.1，commit `11188ae`
> 状态：阶段 A 已随 v0.3.0 发布；阶段 B 已随 v0.4.0 发布；C1–C3 纳入 patch v0.4.1，工作区实现及验收完成，本次不发布。持续建设第 1、2 项已完成；C4 与其余持续建设待办保留。历史研究基线与风险判断保留供追溯。

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

**v0.2.1 依据：** [`read_from_slot`](../../src/source.rs) 在检查 `InFlight` 状态并释放锁后才创建 `notified()`；生产者使用 `notify_waiters()`，不保存通知 permit。
根据 [Tokio Notify 文档](https://docs.rs/tokio/latest/tokio/sync/struct.Notify.html)，该顺序存在消费者错过通知后持续等待的窗口。
这是原 v0.2.1 的竞态风险；v0.3.0 已用受控交错回归测试验证修复。

- [x] 先创建通知 future，再检查共享状态；或改为能保存状态的通知机制。
- [x] 用同步屏障控制生产者完成时机，补充确定性回归测试。
- [x] 覆盖 worker 创建 slot、consumer 自建 slot、失败通知等路径。

**验收：** 在“检查状态与进入等待之间完成下载”的受控交错下，消费者始终能返回；成功和失败路径均不永久等待。

### A2. 接入实际可中断的取消等待

**v0.2.1 依据：** [`CancelToken`](../../src/cancel.rs) 和选项文档描述了读取与取消竞争，但 [`demux_segment`](../../src/transmux.rs) 直接 await 资源读取，当前流水线没有调用 `CancelToken::cancelled()`。
取消主要在分片开始前检查，无法保证正在等待慢请求时迅速返回。

- [x] 在 playlist、init segment、media segment 读取处接入取消竞争。
- [x] 取消后停止本次任务的预取请求，包括 worker 和 consumer 自建 fetch。
- [x] 明确复用同一 `Source` 时的任务边界，避免依赖外部 `Arc` 被释放才结束后台工作。
- [x] 为阻塞 writer、finalize 定义取消行为；部分写入恢复与 A3 一起设计。
- [x] 明确 native CPU 密集工作是否需要 `spawn_blocking`，保留 wasm 兼容路径。

**验收：** 永不返回的 Source 不阻止取消；阻塞 sink 的取消行为可验证；任务退出后没有持续发起的后台请求。
测试使用明确超时阈值，避免依赖公网和机器速度。

### A3. 将正常暂停续传升级为崩溃恢复

**v0.2.1 依据：** [`transmux_fragmented_async`](../../src/transmux.rs) 以 append 模式打开已有文件。
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

**v0.2.1 依据：** [`StreamingMp4` 文件入口](../../src/transmux.rs) 在非取消的 stage-1 错误后删除临时文件，stage-2 返回后也删除临时文件，即使 finalize 失败。
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
- 验证：连续三分片 TS 与外部生成 fMP4 B 帧样本通过 FFprobe 的 DTS/PTS/duration/payload hash 对照与 FFmpeg 解码/seek；4.40 GB 稀疏输出由 FFprobe seek 到 32 位边界之外。RSS、耗时、吞吐和磁盘测量见 [benchmarks.md](../benchmarks.md)。小型独立媒体验证已接入 FFmpeg CI job。
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

- [x] 新增真实连续多分片 TS、HEVC、外部生成 fMP4、B 帧、VFR、纯音频和纯视频样本。
- [x] 使用 FFprobe 和 FFmpeg 做独立的结构、时间线及解码验证；本次按要求无需目标播放器验证。
- [ ] 为取消、并发、恢复、短写和 finalize 失败增加故障注入。
- [ ] 针对 HLS、TS、ISOBMFF 和 codec parser 建立 fuzz；限制异常 sample count 等输入引起的资源分配。
- [ ] 建立下载吞吐、首字节时间、峰值 RSS、finalize 耗时和取消响应基准。
- [ ] CI 加入 fmt、Clippy、无默认 feature、serde、wasm 和 FFmpeg 后端组合；FFmpeg 环境单独配置。
- [ ] 核心媒体改造完成前，先建立外部参考输出与测试断言，避免 parser 与 muxer 共享错误却相互验证通过。

**第 1、2 项实现与验收（2026-09-30，工作区，未发布）：**

- 样本与生成记录：[tests/fixtures/media/README.md](../../tests/fixtures/media/README.md) 和 `manifest.json`。14 组约 6 秒的独立 FFmpeg 编码输入，约 3.1 MiB；每组至少 3 个不同且时间连续的分片，不使用重复单片或人为平移时间戳。包含 AVC/AAC TS、HEVC/AAC TS、AVC/HEVC fMP4、B 帧、VFR、负 CTS、音频延迟、NAL 1/2 字节及多 trun，以及 TS/fMP4 纯音频和纯视频。保留工具版本、完整命令、派生变换、输入 SHA-256 和 FFprobe 参考计数/时间信息。
- 常规回归：[tests/media_corpus.rs](../../tests/media_corpus.rs) 无需 FFmpeg，在默认/无默认 feature 下读取固定样本、校验 hash、对照外部 codec/sample count，并验证三种文件输出的成功或明确拒绝边界。
- 独立验收：[scripts/verify_media.py](../../scripts/verify_media.py) 默认读取固定 corpus；`--generate` 可重新生成并验收。FFprobe 对照输入和输出的 track、sample count、DTS/PTS、duration、规范化 NAL/AAC 内容，时间容差一个输出 tick；检查经典 MP4 faststart 与分片 box 布局。FFmpeg 完整解码及 3 秒处 seek 覆盖输入和成功输出的每个存在轨道。验证样本确实包含 B 帧、VFR、负 CTS、音频偏移。
- 共 42 个样本/输出组合：34 个成功输出、8 个预期拒绝。纯 TS 三种输出均拒绝；纯 fMP4 支持 Fragmented/Native Streaming，batch 保持拒绝。拒绝时目标不存在或为空，不写入损坏媒体；Streaming 仍按恢复约定保留 partial。此项建立样本和兼容边界，未扩展 C4。
- FFmpeg CI job 已使用固定 corpus，不再每次生成临时输入。全 feature Cargo 测试 121 项通过、2 项既有手工测试 ignored；默认/无默认 feature corpus 回归、wasm 无默认 feature 编译、fmt 和 all-feature Clippy 通过；独立工具验收通过。本地环境为 macOS arm64，跨平台 CI 本次未在本地模拟。
- 边界：样本来自合成测试图形/音频的真实连续编码过程，不代表摄像机/生产采集媒体；不是长视频性能基准。生成器版本不同可能改变二进制，重新生成需要审阅媒体和 manifest。纯 AAC TS 的参考包读取拼接后的物理分片，规避当前 FFprobe HLS demuxer 首包重复报告。按用户要求不执行播放器验证。

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
- 单资源上限已实现；总预取字节预算本次仅交付测量和背压设计，见 [benchmarks.md](../benchmarks.md)。并发 slot 限制不是整个调用的字节上限。
- 不支持复杂 edit list、不同配置参数集、多 sample description、隐式跨 traf 布局；不连续 decode 时间线明确拒绝。仅一次换算造成的一 tick 间隙可修正。C4 的加密、discontinuity、alternate audio、live 保持拒绝。
- 跨平台由既有 CI matrix 验证；本轮本地 macOS。两个手工大文件/RSS 测试保持 ignored，可按基准文档单独运行。
格式和 lint 清理单独提交，便于审阅行为变更。

## 8. 维护约定

- 完成待办时附上对应 PR 或 commit、验证结果及剩余限制，再勾选状态。
- 待复现风险在加入回归测试后更新为已复现、已排除或已修复，保留判断依据。
- 性能结论附上输入规模、运行环境和测量方法，不用单个样本替代长视频基准。
- 新功能按实际接入需求重新排序；公开 API 或 checkpoint 变化说明兼容影响。
