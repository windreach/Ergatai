# tmux 简单教程（图文版）

## 🎯 最简单的开始

**运行这个脚本，自动帮你搞定一切：**

```bash
./tmux-easy-setup.sh
```

这会自动：
- ✅ 创建 tmux session
- ✅ 分割成 3 个窗口
- ✅ 在每个窗口启动程序

---

## 📺 查看结果

```bash
tmux attach -t ergatai-test
```

你会看到这样的界面：

```
┌──────────────────┬──────────────────┐
│                  │                  │
│   Agent A        │   Agent B        │
│   (pane 0)       │   (pane 1)       │
│                  │                  │
├──────────────────┴──────────────────┤
│                                      │
│            Agent C                   │
│            (pane 2)                  │
│                                      │
└──────────────────────────────────────┘
```

---

## 🎮 tmux 快捷键（超简单）

### 1. 切换窗口

```
按 Ctrl+B（同时按，然后松开）
然后按 方向键（← → ↑ ↓）
```

**示例：**
- `Ctrl+B` 然后 `→` = 切换到右边的窗口
- `Ctrl+B` 然后 `↓` = 切换到下面的窗口

### 2. 退出 tmux（不关闭程序）

```
按 Ctrl+B
然后按 D（Disconnect）
```

**效果：** 回到普通终端，但 tmux 里的程序还在运行

### 3. 重新进入 tmux

```bash
tmux attach -t ergatai-test
```

---

## 🔧 常用命令

### 查看所有 session

```bash
tmux ls
```

### 关闭 session

```bash
./poc-cleanup.sh
# 或者
tmux kill-session -t ergatai-test
```

### 向指定窗口发送消息

```bash
# 向 pane 1 发送消息
tmux send-keys -t ergatai-test:0.1 "Hello Agent B!" Enter

# 向 pane 2 发送消息
tmux send-keys -t ergatai-test:0.2 "Hello Agent C!" Enter
```

---

## 🎬 完整操作流程

### 步骤 1: 自动设置

```bash
./tmux-easy-setup.sh
```

### 步骤 2: 查看

```bash
tmux attach -t ergatai-test
```

### 步骤 3: 切换窗口

```
按 Ctrl+B
按 → 切换到 Agent B
```

### 步骤 4: 在另一个终端注入消息

```bash
# 新开一个终端窗口
tmux send-keys -t ergatai-test:0.1 "来自外部的消息" Enter
```

### 步骤 5: 回到 tmux 查看结果

```bash
tmux attach -t ergatai-test
# 切换到 pane 1，你会看到消息
```

### 步骤 6: 退出

```
按 Ctrl+B
按 D（退出 tmux）

./poc-cleanup.sh（清理）
```

---

## 💡 小贴士

### 如果卡住了

```bash
# 强制关闭所有 tmux
tmux kill-server
```

### 如果找不到 session

```bash
# 查看所有 session
tmux ls
```

### 如果想重新开始

```bash
# 清理
./poc-cleanup.sh

# 重新设置
./tmux-easy-setup.sh
```

---

## 🎯 总结

**你只需要记住 3 个操作：**

1. **切换窗口**: `Ctrl+B` 然后 `方向键`
2. **退出 tmux**: `Ctrl+B` 然后 `D`
3. **重新进入**: `tmux attach -t ergatai-test`

**就这么多！** 🎉
