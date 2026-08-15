# Ergatai PoC - 终端复用器消息注入

验证通过 tmux 向运行中的 agent 注入消息的可行性。

## 原理

```
终端复用器 (tmux) 控制 PTY (伪终端)
    ↓
通过 send-keys 模拟键盘输入
    ↓
注入到 agent 的 stdin
    ↓
Agent 以为是用户输入，处理并显示
```

## 文件说明

- `poc-tmux-injection.sh` - 启动 PoC（创建 tmux session 并启动 agent）
- `poc-inject.sh` - 注入消息到 agent
- `poc-cleanup.sh` - 清理资源

## 使用方法

### 1. 启动 PoC（默认使用 claude）

```bash
chmod +x poc-*.sh
./poc-tmux-injection.sh
```

或指定其他 agent：

```bash
./poc-tmux-injection.sh opencode
./poc-tmux-injection.sh aider
```

### 2. 查看 agent（在另一个终端）

```bash
tmux attach -t ergatai-poc
```

你会看到 agent 的 TUI 界面正常运行。

### 3. 注入消息（在另一个终端）

```bash
./poc-inject.sh "请帮我写一个 hello world 程序"
```

### 4. 观察结果

回到 tmux session，观察 agent 是否：
- ✅ 接收到注入的消息
- ✅ 像处理用户输入一样处理它
- ✅ 在 TUI 中显示响应

### 5. 清理

```bash
./poc-cleanup.sh
```

## 验证要点

1. **Agent TUI 是否正常** - 界面应该完整显示，交互正常
2. **消息是否被接收** - 注入的消息应该出现在 agent 的输入区域
3. **Agent 是否处理** - agent 应该像处理用户输入一样处理注入的消息
4. **输出是否正常** - agent 的响应应该正常显示在 TUI 中

## 预期结果

如果 PoC 成功：
- ✅ 证明终端复用器方案可行
- ✅ 可以在保留 TUI 的情况下注入消息
- ✅ 为 Ergatai 的多 agent 协作提供技术基础

如果失败：
- ❌ Agent 可能不接受外部注入
- ❌ 或者 TUI 渲染有问题
- ❌ 需要探索其他方案

## 下一步

如果 PoC 成功，下一步：
1. 构建 multiplexer wrapper（自动化管理）
2. 集成到 Ergatai（通过 socket/API 控制）
3. 实现完整的消息路由
4. 测试多 agent 协作场景
