//! DAG YAML parser
//!
//! 使用 serde_yaml 将 YAML 格式的 DAG 定义解析为 TaskGraph。
//! 替代原有的 Markdown 手写解析器，提供强类型、schema 验证和更好的 AI 生成兼容性。
//!
//! ## YAML 格式
//!
//! ```yaml
//! # 全局信息（可选）
//! name: feature-implementation
//! description: 实现新功能的协作流程
//!
//! # 任务列表
//! tasks:
//!   # 最简节点 — 只需 name
//!   - name: 代码审查
//!
//!   # 完整节点 — 所有字段
//!   - name: 前端实现
//!     agent: frontend-dev          # 执行 agent
//!     task: tasks/frontend.md      # 任务描述文件路径
//!     depends_on: [架构设计]        # 依赖的上游任务名列表
//!     input: "{{user_query}}"      # 输入模板
//!     output: dist/output.js       # 输出路径
//!     priority: high               # 优先级
//!     timeout: 300                 # 超时秒数
//!     retry: 2                     # 最大重试次数
//!     scope: "src/frontend/**"     # 文件访问范围（glob）
//! ```

use std::collections::HashMap;

use ergatai_error::{ErgataiError, ErgataiResult};
use serde::Deserialize;
use uuid::Uuid;

use crate::dag_topology::{TaskGraph, TaskNode, TaskStatus};

// ── YAML Schema (serde 反序列化目标) ──

/// YAML 顶层结构
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Schema fields: name/description are optional metadata, consumed by serde
struct YamlDag {
    /// DAG 名称（可选）
    name: Option<String>,
    /// DAG 描述（可选）
    description: Option<String>,
    /// DAG 全局超时秒数（可选）- 整个 DAG 的截止时间
    timeout: Option<u64>,
    /// DAG 全局 agent 调用次数上限（可选）- 超过后 DAG 被最终化为失败
    #[serde(skip_serializing_if = "Option::is_none")]
    max_agent_calls: Option<u64>,
    /// DAG 全局停滞超时秒数（可选）- 无进展超过此时长则 DAG 被最终化
    #[serde(skip_serializing_if = "Option::is_none")]
    stall_timeout_secs: Option<u64>,
    /// DAG 全局优先级（可选）- 应用于所有节点，除非节点自己指定优先级
    priority: Option<String>,
    /// 参数定义列表（可选）- 用于模板参数化
    #[serde(default)]
    parameters: Vec<YamlParameter>,
    /// 任务列表
    tasks: Vec<YamlTask>,
    /// 通信模式（可选）- DAG 内 agent 之间如何交流：open / adjacent / star:{hub_agent}
    communication: Option<String>,
}

/// YAML 参数定义
#[derive(Debug, Deserialize, Clone)]
struct YamlParameter {
    /// 参数名称
    name: String,
    /// 参数描述（可选）
    #[allow(dead_code)]
    description: Option<String>,
    /// 默认值（可选）
    default: Option<serde_yaml::Value>,
    /// 是否必需（可选，默认 false）
    #[serde(default)]
    required: bool,
    /// 参数类型（可选，用于验证）：string, number, boolean
    param_type: Option<String>,
}

/// YAML 单个任务节点
#[derive(Debug, Deserialize)]
struct YamlTask {
    /// 任务名称（必填，用于 depends_on 引用和显示）
    name: String,
    /// 执行 agent 标识
    #[serde(default = "default_agent")]
    agent: String,
    /// 任务描述文件路径
    task: Option<String>,
    /// 依赖的上游任务名称列表
    #[serde(default)]
    depends_on: Vec<String>,
    /// 父任务名（自动合并到 depends_on）
    parent: Option<String>,
    /// 输入模板
    input: Option<String>,
    /// 输出路径
    output: Option<String>,
    /// 优先级
    priority: Option<String>,
    /// 超时秒数
    timeout: Option<u64>,
    /// 最大重试次数（兼容 retry 和 max_retries 两种写法）
    #[serde(alias = "max_retries")]
    retry: Option<u32>,
    /// 文件访问范围（glob 模式）
    scope: Option<String>,
    /// 条件表达式（可选）- 只有条件为真时才执行此节点
    /// 示例: "{{test.exit_code}} == 0"
    condition: Option<String>,
    /// 额外自定义字段（通过 flatten 收集）
    #[serde(flatten)]
    metadata: HashMap<String, serde_yaml::Value>,
}

