//! Critical Path Method (CPM) for DAG optimization
//!
//! 关键路径法用于识别 DAG 中最长的执行路径，从而优化整体执行时间。
//! 关键路径上的节点决定了 DAG 的最短完成时间，应该优先执行。

use std::collections::{HashMap, VecDeque};

use crate::dag_topology::{TaskGraph, TaskNode, TaskStatus};

/// 关键路径计算结果
#[derive(Debug, Clone)]
pub struct CriticalPathResult {
    /// 关键路径上的节点 ID 列表（按执行顺序）
    pub critical_path: Vec<String>,
    /// 每个节点的浮动时间（Slack = LST - EST）
    /// 浮动时间为 0 的节点在关键路径上
    pub slack_times: HashMap<String, u64>,
    /// 每个节点的最早开始时间
    pub earliest_start: HashMap<String, u64>,
    /// 每个节点的最晚开始时间
    pub latest_start: HashMap<String, u64>,
    /// DAG 的关键路径总时长（秒）
    pub total_duration: u64,
}

/// 计算 DAG 的关键路径
///
/// 使用 Critical Path Method (CPM) 算法：
/// 1. 正向遍历计算最早开始时间（EST）
/// 2. 反向遍历计算最晚开始时间（LST）
/// 3. 识别关键路径（Slack == 0 的节点）
///
/// # Arguments
/// * `graph` - DAG 任务图
/// * `estimated_durations` - 每个节点的预估执行时间（秒）
///
/// # Returns
/// 关键路径计算结果
pub fn calculate_critical_path(
    graph: &TaskGraph,
    estimated_durations: &HashMap<String, u64>,
) -> Option<CriticalPathResult> {
    if graph.nodes.is_empty() {
        return None;
    }

    // 构建节点映射和依赖图
    let node_map: HashMap<String, &TaskNode> =
        graph.nodes.iter().map(|n| (n.id.clone(), n)).collect();

    // 构建反向依赖图（谁依赖我）
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    for node in &graph.nodes {
        dependents.entry(node.id.clone()).or_default();
        for dep_id in &node.depends_on {
            dependents
                .entry(dep_id.clone())
                .or_default()
                .push(node.id.clone());
        }
    }

    // 找到所有起始节点（没有依赖的节点）
    let start_nodes: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| n.depends_on.is_empty() && n.status != TaskStatus::Skipped)
        .map(|n| n.id.clone())
        .collect();

    if start_nodes.is_empty() {
        return None;
    }

    // Step 1: 正向遍历 - 计算最早开始时间 (EST)
    let mut earliest_start: HashMap<String, u64> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    // 起始节点的 EST = 0
    for start_id in &start_nodes {
        earliest_start.insert(start_id.clone(), 0);
        queue.push_back(start_id.clone());
    }

    // BFS 正向遍历
    while let Some(node_id) = queue.pop_front() {
        let _node = node_map.get(&node_id)?;
        let node_est = *earliest_start.get(&node_id)?;
        let duration = estimated_durations.get(&node_id).copied().unwrap_or(10); // 默认 10 秒

        // 计算最早完成时间 (EFT)
        let earliest_finish = node_est + duration;

        // 更新所有依赖此节点的节点的 EST
        if let Some(deps) = dependents.get(&node_id) {
            for dep_id in deps {
                let dep_node = node_map.get(dep_id)?;

                // 跳过已完成的节点
                if dep_node.status == TaskStatus::Skipped {
                    continue;
                }

                let dep_est = earliest_start.entry(dep_id.clone()).or_insert(0);

                // 依赖节点的 EST = max(当前 EST, 当前节点的 EFT)
                *dep_est = (*dep_est).max(earliest_finish);

                // 检查是否所有依赖都已计算
                let all_deps_calculated = dep_node
                    .depends_on
                    .iter()
                    .all(|d| earliest_start.contains_key(d));

                if all_deps_calculated {
                    queue.push_back(dep_id.clone());
                }
            }
        }
    }

    // Step 2: 计算 DAG 总时长（所有结束节点的最大 EFT）
    let end_nodes: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| dependents.get(&n.id).is_none_or(|d| d.is_empty()))
        .filter(|n| n.status != TaskStatus::Skipped)
        .map(|n| n.id.clone())
        .collect();

    let mut total_duration = 0u64;
    for end_id in &end_nodes {
        let _end_node = node_map.get(end_id)?;
        let end_est = earliest_start.get(end_id)?;
        let duration = estimated_durations.get(end_id).copied().unwrap_or(10);
        let end_eft = end_est + duration;
        total_duration = total_duration.max(end_eft);
    }

    // Step 3: 反向遍历 - 计算最晚开始时间 (LST)
    let mut latest_start: HashMap<String, u64> = HashMap::new();
    queue.clear();

    // 结束节点的 LST = total_duration - duration
    for end_id in &end_nodes {
        let _end_node = node_map.get(end_id)?;
        let duration = estimated_durations.get(end_id).copied().unwrap_or(10);
        let lst = total_duration.saturating_sub(duration);
        latest_start.insert(end_id.clone(), lst);
        queue.push_back(end_id.clone());
    }

    // BFS 反向遍历
    while let Some(node_id) = queue.pop_front() {
        let node = node_map.get(&node_id)?;
        let node_lst = *latest_start.get(&node_id)?;

        // 更新所有此节点依赖的节点的 LST
        for dep_id in &node.depends_on {
            let dep_node = node_map.get(dep_id)?;

            if dep_node.status == TaskStatus::Skipped {
                continue;
            }

            let dep_duration = estimated_durations.get(dep_id).copied().unwrap_or(10);
            let dep_lst = latest_start.entry(dep_id.clone()).or_insert(total_duration);

            // 依赖节点的 LST = min(当前 LST, 当前节点的 LST - 依赖节点的 duration)
            *dep_lst = (*dep_lst).min(node_lst.saturating_sub(dep_duration));

            // 检查是否所有依赖此节点的节点都已计算
            let all_dependents_calculated = dependents
                .get(dep_id)
                .is_none_or(|deps| deps.iter().all(|d| latest_start.contains_key(d)));

            if all_dependents_calculated {
                queue.push_back(dep_id.clone());
            }
        }
    }

    // Step 4: 计算浮动时间 (Slack = LST - EST)
    let mut slack_times: HashMap<String, u64> = HashMap::new();
    for node_id in earliest_start.keys() {
        let est = earliest_start.get(node_id).copied().unwrap_or(0);
        let lst = latest_start.get(node_id).copied().unwrap_or(0);
        let slack = lst.saturating_sub(est);
        slack_times.insert(node_id.clone(), slack);
    }

    // Step 5: 识别关键路径（Slack == 0 的节点）
    let critical_path: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| slack_times.get(&n.id).copied().unwrap_or(0) == 0)
        .filter(|n| n.status != TaskStatus::Skipped)
        .map(|n| n.id.clone())
        .collect();

    Some(CriticalPathResult {
        critical_path,
        slack_times,
        earliest_start,
        latest_start,
        total_duration,
    })
}

