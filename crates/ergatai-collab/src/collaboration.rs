//! Collaboration Session & Mesh Policy
//!
//! Binds a DAG execution to a set of participants and a communication policy,
//! forming the glue between the orchestration layer (TaskGraph) and the
//! communication layer (agent-to-agent messaging).
//!
//! # Usage
//!
//! When a DAG is submitted via `submit_orchestration`, the `DagScheduler`
//! constructs a `CollaborationSession` from the graph's `communication` field
//! (default: `MeshPolicy::Open`). Downstream consumers (e.g., an MCP tool)
//! can query the current session to learn who is collaborating and under
//! which policy.
//!
//! ACL enforcement in `send_message` calls `DagScheduler::check_communication`,
//! which uses the pre-computed adjacency set for O(1) `Adjacent` policy checks.

use std::collections::HashSet;

use ergatai_dag::TaskGraph;
use serde::{Deserialize, Serialize};

/// Communication mode: defines how agents inside a DAG may talk to each other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MeshPolicy {
    /// Default: any participant may @mention any other participant.
    #[default]
    Open,
    /// Only agents directly connected by a DAG dependency edge may communicate.
    Adjacent,
    /// All communication must pass through the designated hub agent.
    Star {
        /// Hub agent identifier (matches `TaskNode.agent`).
        hub: String,
    },
    /// Explicit allow-list of agent pairs (bidirectional).
    Restricted {
        /// Allowed pairs. Each `(a, b)` also permits `b → a`.
        pairs: Vec<(String, String)>,
    },
}

impl MeshPolicy {
    /// Parse a policy string. Accepted forms:
    /// - `"open"`
    /// - `"adjacent"`
    /// - `"star:{hub_agent}"`
    pub fn parse(s: &str) -> Result<Self, String> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("open") {
            return Ok(MeshPolicy::Open);
        }
        if trimmed.eq_ignore_ascii_case("adjacent") {
            return Ok(MeshPolicy::Adjacent);
        }
        if let Some(hub) = trimmed.strip_prefix("star:") {
            let hub = hub.trim();
            if hub.is_empty() {
                return Err("star: policy requires a hub agent id".to_string());
            }
            return Ok(MeshPolicy::Star {
                hub: hub.to_string(),
            });
        }
        Err(format!("Unknown mesh policy: {trimmed:?}"))
    }
}

/// Result of a `CollaborationSession` / `DagScheduler` ACL check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunicationCheck {
    /// At least one endpoint is not a participant of this session; the
    /// policy was not applied. Callers should treat this as "no opinion".
    NotApplicable,
    /// Both endpoints are participants and the policy permits the pair.
    Allowed,
    /// Both endpoints are participants but the policy denies the pair.
    /// The wrapped string is a human-readable reason.
    Denied(String),
}

impl CommunicationCheck {
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            CommunicationCheck::Allowed | CommunicationCheck::NotApplicable
        )
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, CommunicationCheck::Denied(_))
    }
}

/// A collaboration session bound to a single DAG execution.
///
/// Aggregates the set of participating agents (derived from `TaskNode.agent`
/// across all nodes) and the `MeshPolicy` governing their communication.
/// Pre-computes an adjacency set for O(1) `Adjacent` policy checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationSession {
    /// The DAG this session is bound to.
    pub dag_id: String,
    /// Set of agent identifiers participating in this DAG.
    pub participants: HashSet<String>,
    /// Communication policy for this session.
    pub policy: MeshPolicy,
    /// Pre-computed bidirectional adjacent pairs as combined keys "from\0to".
    /// Populated from the graph at construction time for O(1) lookups.
    /// Using a single String key reduces allocations from 2 to 1 per lookup.
    #[serde(skip)]
    pub adjacent_pairs: HashSet<String>,
    /// Unix timestamp (seconds) when the session was created.
    pub created_at: u64,
}

