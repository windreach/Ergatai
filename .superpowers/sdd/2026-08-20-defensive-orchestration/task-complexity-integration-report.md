# Task Complexity Integration Report

**Date**: 2026-08-20
**Status**: [pending test results]
**Component**: `ergatai-collab::dag_scheduler` + `ergatai-dag::dag_topology` + `ergatai-dag::yaml_parser`

## Summary

Integrated `TaskComplexity` (Low/Medium/High) into the DAG scheduler so per-node timeouts
are automatically scaled by complexity before being handed to the three-stage timeout
watchdog and the NATS `TaskSubmitPayload`.

## Multipliers

| Complexity | Multiplier | Example (base = 60s) |
|------------|------------|----------------------|
| Low        | × 0.5      | 30s                  |
| Medium     | × 1.0      | 60s                  |
| High       | × 2.0      | 120s                 |

Default complexity is `Medium` (preserves existing behavior when the field is absent).

## Changes

### `crates/ergatai-dag/src/dag_topology.rs`
- Added `TaskGraph.node_timeout_secs: Option<u64>` — DAG-level default per-node timeout.
- Initialized to `None` in `TaskGraph::new`.

### `crates/ergatai-dag/src/yaml_parser.rs`
- Added `YamlDag.node_timeout_secs: Option<u64>`.
- Propagated to `graph.node_timeout_secs` during parsing.

### `crates/ergatai-collab/src/dag_scheduler.rs`
- Imported `TaskComplexity` from `ergatai_dag`.
- Added public helper `adjust_timeout_by_complexity(base, complexity) -> u64`.
- `generate_and_submit`:
  - Resolves effective base timeout as `node.timeout.or(graph.node_timeout_secs)`.
  - Applies `adjust_timeout_by_complexity` → `adjusted_timeout`.
  - Uses `adjusted_timeout` for both the NATS `TaskSubmitPayload.timeout_secs` and
    the three-stage `spawn_timeout_watcher` call.
  - Emits `tracing::info!` log with `complexity`, `complexity_score`,
    `base_timeout_secs`, and `adjusted_timeout_secs`.
- `calculate_critical_path`: duration estimates now also use complexity-adjusted
  timeouts so CPM priority boosting reflects actual expected work.

## Tests added

1. `test_complexity_timeout_adjustment` — unit tests for the pure scaling function,
   including odd bases, zero base, and the default complexity.
2. `test_scheduler_uses_complexity_for_timeout` — parses a YAML DAG with
   `node_timeout_secs: 60` and verifies the derived adjusted map is
   `{low_task: 30, high_task: 120}`.
3. `test_per_node_timeout_overrides_dag_default_with_complexity` — verifies that a
   per-node `timeout` overrides the DAG default and complexity adjustment still applies.

## Backward Compatibility

- Nodes without `complexity` default to `Medium` (multiplier 1.0) → no behavior change.
- DAGs without `node_timeout_secs` and nodes without `timeout` still skip the watchdog
  entirely (both sources are `None` → `adjusted_timeout` is `None`).
- `TaskComplexity` enum was already re-exported from `ergatai_dag`.

## Execution results

Tests run:
```
ERGATAI_SKIP_RMUX_DOWNLOAD=1 cargo test -p ergatai-collab complexity
ERGATAI_SKIP_RMUX_DOWNLOAD=1 cargo test -p ergatai-collab timeout
ERGATAI_SKIP_RMUX_DOWNLOAD=1 cargo clippy -p ergatai-collab -- -D warnings
```

[Results pending]