fn default_agent() -> String {
    "agent".to_string()
}

// ── 公开 API ──

/// 解析 YAML 格式的 DAG 定义为 TaskGraph
///
/// 自动处理：
/// - 任务名 → UUID 映射
/// - depends_on 名称引用 → UUID 引用
/// - parent → depends_on 合并
/// - scope glob 模式验证
/// - 重复任务名检测
///
/// # Arguments
/// * `content` - YAML 内容
/// * `params` - 可选的参数值映射，用于模板参数化
///
/// # Returns
/// 解析后的 TaskGraph
pub fn parse_dag_yaml(
    content: &str,
    params: Option<HashMap<String, serde_json::Value>>,
) -> ErgataiResult<TaskGraph> {
    let yaml_dag: YamlDag = serde_yaml::from_str(content)
        .map_err(|e| ErgataiError::InvalidArgument(format!("YAML parse error: {}", e)))?;

    if yaml_dag.tasks.is_empty() {
        return Err(ErgataiError::InvalidArgument(
            "No tasks found in YAML definition".to_string(),
        ));
    }

    // 验证和合并参数
    let resolved_params = resolve_parameters(&yaml_dag.parameters, params)?;

    // 检查重复任务名
    let mut seen_names = std::collections::HashSet::new();
    for task in &yaml_dag.tasks {
        if !seen_names.insert(&task.name) {
            return Err(ErgataiError::InvalidArgument(format!(
                "Duplicate task name: '{}'",
                task.name
            )));
        }
    }

    // 验证 depends_on 引用的任务名存在
    let task_names: std::collections::HashSet<&str> =
        yaml_dag.tasks.iter().map(|t| t.name.as_str()).collect();
    for task in &yaml_dag.tasks {
        for dep in &task.depends_on {
            if !task_names.contains(dep.as_str()) {
                return Err(ErgataiError::InvalidArgument(format!(
                    "Task '{}' depends_on unknown task '{}'",
                    task.name, dep
                )));
            }
        }
        if let Some(ref parent) = task.parent {
            if !task_names.contains(parent.as_str()) {
                return Err(ErgataiError::InvalidArgument(format!(
                    "Task '{}' has unknown parent '{}'",
                    task.name, parent
                )));
            }
        }
    }

    // 构建 name → UUID 映射
    let mut name_to_uuid: HashMap<String, String> = HashMap::with_capacity(yaml_dag.tasks.len());
    for task in &yaml_dag.tasks {
        name_to_uuid.insert(task.name.clone(), Uuid::new_v4().to_string());
    }

    // DAG-level priority (for propagation to nodes)
    let dag_priority = yaml_dag.priority.clone();

    // 转换为 TaskNode
    let nodes: Vec<TaskNode> = yaml_dag
        .tasks
        .into_iter()
        .map(|task| {
            let uuid = name_to_uuid
                .get(&task.name)
                .cloned()
                .expect("name_to_uuid populated above");

            // 合并 depends_on + parent
            let mut depends_on = task.depends_on;
            if let Some(ref parent) = task.parent {
                if !depends_on.contains(parent) {
                    depends_on.push(parent.clone());
                }
            }

            // depends_on 名称 → UUID
            let depends_on: Vec<String> = depends_on
                .iter()
                .map(|name| {
                    name_to_uuid
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| name.clone())
                })
                .collect();

            // 构建 metadata
            let mut metadata = HashMap::new();
            if let Some(ref task_path) = task.task {
                metadata.insert("task_path".to_string(), task_path.clone());
            }
            if let Some(ref parent) = task.parent {
                metadata.insert("parent".to_string(), parent.clone());
            }
            // 额外字段存入 metadata（转为字符串）
            for (key, value) in &task.metadata {
                let str_value = match value {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Number(n) => n.to_string(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    other => format!("{:?}", other),
                };
                metadata.insert(key.clone(), str_value);
            }

            // 验证 scope
            let scope = task.scope.and_then(|s| {
                match validate_scope_pattern(&s) {
                    Ok(_) => Some(s),
                    Err(e) => {
                        tracing::warn!(scope = %s, error = %e, "Invalid scope pattern in YAML DAG, ignoring");
                        None
                    }
                }
            });

            // Priority propagation: node's own priority takes precedence over DAG-level
            let priority = task.priority.or_else(|| dag_priority.clone());

            TaskNode {
                id: uuid,
                agent: task.agent,
                task: task.name,
                status: TaskStatus::Pending,
                depends_on,
                input: task.input,
                output: task.output,
                result_path: None,
                max_retries: task.retry.unwrap_or(0),
                retry_count: 0,
                priority,
                timeout: task.timeout,
                scope,
                metadata,
                condition: task.condition,
            }
        })
        .collect();

    let mut graph = TaskGraph::new(nodes);
    graph.timeout = yaml_dag.timeout;
    graph.max_agent_calls = yaml_dag.max_agent_calls;
    graph.stall_timeout_secs = yaml_dag.stall_timeout_secs;
    graph.priority = dag_priority;
    graph.parameters = resolved_params;
    graph.communication = yaml_dag.communication;
    graph.validate()?;

    Ok(graph)
}

