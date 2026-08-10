//! NATS integration for Ergatai
//!
//! Provides:
//! - Embedded nats-server process management
//! - async-nats client wrapper
//! - JetStream-based task queue
//! - DAG event types and event bus for event-driven communication
//! - File access control event payloads and JetStream streams

pub mod server;
pub mod connection;
pub mod task_queue;
pub mod manager;
pub mod events;
pub mod event_bus;
pub mod file_access_streams;

pub use server::NatsServer;
pub use connection::NatsConnection;
pub use task_queue::NatsTaskQueue;
pub use manager::{init_nats, get_nats_connection, is_nats_initialized, shutdown_nats, get_nats_server_port};
pub use events::{
    TaskSubmitPayload, NodeCompletePayload, NodeFailedPayload, DagCompletePayload,
    AgentMessagePayload, DagEvent,
    // File access control payloads
    FileAccessRequestPayload, FileAccessGrantPayload, FileAccessDenyPayload,
    FileAccessEscalatePayload, FileAccessApprovePayload, FileAccessRejectPayload,
    FileAccessReleasePayload, FileAccessRevokePayload, FileConflictArbitratePayload,
    FileReadyPayload, FileErrorPayload, SystemTokenPayload,
};
pub use event_bus::EventBus;
pub use file_access_streams::{
    FILE_ACCESS_REQUEST_STREAM, FILE_ACCESS_GRANT_STREAM, FILE_ACCESS_ESCALATE_STREAM,
    file_access_request_stream_config, file_access_grant_stream_config,
    file_access_escalate_stream_config, all_file_access_stream_configs,
};
