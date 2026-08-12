# Frontend Agent Architecture - Visual Summary

## Data Model Relationships

```
┌─────────────────────────────────────────────────────────────┐
│                        PROJECTS                              │
│  id, name, path, gitRemoteUrl, gitProvider, iconPath        │
└────────────────────┬────────────────────────────────────────┘
                     │ 1:N
                     ▼
┌─────────────────────────────────────────────────────────────┐
│                          CHATS                               │
│  id, name, projectId, worktreePath, branch, baseBranch      │
│  prUrl, prNumber, archivedAt                                │
└────────────────────┬────────────────────────────────────────┘
                     │ 1:N
                     ▼
┌─────────────────────────────────────────────────────────────┐
│                        SUB_CHATS                             │
│  id, chatId, name, sessionId, streamId                      │
│  mode: "auto" | "plan"  ◄── Execution mode                  │
│  runtimeId: "claude" | "codex" | "goose"  ◄── ACP binding   │
│  messages: JSON array (AI SDK format)                       │
└─────────────────────────────────────────────────────────────┘
                     │
                     │ (Future: agent_tasks links here)
                     ▼
┌─────────────────────────────────────────────────────────────┐
│                     AGENT_TASKS                              │
│  id, fromAgent, toAgent, intent, content, expect            │
│  status: "pending" | "in_progress" | "completed"            │
│  chatId, subChatId (context references)                     │
└────────────────────┬────────────────────────────────────────┘
                     │ 1:N
                     ▼
┌─────────────────────────────────────────────────────────────┐
│                    AGENT_MESSAGES                            │
│  id, taskId, fromAgent, toAgent, content                    │
│  type: "request" | "response" | "update" | "broadcast"      │
│  intent, expect, constraints, rawData                       │
└─────────────────────────────────────────────────────────────┘
```

## State Management Layers

```
┌──────────────────────────────────────────────────────────────┐
│                    USER INTERFACE                            │
│  AgentSelector, SubChatSelector, ChatInputArea, etc.        │
└────────────────────────────┬─────────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────────┐
│                  JOTAI ATOMS (UI State)                      │
│  • selectedAgentChatIdAtom                                    │
│  • loadingSubChatsAtom                                        │
│  • subChatRuntimeIdAtomFamily (per sub-chat)                 │
│  • subChatModeAtomFamily (per sub-chat)                      │
│  • subChatModelIdAtomFamily (per sub-chat)                   │
└────────────────────────────┬─────────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────────┐
│                ZUSTAND STORES (Complex Logic)                │
│  • useAgentSubChatStore (tabs, pins, split views)           │
│  • useMessageQueueStore (message queuing)                    │
│  • useStreamingStatusStore (streaming status)                │
└────────────────────────────┬─────────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────────┐
│              MODULE STORES (Outside React)                   │
│  • agentChatStore (Chat objects, stream IDs, cleanup)       │
└────────────────────────────┬─────────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────────┐
│                    TRPC ROUTERS                              │
│  agents.ts: Runtime discovery & config                       │
│  chats.ts: Chat/sub-chat lifecycle                           │
│  acp.ts: ACP protocol bridge                                 │
└────────────────────────────┬─────────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────────┐
│                  RUST BACKEND (NAPI)                         │
│  • ACP SDK integration                                       │
│  • Session management                                        │
│  • Event polling (100ms)                                     │
│  • Pool management (multi-agent)                             │
└──────────────────────────────────────────────────────────────┘
```

## ACP Runtime Flow

