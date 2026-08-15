//! NATS integration for Ergatai
//!
//! Provides:
//! - Embedded nats-server process management
//! - async-nats client wrapper
//! - JetStream-based task queue
//! - DAG event types and event bus for event-driven communication
//! - File access control event payloads and JetStream streams

pub mod connection;
pub mod event_bus;
pub mod events;
pub mod file_access_streams;
pub mod manager;
pub mod server;
pub mod task_queue;

pub use connection::NatsConnection;
pub use event_bus::EventBus;
pub use events::{
    AgentMessagePayload,
    DagCompletePayload,
    DagEvent,
    FileAccessApprovePayload,
    FileAccessDenyPayload,
    FileAccessEscalatePayload,
    FileAccessGrantPayload,
    FileAccessRejectPayload,
    FileAccessReleasePayload,
    // File access control payloads
    FileAccessRequestPayload,
    FileAccessRevokePayload,
    FileConflictArbitratePayload,
    FileErrorPayload,
    FileReadyPayload,
    NodeCompletePayload,
    NodeFailedPayload,
    SystemTokenPayload,
    TaskSubmitPayload,
};
pub use file_access_streams::{
    all_file_access_stream_configs, file_access_escalate_stream_config,
    file_access_grant_stream_config, file_access_request_stream_config,
    FILE_ACCESS_ESCALATE_STREAM, FILE_ACCESS_GRANT_STREAM, FILE_ACCESS_REQUEST_STREAM,
};
pub use manager::{
    get_nats_connection, get_nats_server_port, init_nats, is_nats_initialized, shutdown_nats,
};
pub use server::{shared_test_server, NatsServer};
pub use task_queue::NatsTaskQueue;