impl CollaborationSession {
    /// Build a session from a TaskGraph, extracting participants from node agents
    /// and pre-computing the adjacency set for the `Adjacent` policy.
    pub fn from_graph(dag_id: &str, graph: &TaskGraph, policy: MeshPolicy) -> Self {
        let participants: HashSet<String> = graph.nodes.iter().map(|n| n.agent.clone()).collect();
        let adjacent_pairs = Self::compute_adjacency(graph);
        Self {
            dag_id: dag_id.to_string(),
            participants,
            policy,
            adjacent_pairs,
            created_at: chrono::Utc::now().timestamp() as u64,
        }
    }

    /// Pre-compute the set of bidirectional adjacent agent pairs from the graph.
    /// Uses combined keys "from\0to" to reduce allocations.
    fn compute_adjacency(graph: &TaskGraph) -> HashSet<String> {
        let mut pairs = HashSet::new();
        for node in &graph.nodes {
            for dep_id in &node.depends_on {
                if let Some(dep_node) = graph.find_node(dep_id) {
                    if node.agent != dep_node.agent {
                        // Bidirectional: insert both directions
                        pairs.insert(format!("{}\0{}", node.agent, dep_node.agent));
                        pairs.insert(format!("{}\0{}", dep_node.agent, node.agent));
                    }
                }
            }
        }
        pairs
    }

