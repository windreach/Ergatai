# 修复计划 - Ergatai 项目

## 执行顺序

### 第零步：修复预先存在的编译错误

#### 0.1 修复正则表达式语法错误
- **文件：** `crates/ergatai-collab/src/message_router.rs:23`
- **错误类型：** clippy::invalid_regex
- **修复类型：** 修改正则表达式语法
- **修复步骤：**
  1. 移除 look-around 语法（`(?<=\s)`）
  2. 使用捕获组替代
  3. 调整匹配逻辑以处理前导空白
- **修复代码：**
  ```rust
  // 当前（错误）
  static AT_MENTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
      Regex::new(r"(?m)(?:^|(?<=\s))@([a-zA-Z0-9_-]+)").expect("valid regex")
  });

  // 修复后
  static AT_MENTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
      Regex::new(r"(?m)(?:^|\s)@([a-zA-Z0-9_-]+)").expect("valid regex")
  });
  ```
- **验证方法：** `cargo clippy --all-targets` 通过
- **风险：** 低 - 只是语法调整，语义相同

### 第一步：修复高危问题

（无额外高危问题）

### 第二步：修复中等问题（建议尽快修复）

#### 2.1 减少 ergatai-lock 中的 unwrap() 使用
- **文件：** `crates/ergatai-lock/src/lock_manager.rs`
- **修复类型：** 错误处理改进
- **修复步骤：**
  1. 识别测试代码中的 unwrap()
  2. 使用 `?` 操作符替代部分 unwrap()
  3. 使用 `assert!` 替代布尔断言的 unwrap()
- **验证方法：** `cargo test -p ergatai-lock` 通过

#### 2.2 减少 ergatai-nats 中的 unwrap() 使用
- **文件：** `crates/ergatai-nats/src/*.rs`
- **修复类型：** 错误处理改进
- **修复步骤：**
  1. 识别生产代码中的 unwrap()
  2. 使用 proper error handling（`?` 或 match）
- **验证方法：** `cargo test -p ergatai-nats` 通过

#### 2.3 预分配 Vec 容量
- **文件：** 整个项目（43 处）
- **修复类型：** 性能优化
- **修复步骤：**
  1. 识别可以预分配容量的 Vec::new()
  2. 使用 `Vec::with_capacity(n)` 替代
- **验证方法：** 性能基准测试（如有）

### 第三步：修复低危问题（可选，代码清理）

#### 3.1 清理示例代码中的未使用警告
- **文件：** `examples/simple-agent/src/main.rs`
- **修复类型：** 代码清理
- **修复步骤：**
  1. 移除未使用的字段和变量
  2. 移除未使用的导入
- **验证方法：** `cargo build --examples` 无警告

## 修复验证清单

- [ ] 所有预先存在的编译错误已修复
- [ ] 所有高危问题已修复
- [ ] 所有中等问题已修复或标记为"接受风险"
- [ ] 代码编译通过（`cargo check`）
- [ ] 所有测试通过（`cargo test`）
- [ ] Clippy 检查通过（`cargo clippy`）
- [ ] 无新引入的问题（diff 阶段 0 基线 vs 修复后）