/// 根据关键路径调整节点优先级
///
/// 关键路径上的节点优先级提升，以加速整体 DAG 完成
pub fn adjust_priority_with_critical_path(
    node: &TaskNode,
    critical_path_result: &CriticalPathResult,
    base_priority: u32,
) -> u32 {
    // 关键路径上的节点优先级 +10
    if critical_path_result.critical_path.contains(&node.id) {
        base_priority.saturating_add(10)
    } else {
        // 浮动时间越小的节点，优先级越高
        let slack = critical_path_result
            .slack_times
            .get(&node.id)
            .copied()
            .unwrap_or(u64::MAX);

        // 浮动时间转换为优先级调整（浮动时间越小，调整越大）
        // 例如：slack=0 → +10, slack=5 → +5, slack>=10 → +0
        let adjustment = if slack == 0 {
            10
        } else if slack < 10 {
            5
        } else {
            0
        };

        base_priority.saturating_add(adjustment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag_topology::{TaskComplexity, TaskGraph, TaskNode, TaskStatus};

    fn create_test_graph() -> TaskGraph {
        // 创建测试图：
        // A(5s) → B(10s) → D(3s)
        //  ↓                ↑
        // C(2s) ─────────────
        //
        // 关键路径：A → B → D (18s)
        // C 的浮动时间：18 - 2 = 16s

        let nodes = vec![
            TaskNode {
                id: "A".to_string(),
                task: "Task A".to_string(),
                agent: "agent".to_string(),
                depends_on: vec![],
                status: TaskStatus::Pending,
                input: None,
                output: None,
                result_path: None,
                max_retries: 0,
                retry_count: 0,
                priority: None,
                timeout: None,
                scope: None,
                metadata: std::collections::HashMap::new(),
                condition: None,
                complexity: TaskComplexity::Medium,
            },
            TaskNode {
                id: "B".to_string(),
                task: "Task B".to_string(),
                agent: "agent".to_string(),
                depends_on: vec!["A".to_string()],
                status: TaskStatus::Pending,
                input: None,
                output: None,
                result_path: None,
                max_retries: 0,
                retry_count: 0,
                priority: None,
                timeout: None,
                scope: None,
                metadata: std::collections::HashMap::new(),
                condition: None,
                complexity: TaskComplexity::Medium,
            },
            TaskNode {
                id: "C".to_string(),
                task: "Task C".to_string(),
                agent: "agent".to_string(),
                depends_on: vec!["A".to_string()],
                status: TaskStatus::Pending,
                input: None,
                output: None,
                result_path: None,
                max_retries: 0,
                retry_count: 0,
                priority: None,
                timeout: None,
                scope: None,
                metadata: std::collections::HashMap::new(),
                condition: None,
                complexity: TaskComplexity::Medium,
            },
            TaskNode {
                id: "D".to_string(),
                task: "Task D".to_string(),
                agent: "agent".to_string(),
                depends_on: vec!["B".to_string(), "C".to_string()],
                status: TaskStatus::Pending,
                input: None,
                output: None,
                result_path: None,
                max_retries: 0,
                retry_count: 0,
                priority: None,
                timeout: None,
                scope: None,
                metadata: std::collections::HashMap::new(),
                condition: None,
                complexity: TaskComplexity::Medium,
            },
        ];

        TaskGraph::new(nodes)
    }

    #[test]
    fn test_calculate_critical_path() {
        let graph = create_test_graph();
        let mut durations = HashMap::new();
        durations.insert("A".to_string(), 5);
        durations.insert("B".to_string(), 10);
        durations.insert("C".to_string(), 2);
        durations.insert("D".to_string(), 3);

        let result = calculate_critical_path(&graph, &durations).unwrap();

        // 关键路径应该是 A → B → D
        assert_eq!(result.critical_path.len(), 3);
        assert!(result.critical_path.contains(&"A".to_string()));
        assert!(result.critical_path.contains(&"B".to_string()));
        assert!(result.critical_path.contains(&"D".to_string()));

        // 总时长应该是 18s (A:5 + B:10 + D:3)
        assert_eq!(result.total_duration, 18);

        // C 的浮动时间应该是 8s
        // C 的 EST = 5 (在 A 之后)
        // C 的 LST = 13 (D 需要在 15 开始，C 需要 2s，所以 C 最晚 13 开始)
        // Slack = LST - EST = 13 - 5 = 8
        assert_eq!(result.slack_times.get("C"), Some(&8));

        // 关键路径节点的浮动时间应该是 0
        assert_eq!(result.slack_times.get("A"), Some(&0));
        assert_eq!(result.slack_times.get("B"), Some(&0));
        assert_eq!(result.slack_times.get("D"), Some(&0));
    }

    #[test]
    fn test_adjust_priority_with_critical_path() {
        let graph = create_test_graph();
        let mut durations = HashMap::new();
        durations.insert("A".to_string(), 5);
        durations.insert("B".to_string(), 10);
        durations.insert("C".to_string(), 2);
        durations.insert("D".to_string(), 3);

        let result = calculate_critical_path(&graph, &durations).unwrap();

        // 关键路径节点的优先级应该提升
        let node_a = graph.nodes.iter().find(|n| n.id == "A").unwrap();
        let adjusted_priority = adjust_priority_with_critical_path(node_a, &result, 5);
        assert_eq!(adjusted_priority, 15); // 5 + 10 (关键路径)

        // 非关键路径节点但浮动时间小的节点也有优先级提升
        let node_c = graph.nodes.iter().find(|n| n.id == "C").unwrap();
        let adjusted_priority = adjust_priority_with_critical_path(node_c, &result, 5);
        assert_eq!(adjusted_priority, 10); // 5 + 5 (slack=8 < 10)
    }
}
