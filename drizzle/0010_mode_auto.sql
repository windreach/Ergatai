-- Migrate mode values: "agent" → "auto"
-- This aligns DB with the new frontend AgentMode type: "auto" | "plan" | "team"
UPDATE `sub_chats` SET `mode` = 'auto' WHERE `mode` = 'agent';