    /// Whether `from → to` is permitted: both must be participants AND the
    /// policy must allow the pair. Uses pre-computed adjacency for O(1) lookups.
    pub fn allows(&self, from: &str, to: &str) -> bool {
        if !self.participants.contains(from) || !self.participants.contains(to) {
            return false;
        }
        match &self.policy {
            MeshPolicy::Open => true,
            MeshPolicy::Star { hub } => from == hub || to == hub,
            MeshPolicy::Adjacent => {
                // Single allocation instead of two (format vs to_string + to_string)
                self.adjacent_pairs.contains(&format!("{}\0{}", from, to))
            }
            MeshPolicy::Restricted { pairs } => pairs
                .iter()
                .any(|(a, b)| (a == from && b == to) || (a == to && b == from)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ergatai_dag::TaskNode;

    fn make_node(id: &str, agent: &str) -> TaskNode {
        TaskNode::new(id, agent, "t")
    }

    fn sample_graph() -> TaskGraph {
        // A --depends_on--> B --depends_on--> C   (agents: a1, a2, a3)
        // D (agent: a4, isolated)
        let mut b = make_node("B", "a2");
        b.depends_on.push("A".to_string());
        let mut c = make_node("C", "a3");
        c.depends_on.push("B".to_string());
        let d = make_node("D", "a4");
        let a = make_node("A", "a1");
        TaskGraph::new(vec![a, b, c, d])
    }

    #[test]
    fn test_parse_open() {
        assert_eq!(MeshPolicy::parse("open").unwrap(), MeshPolicy::Open);
        assert_eq!(MeshPolicy::parse("  Open  ").unwrap(), MeshPolicy::Open);
    }

    #[test]
    fn test_parse_adjacent() {
        assert_eq!(MeshPolicy::parse("adjacent").unwrap(), MeshPolicy::Adjacent);
    }

    #[test]
    fn test_parse_star() {
        let p = MeshPolicy::parse("star:hub-agent").unwrap();
        assert_eq!(
            p,
            MeshPolicy::Star {
                hub: "hub-agent".to_string()
            }
        );
    }

    #[test]
    fn test_parse_star_empty_hub_rejected() {
        assert!(MeshPolicy::parse("star:").is_err());
    }

    #[test]
    fn test_parse_unknown_rejected() {
        assert!(MeshPolicy::parse("bogus").is_err());
    }

    #[test]
    fn test_open_allows_everything() {
        let g = sample_graph();
        let s = CollaborationSession::from_graph("dag-1", &g, MeshPolicy::Open);
        assert!(s.allows("a1", "a4"));
        assert!(s.allows("a3", "a1"));
    }

    #[test]
    fn test_star_hub_always_allowed() {
        let g = sample_graph();
        let s = CollaborationSession::from_graph(
            "dag-1",
            &g,
            MeshPolicy::Star {
                hub: "a1".to_string(),
            },
        );
        assert!(s.allows("a1", "a2")); // hub → other
        assert!(s.allows("a4", "a1")); // other → hub
        assert!(!s.allows("a2", "a3")); // peer-to-peer blocked
    }

    #[test]
    fn test_adjacent_respects_edges() {
        let g = sample_graph();
        let s = CollaborationSession::from_graph("dag-1", &g, MeshPolicy::Adjacent);
        assert!(s.allows("a1", "a2")); // A ↔ B edge
        assert!(s.allows("a2", "a3")); // B ↔ C edge
        assert!(!s.allows("a1", "a3")); // A ↔ C not directly connected
        assert!(!s.allows("a1", "a4")); // A ↔ D isolated
    }

    #[test]
    fn test_restricted_bidirectional() {
        let g = sample_graph();
        let s = CollaborationSession::from_graph(
            "dag-1",
            &g,
            MeshPolicy::Restricted {
                pairs: vec![("a1".to_string(), "a4".to_string())],
            },
        );
        assert!(s.allows("a1", "a4"));
        assert!(s.allows("a4", "a1"));
        assert!(!s.allows("a1", "a2"));
    }

    #[test]
    fn test_session_participants() {
        let g = sample_graph();
        let s = CollaborationSession::from_graph("dag-1", &g, MeshPolicy::Open);
        assert_eq!(s.participants.len(), 4);
        assert!(s.participants.contains("a1"));
        assert!(s.participants.contains("a4"));
        assert_eq!(s.dag_id, "dag-1");
    }

    #[test]
    fn test_session_adjacency_precomputed() {
        // sample_graph: A(a1) <- B(a2) <- C(a3), D(a4) isolated
        let g = sample_graph();
        let s = CollaborationSession::from_graph("dag-1", &g, MeshPolicy::Adjacent);
        // A↔B edge → a1↔a2
        assert!(s.adjacent_pairs.contains("a1\0a2"));
        assert!(s.adjacent_pairs.contains("a2\0a1"));
        // B↔C edge → a2↔a3
        assert!(s.adjacent_pairs.contains("a2\0a3"));
        assert!(s.adjacent_pairs.contains("a3\0a2"));
        // No A↔C edge
        assert!(!s.adjacent_pairs.contains("a1\0a3"));
        // No A↔D edge (D isolated)
        assert!(!s.adjacent_pairs.contains("a1\0a4"));
        // Adjacent policy uses precomputed set
        assert!(s.allows("a1", "a2"));
        assert!(s.allows("a2", "a3"));
        assert!(!s.allows("a1", "a3"));
        assert!(!s.allows("a1", "a4"));
    }

    #[test]
    fn test_session_blocks_non_participants() {
        let g = sample_graph();
        let s = CollaborationSession::from_graph(
            "dag-1",
            &g,
            MeshPolicy::Star {
                hub: "a1".to_string(),
            },
        );
        // outsider → participant rejected at session level
        assert!(!s.allows("outsider", "a2"));
        // participant → outsider rejected
        assert!(!s.allows("a1", "outsider"));
    }

    #[test]
    fn test_communication_check_not_applicable_for_outsiders() {
        // When either endpoint is not a participant, the ACL check is
        // NotApplicable (does not take a stance).
        let g = sample_graph();
        let s = CollaborationSession::from_graph(
            "dag-1",
            &g,
            MeshPolicy::Star {
                hub: "a1".to_string(),
            },
        );
        // Both participants, policy denies → session.allows() false
        assert!(!s.allows("a2", "a3"));
        // One outsider → session.allows() false (not a participant)
        assert!(!s.allows("outsider", "a1"));
    }
}
