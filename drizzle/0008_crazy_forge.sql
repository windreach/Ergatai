CREATE TABLE `agent_messages` (
	`id` text PRIMARY KEY NOT NULL,
	`task_id` text NOT NULL,
	`from_agent` text NOT NULL,
	`to_agent` text NOT NULL,
	`content` text NOT NULL,
	`type` text DEFAULT 'request' NOT NULL,
	`intent` text,
	`expect` text,
	`constraints` text,
	`raw_data` text,
	`created_at` integer,
	FOREIGN KEY (`task_id`) REFERENCES `agent_tasks`(`id`) ON UPDATE no action ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX `agent_messages_task_idx` ON `agent_messages` (`task_id`);--> statement-breakpoint
CREATE INDEX `agent_messages_from_idx` ON `agent_messages` (`from_agent`);--> statement-breakpoint
CREATE INDEX `agent_messages_to_idx` ON `agent_messages` (`to_agent`);--> statement-breakpoint
CREATE TABLE `agent_tasks` (
	`id` text PRIMARY KEY NOT NULL,
	`from_agent` text NOT NULL,
	`to_agent` text NOT NULL,
	`intent` text,
	`content` text NOT NULL,
	`expect` text,
	`status` text DEFAULT 'pending' NOT NULL,
	`result` text,
	`chat_id` text,
	`sub_chat_id` text,
	`created_at` integer,
	`updated_at` integer,
	`completed_at` integer
);
--> statement-breakpoint
CREATE INDEX `agent_tasks_from_idx` ON `agent_tasks` (`from_agent`);--> statement-breakpoint
CREATE INDEX `agent_tasks_to_idx` ON `agent_tasks` (`to_agent`);--> statement-breakpoint
CREATE INDEX `agent_tasks_status_idx` ON `agent_tasks` (`status`);--> statement-breakpoint
CREATE INDEX `agent_tasks_chat_idx` ON `agent_tasks` (`chat_id`);