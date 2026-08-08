# 架构变更历史

## 2026-08-08 - 初始架构分析

### 审查信息
- **审查者**：AI + Subagent
- **AI 评分**：B
- **Subagent 评分**：C
- **审查时间**：2026-08-08

### 关键发现

#### AI 的分析
- 识别 7 个架构层次
- 识别分层架构 + 混合语言后端模式
- 发现 5 个潜在问题
- 提出 5 个改进建议（P0-P3）

#### Subagent 的审查
- **发现 AI 的重大误判**：
  1. "acp-sdk → renderer 反向依赖"是误报（docs/acp-sdk/ 是独立仓库被误索引）
  2. "src 包 fan-out=437"是索引伪影（CodeGraph package detection 问题）
- **识别 AI 遗漏的问题**：
  1. Rust `cross_agent/` (2500+ 行) 和 `orchestration/` (1400+ 行) 是真复杂度集中地
  2. `src/main/lib/trpc/routers/chats.ts` 2196 行，main 层逻辑开始塌陷
  3. `.deprecated-engines/` 仍被识别为 entry_points，污染打包
  4. 测试覆盖不足，无测试策略评估

#### 最终决策
- **采纳 Subagent 分析**：撤销 AI 的 P0/P1 建议
- **重新排序优先级**：
  - P0：修正 CodeGraph 索引范围
  - P1：删除 deprecated 和孤立 demo
  - P1：拆分 god file（chats.ts, cross_agent/*.rs）
  - P2：处理 docs/acp-sdk/ git 状态
  - P2：消除 agent 模块职责重叠
  - P3：补充测试策略评估

### 架构文件创建
- ✅ ARCHITECTURE.md（架构概览）
- ✅ DEPENDENCIES.md（依赖关系）
- ✅ RULES.md（架构规则）
- ✅ CHANGELOG.md（本文件）
- ✅ REVIEW.md（Subagent 审查记录）

### 用户确认
- ✅ 同意采纳 Subagent 分析结果
- ✅ 同意立即修正 CodeGraph 索引

### P0 执行结果（2026-08-08）
- **发现**：CodeGraph MCP 工具不支持 `.codegraphignore` 文件
- **解决方案**：手动临时移动问题目录，重新索引，移回目录
- **执行步骤**：
  1. 临时移动 `docs/acp-sdk/`, `network-demo/`, `.deprecated-engines/` 到 `/tmp/`
  2. 删除 `.codegraph/` 索引
  3. 重新调用 `index_repository`
  4. 移回目录
- **结果**：
  - 节点数：12,846 → 8,016（-37%）
  - 边数：46,126 → 21,123（-54%）
  - `acp-sdk` 包从索引中消失
  - 虚假的 `acp-sdk → renderer: 109 calls` 依赖消失
  - entry_points 不再包含 `.deprecated-engines/`
- **状态**：✅ P0 完成，架构分析现在准确可信

---

## 待办事项

### P0 - 已完成 ✅
- [x] ~~修正 CodeGraph 索引范围~~
- [x] 手动执行：临时移动 docs/acp-sdk/, network-demo/, .deprecated-engines/，重新索引
- [x] 验证：节点数 12,846 → 8,016（-37%），边数 46,126 → 21,123（-54%）
- [x] 验证：acp-sdk 包消失，虚假依赖消失，entry_points 正确

### P1 - 本周执行
- [ ] 删除 `.deprecated-engines/` 目录
- [ ] 删除 `network-demo/` 目录
- [ ] 拆分 `src/main/lib/trpc/routers/chats.ts`（2196 行）
- [ ] 拆分 `src-rust/src/cross_agent/task_coordinator.rs`（811 行）
- [ ] 拆分 `src-rust/src/cross_agent/agent_launcher.rs`（816 行）

### P2 - 本月执行
- [ ] 处理 `docs/acp-sdk/` git 状态（submodule 或移出）
- [ ] 评估并消除 agent 模块职责重叠
- [ ] 补充测试策略评估

### P3 - 未来执行
- [ ] 完善测试覆盖
- [ ] 统一命名规范（如有不一致）
