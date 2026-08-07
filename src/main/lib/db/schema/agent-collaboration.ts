import { index, sqliteTable, text, integer } from "drizzle-orm/sqlite-core"
import { relations } from "drizzle-orm"
import { createId } from "../utils"

// ============ AGENT TASKS ============
// Tasks created by agents for other agents (collaboration)
export const agentTasks = sqliteTable("agent_tasks", {
  id: text("id")
    .primaryKey()
    .$defaultFn(() => createId()),
  // Task creator (agent ID)
  fromAgent: text("from_agent").notNull(),
  // Task assignee (agent ID)
  toAgent: text("to_agent").notNull(),
  // Task intent (parsed from AI-friendly message)
  intent: text("intent"), // e.g., "implement_feature", "review_code", "fix_bug"
  // Original request content
  content: text("content").notNull(),
  // Expected output/deliverable
  expect: text("expect"),
  // Task status: "pending" | "in_progress" | "completed" | "failed" | "cancelled"
  status: text("status").notNull().default("pending"),
  // Task result (JSON string)
  result: text("result"), // JSON: { output, artifacts, errors }
  // Related chat/subchat context
  chatId: text("chat_id"),
  subChatId: text("sub_chat_id"),
  // Timestamps
  createdAt: integer("created_at", { mode: "timestamp" }).$defaultFn(
    () => new Date(),
  ),
  updatedAt: integer("updated_at", { mode: "timestamp" }).$defaultFn(
    () => new Date(),
  ),
  completedAt: integer("completed_at", { mode: "timestamp" }),
}, (table) => [
  index("agent_tasks_from_idx").on(table.fromAgent),
  index("agent_tasks_to_idx").on(table.toAgent),
  index("agent_tasks_status_idx").on(table.status),
  index("agent_tasks_chat_idx").on(table.chatId),
])

export const agentTasksRelations = relations(agentTasks, ({ many }) => ({
  messages: many(agentMessages),
}))

// ============ AGENT MESSAGES ============
// Messages exchanged between agents during collaboration
export const agentMessages = sqliteTable("agent_messages", {
  id: text("id")
    .primaryKey()
    .$defaultFn(() => createId()),
  // Related task
  taskId: text("task_id")
    .notNull()
    .references(() => agentTasks.id, { onDelete: "cascade" }),
  // Message sender (agent ID)
  fromAgent: text("from_agent").notNull(),
  // Message recipient (agent ID, or "broadcast" for channel messages)
  toAgent: text("to_agent").notNull(),
  // Message content
  content: text("content").notNull(),
  // Message type: "request" | "response" | "update" | "error" | "broadcast"
  type: text("type").notNull().default("request"),
  // AI-friendly message metadata
  intent: text("intent"), // e.g., "task_request", "status_update", "result"
  expect: text("expect"), // What sender expects from recipient
  constraints: text("constraints"), // JSON array of constraints
  // Raw message data (JSON string, for full context)
  rawData: text("raw_data"),
  // Timestamp
  createdAt: integer("created_at", { mode: "timestamp" }).$defaultFn(
    () => new Date(),
  ),
}, (table) => [
  index("agent_messages_task_idx").on(table.taskId),
  index("agent_messages_from_idx").on(table.fromAgent),
  index("agent_messages_to_idx").on(table.toAgent),
])

export const agentMessagesRelations = relations(agentMessages, ({ one }) => ({
  task: one(agentTasks, {
    fields: [agentMessages.taskId],
    references: [agentTasks.id],
  }),
}))

// ============ TYPE EXPORTS ============
export type AgentTask = typeof agentTasks.$inferSelect
export type NewAgentTask = typeof agentTasks.$inferInsert
export type AgentMessage = typeof agentMessages.$inferSelect
export type NewAgentMessage = typeof agentMessages.$inferInsert
