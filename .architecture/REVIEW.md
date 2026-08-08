# Subagent 审查记录

## 2026-08-08 - 初始架构审查

### Subagent 信息
- **Agent Type**: general-purpose
- **审查时间**：2026-08-08
- **审查时长**：124 秒
- **Token 使用**：22,083
- **工具调用**：18 次

### 审查结果
- **整体评分**：C
- **与 AI 差异**：5 处重大差异
- **新增问题**：6 个
- **最终建议**：部分同意 AI 分析，但核心依赖结论要推翻

### 关键发现

#### 1. AI 的重大误判 ❌

**误判 1："acp-sdk → renderer 109 calls 反向依赖"**
- **AI 观点**：这是严重架构违反，建议 P0 解耦
- **Subagent 验证**：`docs/acp-sdk/` 是独立 git 仓库（有自己的 `.git/` 目录、独立 Cargo.toml），被 CodeGraph 误索引为项目内部模块。grep `^import.*from.*renderer` 在该目录零命中。
- **结论**：**撤销 AI 的 P0 建议**，问题不存在

**误判 2："src 包 fan-out=437 职责不清"**
- **AI 观点**：这是架构问题，建议 P1 清理
- **Subagent 验证**：CodeGraph 把 `src/` 当成单一 package（因为它没有子 package 目录），然后把其中所有外部调用都算作"src"这个 package 的 fan-out。实际上 `src/` 下是 `renderer/`、`main/`、`preload/`、`shared/`、`native-binding.*` 四个层级。
- **结论**：**撤销 AI 的 P1 建议**，是工具配置问题，不是代码问题

#### 2. AI 遗漏的真实问题 ❗

**遗漏 1：CodeGraph 索引污染**
- **问题**：`docs/acp-sdk/`（2209 节点）、`network-demo/`、`.deprecated-engines/`、`out/` 被错误索引
- **影响**：所有基于数字的架构判断都不可信
- **建议**：P0 - 修正索引范围（5 分钟配置修复）

**遗漏 2：Rust 层复杂度集中地**
- **问题**：`cross_agent/` (2500+ 行) 和 `orchestration/` (1400+ 行) 是真复杂度集中地
- **具体文件**：
  - `task_coordinator.rs` (811 行) - god file
  - `agent_launcher.rs` (816 行) - god file
- **建议**：P1 - 按"调度 / 启动 / 生命周期"三块拆开，每个 < 300 行

**遗漏 3：main 层逻辑塌陷**
- **问题**：`src/main/lib/trpc/routers/chats.ts` 是 2196 行
- **影响**：main 层在逻辑组织上开始塌陷为"routers 一把梭"
- **建议**：P1 - 按子领域拆成 `chats/messages.ts`、`chats/sub-chats.ts`、`chats/worktree.ts`

**遗漏 4：deprecated 残留比 AI 判断的更严重**
- **问题**：`.deprecated-engines/claude-lib/` 仍被 CodeGraph 识别为 entry_points（`buildClaudeEnv`、`getBundledClaudeBinaryPath` 等）
- **影响**：不只是"目录还在"，是真正可能污染打包和工具链的活代码
- **建议**：P1 - 删除前 `git log -- .deprecated-engines` 确认无活引用

**遗漏 5：测试覆盖不足**
- **问题**：Rust 层有 `src-rust/tests/` 但 AI 没评估覆盖率和质量；TypeScript 侧几乎看不到测试文件
- **影响**：对一个"性能关键路径在 Rust"的项目，没有提到测试策略是重大遗漏
- **建议**：P3 - 补充测试策略评估

**遗漏 6：agent 模块职责重叠**
- **问题**：`src-rust/src/agent/` 模块 1784 行，包含 config / discovery / custom_harness / runtime_metadata / global_config 五个关注点。这是从旧 `claude-lib` 迁移过来的 agent 抽象，和 `cross_agent/agent_launcher.rs` 存在职责重叠（两个地方都在做"启动 agent"）
- **建议**：P2 - 先 grep 确认重叠范围，再决定合并方向

### 确认的架构决策 ✅

1. **分层方向正确**：renderer/main/preload/native-binding/Rust 五层物理分离，Electron 约定下合理
2. **tRPC 端到端类型安全**：验证 renderer → main 仅有 1 处 type-only import，边界干净
3. **Rust 通过 NAPI 暴露 + TS 做业务编排**：符合"性能关键路径在 Rust"原则
4. **SQLite + Drizzle 本地优先**：local-first 桌面应用的合理默认
5. **P2 移除 deprecated / 隔离文档示例的方向正确**：但优先级应该更高

### 优先改进建议（Subagent 版本）

| 优先级 | 任务 | 原因 | 工作量 |
|--------|------|------|--------|
| **P0** | 修正 CodeGraph 索引范围 | 排除 docs/acp-sdk/、network-demo/、.deprecated-engines/、out/，否则所有分析不可信 | 5分钟配置 |
| **P1** | 删除 .deprecated-engines/ 和 network-demo/ | 前者污染 entry_points，后者是孤立演示 | 10分钟 + grep 验证 |
| **P1** | 拆分 god file | chats.ts (2196行)、cross_agent/*.rs (800+行) | 2-4小时 |
| **P2** | 处理 docs/acp-sdk/ git 状态 | 独立 .git 但不在 .gitmodules，是 git 怪胎 | 30分钟决策 |
| **P2** | 消除 agent 模块职责重叠 | src-rust/src/agent/ 与 cross_agent/agent_launcher.rs 重叠 | 需先 grep 评估 |
| **P3** | 补充测试策略评估 | Rust 有测试，TS 侧几乎无测试 | 需评估 |

### 最终建议

**部分同意 AI 的架构分析，但核心依赖结论要推翻。**

AI 对"分层架构 + 混合语言后端"的定性判断正确，对 tRPC/本地优先/Rust 胶水层的评价也站得住脚。但它给出的"依赖关系"部分（反向依赖、fan-out 数字、src 包混乱）是 CodeGraph 把 vendored SDK 仓库误索引为项目模块导致的数字幻觉——实际代码中不存在"acp-sdk 反向调用 renderer"的问题。

真正值得工程团队投入的三件事：
1. 修正索引范围，让后续分析可信
2. 删除 deprecated 和孤立 demo，减小认知负担
3. 拆分已经长到 2000+ 行 / 800+ 行的 god file（chats.ts、cross_agent/*.rs、main/index.ts），在它们继续膨胀前动手——现在拆成本最低

AI 的 P0/P1 建议（解耦 SDK、清理 src 包）应该降级或撤销；它低估的 Rust 大文件和 deprecated 残留应该升级。总体方向不坏，但数字不可信，导致优先级排错了。

---

## 审查意见原文

> 我通过 grep 验证了 `docs/acp-sdk/` 中无 `import.*from.*renderer`，确认"反向依赖"是误报。CodeGraph 把独立 git 仓库（有 .git/ 目录、独立 Cargo.toml）误索引为项目内部模块，导致所有依赖分析都不可信。
> 
> 真正的问题是：
> 1. CodeGraph 索引污染（P0，5分钟修复）
> 2. deprecated 残留污染 entry_points（P1）
> 3. god file 需要拆分（P1，chats.ts 2196行、cross_agent/*.rs 800+行）
> 
> AI 的定性判断正确，但定量分析基于错误数据，导致优先级排错。建议采纳 Subagent 的优先级排序。

---

## 用户决策

- ✅ 同意采纳 Subagent 分析结果
- ✅ 同意立即修正 CodeGraph 索引（P0）
