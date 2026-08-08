# CodeGraph 索引修复说明

## 问题描述

CodeGraph 在初始索引时，将以下目录错误地包含在项目索引中：
1. `docs/acp-sdk/` - 独立的 ACP SDK 参考实现仓库（2209 节点）
2. `network-demo/` - 独立的网络演示代码
3. `.deprecated-engines/` - 废弃的 Claude 引擎代码
4. `out/` - 构建产物

这导致依赖分析结果失真：
- 虚假的"acp-sdk → renderer 109 calls 反向依赖"
- 虚假的"src 包 fan-out=437"
- 错误的 entry_points 识别

## 解决方案

### ⚠️ 工具限制
CodeGraph MCP 工具**不支持 `.codegraphignore` 文件**。重新索引时仍会包含所有目录。

### 方案 1：手动排除（当前可行）

**临时移动问题目录**：

```bash
# 1. 创建临时目录
mkdir -p /tmp/codegraph-exclude

# 2. 临时移动问题目录
mv docs/acp-sdk /tmp/codegraph-exclude/
mv network-demo /tmp/codegraph-exclude/
mv .deprecated-engines /tmp/codegraph-exclude/

# 3. 删除旧索引
rm -rf .codegraph/

# 4. 通过 MCP 工具重新索引
# 调用 index_repository(repo_path="/home/yubing/code/ergatai")

# 5. 移回目录
mv /tmp/codegraph-exclude/acp-sdk docs/
mv /tmp/codegraph-exclude/network-demo ./
mv /tmp/codegraph-exclude/.deprecated-engines ./
```

**预期结果**：
- 节点数从 12,846 减少到 ~9,000-10,000
- 不再包含 `acp-sdk` 作为独立包
- entry_points 不再包含 `.deprecated-engines/`
- boundaries 不再显示虚假的 `acp-sdk → renderer` 依赖

### 方案 2：手动排除（如果 .codegraphignore 不支持）
如果 CodeGraph 不支持 `.codegraphignore`，需要在索引时手动指定排除路径。

**临时解决方案**：
1. 暂时移动问题目录到临时位置
2. 重新索引
3. 移回目录

```bash
# 临时移动
mkdir -p /tmp/codegraph-exclude
mv docs/acp-sdk /tmp/codegraph-exclude/
mv network-demo /tmp/codegraph-exclude/
mv .deprecated-engines /tmp/codegraph-exclude/

# 删除旧索引
rm -rf .codegraph/

# 重新索引（通过 MCP 工具）

# 移回目录
mv /tmp/codegraph-exclude/acp-sdk docs/
mv /tmp/codegraph-exclude/network-demo ./
mv /tmp/codegraph-exclude/.deprecated-engines ./
```

## 验证步骤

重新索引后，验证以下内容：

1. **检查节点数**：应该显著减少（从 12,721 减少约 3,000-4,000）
   ```
   预期：~8,000-9,000 节点
   ```

2. **检查包列表**：不应该包含 `docs/acp-sdk` 作为独立包
   ```bash
   # 通过 MCP 工具调用 get_architecture
   # 检查 packages 列表
   ```

3. **检查依赖关系**：
   - 不应该有 `acp-sdk → renderer` 的调用
   - `src` 包的 fan-out 应该更合理

4. **检查 entry_points**：
   - 不应该包含 `.deprecated-engines/claude-lib/` 中的函数

## 影响评估

### 修复后的预期结果
- ✅ 依赖分析更准确
- ✅ 架构评估更可信
- ✅ 不会再出现"反向依赖"误报
- ✅ src 包的 fan-out 数字会更合理

### 注意事项
- 重新索引需要 5-10 分钟
- 索引期间 CodeGraph 工具不可用
- 建议在非工作时间执行

## 后续维护

### 添加新的排除目录
编辑 `.codegraphignore` 文件，添加需要排除的路径。

### 定期检查
每次大规模代码变更后，检查索引是否仍然准确：
```bash
# 通过 MCP 工具调用 get_architecture
# 检查 packages、boundaries、entry_points
```