```
┌──────────────────────────────────────────────────────────────┐
│ 1. RUNTIME DISCOVERY                                         │
│    agents.listRuntimes() → AcpRuntime[]                      │
│    Returns: id, label, avatar_url, availability, command     │
└──────────────────────────┬───────────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────────┐
│ 2. RUNTIME SELECTION                                         │
│    AgentSelector component                                   │
│    • Shows available runtimes (availability === "available") │
│    • Binds to sub-chat via subChatRuntimeIdAtomFamily        │
│    • 1:1 mapping (cannot switch mid-session)                 │
└──────────────────────────┬───────────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────────┐
│ 3. SUB-CHAT CREATION                                         │
│    chats.createSubChat({ chatId, name, mode, runtimeId })    │
│    • Creates sub_chats record with runtimeId                 │
│    • Frontend stores binding in subChatRuntimeIdAtomFamily   │
└──────────────────────────┬───────────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────────┐
│ 4. ACP SESSION START                                         │
│    acp.acpCreateSession() → sessionId                        │
│    • Maps subChatId → sessionId in sessionMap                │
│    • Starts event polling (100ms interval)                   │
└──────────────────────────┬───────────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────────┐
│ 5. EVENT TRANSLATION                                         │
│    ACP Events → UI Message Chunks                            │
│    • agent_message_chunk → text-delta                        │
│    • tool_call → tool-input-start/available                  │
│    • permission_request → ask-user-question                  │
│    • closed → finish                                         │
└──────────────────────────────────────────────────────────────┘
```

## Component Hierarchy

```
App
└─ AgentsLayout
   ├─ AgentsSidebar (sidebar/agents-sidebar.tsx)
   │  └─ Project list, chat list, settings
   │
   └─ AgentsContent (ui/agents-content.tsx)
      ├─ AgentsSubChatsSidebar (sidebar/agents-subchats-sidebar.tsx)
      │  ├─ Sub-chat tabs (open, pinned)
      │  ├─ Search combobox
      │  └─ Context menus
      │
      └─ ActiveChat (main/active-chat.tsx)
         ├─ ChatHeader
         │  ├─ AgentSelector (components/agent-selector.tsx)
         │  ├─ SubChatSelector (ui/sub-chat-selector.tsx)
         │  └─ Mode indicator (auto/plan)
         │
         ├─ MessagesList (main/messages-list.tsx)
         │  ├─ AssistantMessageItem
         │  ├─ AgentUserMessageBubble
         │  └─ AgentToolCall, AgentThinkingTool, etc.
         │
         └─ ChatInputArea (main/chat-input-area.tsx)
            ├─ AgentMentionsEditor
            └─ AgentSendButton
```

## Key Concepts Mapping

| Frontend Concept | Backend ACP Concept | Notes |
|------------------|---------------------|-------|
| **Sub-Chat** | ACP Session | 1:1 mapping via runtimeId |
| **Runtime** | Agent Config | command, args, env, model |
| **Mode** | Session Mode | "auto" \| "plan" |
| **Agent Tasks** | Task | Work items for agents |
| **Agent Messages** | Message | Agent-to-agent communication |
| **Chat** | Workspace | Git isolation via worktree |
| **Project** | Repository | Local git repo |

## Migration Strategy

```
CURRENT STATE                    TARGET STATE
─────────────                    ────────────
sub_chats.runtimeId       →      agent_config.id
sub_chats.mode            →      session.mode
sub_chats.sessionId       →      acp_session.id
sub_chats.messages        →      session.history
agent_tasks (DB only)     →      task (UI + DB)
agent_messages (DB only)  →      message (UI + DB)
```

### Phase 1: Align Terminology
- Rename "runtime" → "agent" in UI
- Update atom names (subChatRuntimeIdAtomFamily → subChatAgentIdAtomFamily)
- Keep runtimeId field in DB for backward compatibility

### Phase 2: Expose Multi-Agent Features
- Create AgentTaskList component
- Add task creation UI
- Show agent-to-agent messages
- Implement pool management UI

### Phase 3: Deep Integration
- Map Chat objects to ACP sessions
- Use ACP session management instead of Claude SDK
- Implement session resume via ACP
- Add session pooling for performance

### Phase 4: Advanced Features
- DAG-based task orchestration
- Cross-agent communication UI
- Real-time collaboration features
- Agent performance analytics