/// 检测内容是否为 YAML 格式（而非 Markdown）
///
/// 判断规则：
/// 1. 以 `---` 开头（YAML document marker）
/// 2. 包含 `tasks:` 键（YAML DAG 的标志性字段）
/// 3. 不包含 `## ` 或 `### ` 开头的行（Markdown 任务标题）
pub fn is_yaml_format(content: &str) -> bool {
    let trimmed = content.trim_start();

    // YAML document marker
    if trimmed.starts_with("---") {
        return true;
    }

    // 包含 tasks: 键
    if content.lines().any(|line| {
        let t = line.trim();
        t.starts_with("tasks:") || t.starts_with("tasks :")
    }) {
        return true;
    }

    false
}

/// 自动检测格式并解析 DAG 定义
///
/// 优先尝试 YAML 解析，如果不符合 YAML 格式则回退到 Markdown 解析。
pub fn parse_dag_auto(
    content: &str,
    params: Option<HashMap<String, serde_json::Value>>,
) -> ErgataiResult<TaskGraph> {
    if is_yaml_format(content) {
        parse_dag_yaml(content, params)
    } else {
        crate::dag_parser::parse_dag_markdown(content)
    }
}

/// 解析和验证参数
///
/// 根据参数定义 schema 和用户提供的参数值，进行验证和默认值填充。
fn resolve_parameters(
    schema: &[YamlParameter],
    provided: Option<HashMap<String, serde_json::Value>>,
) -> ErgataiResult<HashMap<String, serde_json::Value>> {
    let provided = provided.unwrap_or_default();
    let mut resolved = HashMap::new();
    let mut valid_names = std::collections::HashSet::with_capacity(schema.len());

    // Single pass: validate, apply defaults, and collect valid names
    for param in schema {
        valid_names.insert(param.name.clone());

        if let Some(value) = provided.get(&param.name) {
            // 验证参数类型
            if let Some(ref param_type) = param.param_type {
                validate_param_type(&param.name, value, param_type)?;
            }
            resolved.insert(param.name.clone(), value.clone());
        } else if let Some(ref default) = param.default {
            // 使用默认值
            let json_value = yaml_to_json(default);
            resolved.insert(param.name.clone(), json_value);
        } else if param.required {
            // 必需参数未提供
            return Err(ErgataiError::InvalidArgument(format!(
                "Required parameter '{}' not provided",
                param.name
            )));
        }
        // 非必需且无默认值的参数可以不出现
    }

    // Check for unknown parameters using the collected set
    for key in provided.keys() {
        if !valid_names.contains(key) {
            return Err(ErgataiError::InvalidArgument(format!(
                "Unknown parameter '{}'",
                key
            )));
        }
    }

    Ok(resolved)
}

/// 验证参数类型
fn validate_param_type(
    name: &str,
    value: &serde_json::Value,
    expected_type: &str,
) -> ErgataiResult<()> {
    let is_valid = match expected_type.to_lowercase().as_str() {
        "string" => value.is_string(),
        "number" | "int" | "integer" | "float" => value.is_number(),
        "boolean" | "bool" => value.is_boolean(),
        _ => true, // 未知类型不验证
    };

    if !is_valid {
        return Err(ErgataiError::InvalidArgument(format!(
            "Parameter '{}' expected type '{}', got {:?}",
            name, expected_type, value
        )));
    }
    Ok(())
}

