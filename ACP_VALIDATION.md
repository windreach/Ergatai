# ACP Protocol Validation Test

最小化验证测试，确保 ACP 协议通信流程能跑通。

## 前置条件

1. **安装 ACP 兼容的 agent**（至少一个）：
   - `claude-agent-acp` 或
   - `codex-acp` 或
   - 其他支持 ACP 协议的 agent

2. **配置 agent**：
   ```bash
   # 创建配置文件
   mkdir -p ~/.config/ergatai/agents
   
   # 示例：claude-code.json
   cat > ~/.config/ergatai/agents/claude-code.json << 'EOF'
   {
     "name": "claude-code",
     "command": "claude-agent-acp",
     "args": [],
     "env": {}
   }
   EOF
   ```

## 运行测试

### 方式 1：使用 claude-code（默认）
```bash
cd src-rust
cargo test --release acp_validation -- --nocapture
```

### 方式 2：指定其他 agent
```bash
TEST_AGENT=codex cargo test --release acp_validation -- --nocapture
```

### 方式 3：指定工作目录
```bash
TEST_CWD=/path/to/project cargo test --release acp_validation -- --nocapture
```

## 测试流程

测试会验证以下完整流程：

```
1. 加载 agent 配置
   ↓
2. Spawn agent 进程
   ↓
3. ACP initialize 握手
   ↓
4. 创建 session (new_session)
   ↓
5. 发送 prompt
   ↓
6. 接收响应 + 事件通知
   ↓
7. 关闭 session
   ↓
8. 清理资源
```

## 预期输出

成功运行应该看到：

```
=== ACP Protocol Validation Test ===

📋 Testing with agent: claude-code

🔍 Step 1: Loading agent config...
✅ Config loaded for claude-code
   Command: claude-agent-acp

🚀 Step 2: Spawning ACP session...
   This will start the agent process and initialize ACP protocol
   Working directory: /path/to/project

⏳ Waiting for session creation...
✅ Session created successfully!
   Session ID: session-xxx

🔍 Step 3: Verifying session registration...
✅ Session registered in session manager

📊 Step 4: Listing active sessions...
Active sessions: 1
  - Session: session-xxx
    Agent: claude-code
    CWD: /path/to/project

💬 Step 5: Sending test prompt...
   Prompt: Say 'ACP protocol test successful' and nothing else.
   ⏳ Waiting for response...

✅ Prompt completed successfully!

📡 Step 6: Checking event queue...
Events collected: N
  [0] agent_message_chunk
      Text: ACP protocol test successful

🛑 Step 7: Closing session...
✅ Close command sent

🔍 Step 8: Verifying cleanup...
Active sessions after close: 0
✅ Session cleaned up successfully

=== Test Complete ===

✅ ACP protocol flow validated successfully!
   - Session creation: ✅
   - Agent initialization: ✅
   - Prompt/response: ✅
   - Event notifications: ✅
   - Session cleanup: ✅
```

## 故障排查

### 问题 1：Agent config not found
```bash
# 确保配置文件存在
ls ~/.config/ergatai/agents/claude-code.json
```

### 问题 2：Session creation timeout
- 检查 agent 命令是否正确
- 检查 agent 是否支持 ACP 协议
- 查看详细日志：`RUST_LOG=debug cargo test ...`

### 问题 3：Prompt timeout
- 检查 agent 进程是否正常运行
- 检查网络连接（如果使用远程 agent）
- 查看 agent 自身的日志输出

### 问题 4：Event notifications not received
- 检查 `sdk_session.rs` 中的 notification handler
- 确认事件是否正确转发到 event_tx

## 手动测试

如果不想运行自动化测试，可以手动验证：

```bash
# 1. 启动 Electron 应用
cd /home/yubing/code/ergatai-desktop
bun run dev

# 2. 在开发者控制台执行
await acp_create_session("claude-code", "/path/to/project")
# 应该返回 session_id

await acp_send_prompt(session_id, "hello")
# 应该看到 agent 响应

acp_list_sessions()
# 应该看到活跃会话

await acp_close_session(session_id)
# 应该成功关闭
```

## 下一步

测试通过后，可以继续验证：
1. **Pool 并发测试** - 多个 agent 实例并行工作
2. **任务队列测试** - 任务排队和负载均衡
3. **错误恢复测试** - agent 崩溃后的重试机制
