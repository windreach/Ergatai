//! NATS integration for Ergatai
//!
//! Provides:
//! - Embedded nats-server process management
//! - async-nats client wrapper
//! - JetStream-based task queue
//! - DAG event types and event bus for event-driven communication

pub mod server;
pub mod connection;
pub mod task_queue;
pub mod manager;
pub mod events;
pub mod event_bus;

pub use server::NatsServer;
pub use connection::NatsConnection;
pub use task_queue::NatsTaskQueue;
pub use manager::{init_nats, get_nats_connection, is_nats_initialized, shutdown_nats};
pub use events::{TaskSubmitPayload, NodeCompletePayload, NodeFailedPayload, DagCompletePayload, DagEvent};
pub use event_bus::EventBus;