/// 将 YAML 值转换为 JSON 值
fn yaml_to_json(value: &serde_yaml::Value) -> serde_json::Value {
    match value {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(serde_json::Number::from(i))
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                if let serde_yaml::Value::String(key) = k {
                    obj.insert(key.clone(), yaml_to_json(v));
                }
            }
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

/// 验证 scope glob 模式（与 dag_parser.rs 中的逻辑一致）
fn validate_scope_pattern(pattern: &str) -> ErgataiResult<()> {
    if pattern.contains("..") {
        return Err(ErgataiError::InvalidPath(
            "Scope pattern cannot contain '..' (path traversal)".to_string(),
        ));
    }
    if pattern.starts_with('/') || pattern.starts_with('\\') {
        return Err(ErgataiError::InvalidPath(
            "Scope pattern must be relative, not absolute".to_string(),
        ));
    }
    match glob::Pattern::new(pattern) {
        Ok(_) => Ok(()),
        Err(e) => Err(ErgataiError::InvalidArgument(format!(
            "Invalid glob pattern '{}': {}",
            pattern, e
        ))),
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_yaml() {
        let yaml = r#"
tasks:
  - name: Task A
    agent: agent-a
    task: tasks/a.md

  - name: Task B
    agent: agent-b
    task: tasks/b.md
    depends_on: [Task A]
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        assert_eq!(graph.nodes.len(), 2);

        // 验证 UUID 格式
        for node in &graph.nodes {
            assert!(Uuid::parse_str(&node.id).is_ok());
        }

        // 验证 depends_on 引用已解析为 UUID
        let node_a = &graph.nodes[0];
        let node_b = &graph.nodes[1];
        assert_eq!(node_b.depends_on.len(), 1);
        assert_eq!(node_b.depends_on[0], node_a.id);
    }

    #[test]
    fn test_yaml_with_global_info() {
        let yaml = r#"
name: my-dag
description: A test DAG
tasks:
  - name: Task A
    agent: agent-a
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn test_yaml_minimal_task() {
        let yaml = r#"
tasks:
  - name: Minimal
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        let node = &graph.nodes[0];
        assert_eq!(node.task, "Minimal");
        assert_eq!(node.agent, "agent"); // default
        assert!(node.depends_on.is_empty());
    }

    #[test]
    fn test_yaml_all_fields() {
        let yaml = r#"
tasks:
  - name: Full Task
    agent: agent-a
    task: tasks/a.md
    depends_on: []
    input: some input
    output: some output
    priority: high
    timeout: 300
    retry: 3
    scope: "src/**/*.rs"
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        let node = &graph.nodes[0];
        assert_eq!(node.agent, "agent-a");
        assert_eq!(node.input, Some("some input".to_string()));
        assert_eq!(node.output, Some("some output".to_string()));
        assert_eq!(node.priority, Some("high".to_string()));
        assert_eq!(node.timeout, Some(300));
        assert_eq!(node.max_retries, 3);
        assert_eq!(node.scope, Some("src/**/*.rs".to_string()));
    }

    #[test]
    fn test_yaml_parent_merged_into_depends_on() {
        let yaml = r#"
tasks:
  - name: Root Task
    agent: coordinator
  - name: Child Task
    agent: worker
    parent: Root Task
    depends_on: [Root Task]
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        let child = &graph.nodes[1];
        // parent 与 depends_on 合并后只有一个唯一依赖
        assert_eq!(child.depends_on.len(), 1);
    }

    #[test]
    fn test_yaml_duplicate_task_names() {
        let yaml = r#"
tasks:
  - name: Task A
    agent: agent-a
  - name: Task A
    agent: agent-b
"#;
        let result = parse_dag_yaml(yaml, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Duplicate task name"));
    }

    #[test]
    fn test_yaml_unknown_dependency() {
        let yaml = r#"
tasks:
  - name: Task A
    depends_on: [NonExistent]
"#;
        let result = parse_dag_yaml(yaml, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown task"));
    }

    #[test]
    fn test_yaml_empty_tasks() {
        let yaml = "tasks: []\n";
        let result = parse_dag_yaml(yaml, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No tasks found"));
    }

    #[test]
    fn test_yaml_invalid_syntax() {
        let yaml = "this: is: not: valid: yaml: [";
        let result = parse_dag_yaml(yaml, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_yaml_scope_validation() {
        let yaml = r#"
tasks:
  - name: Task A
    scope: "../secret/**"
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        assert_eq!(graph.nodes[0].scope, None); // 非法 scope 被忽略
    }

    #[test]
    fn test_yaml_metadata_extra_fields() {
        let yaml = r#"
tasks:
  - name: Task A
    custom_key: custom_value
    another: 42
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        let node = &graph.nodes[0];
        assert_eq!(
            node.metadata.get("custom_key"),
            Some(&"custom_value".to_string())
        );
        assert_eq!(node.metadata.get("another"), Some(&"42".to_string()));
    }

    #[test]
    fn test_is_yaml_format() {
        assert!(is_yaml_format("---\ntasks: []"));
        assert!(is_yaml_format("tasks:\n  - name: A"));
        assert!(!is_yaml_format("## Task A\n- **agent**: a"));
        assert!(!is_yaml_format("# Title\nSome text"));
    }

    #[test]
    fn test_parse_dag_auto_yaml() {
        let yaml = r#"
tasks:
  - name: Task A
    agent: agent-a
"#;
        let graph = parse_dag_auto(yaml, None).unwrap();
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn test_parse_dag_auto_markdown() {
        let md = r#"
## Task A
- **agent**: agent-a
"#;
        let graph = parse_dag_auto(md, None).unwrap();
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn test_yaml_max_retries_alias() {
        let yaml = r#"
tasks:
  - name: Task A
    max_retries: 5
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        assert_eq!(graph.nodes[0].max_retries, 5);
    }

    #[test]
    fn test_yaml_multiple_depends() {
        let yaml = r#"
tasks:
  - name: A
  - name: B
  - name: C
  - name: D
    depends_on: [A, B, C]
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        let node_d = &graph.nodes[3];
        assert_eq!(node_d.depends_on.len(), 3);
    }

    #[test]
    fn test_yaml_special_chars_in_name() {
        let yaml = r#"
tasks:
  - name: "Task with special chars: @#$%"
    agent: agent-a
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        assert_eq!(graph.nodes[0].task, "Task with special chars: @#$%");
    }

    #[test]
    fn test_yaml_chinese_content() {
        let yaml = r#"
name: 功能实现流程
description: 多 agent 协作实现功能
tasks:
  - name: 需求分析
    agent: pm
    task: tasks/requirements.md
  - name: 架构设计
    agent: architect
    depends_on: [需求分析]
  - name: 前端开发
    agent: frontend-dev
    depends_on: [架构设计]
    scope: "src/frontend/**"
  - name: 后端开发
    agent: backend-dev
    depends_on: [架构设计]
    scope: "src/backend/**"
  - name: 集成测试
    agent: qa
    depends_on: [前端开发, 后端开发]
"#;
        let graph = parse_dag_yaml(yaml, None).unwrap();
        assert_eq!(graph.nodes.len(), 5);
        // 验证中文名称正确保留
        assert_eq!(graph.nodes[0].task, "需求分析");
        assert_eq!(graph.nodes[4].task, "集成测试");
        // 验证依赖关系
        assert_eq!(graph.nodes[4].depends_on.len(), 2);
    }

    #[test]
    fn yaml_roundtrip_preserves_budget_and_stall_fields() {
        let yaml = r#"
dag_id: test-dag
description: test
max_agent_calls: 50
stall_timeout_secs: 300
tasks:
  - name: a
    agent: alice
"#;
        let graph = parse_dag_yaml(yaml, None).expect("parse");
        assert_eq!(graph.max_agent_calls, Some(50));
        assert_eq!(graph.stall_timeout_secs, Some(300));
    }

    #[test]
    fn yaml_roundtrip_defaults_budget_fields_to_none() {
        let yaml = r#"
dag_id: test-dag
description: test
tasks:
  - name: a
    agent: alice
"#;
        let graph = parse_dag_yaml(yaml, None).expect("parse");
        assert_eq!(graph.max_agent_calls, None);
        assert_eq!(graph.stall_timeout_secs, None);
    }
}
