# Frontend Agent/Sub-Chat Architecture Analysis

**Date**: 2026-08-11  
**Purpose**: Understanding current frontend concepts for migration to backend ACP agent model

---

## 1. Data Model (Drizzle Schema)

### Core Tables

#### `chats` (Parent Chat/Workspace)
- **id**: Primary key
- **name**: Display name
- **projectId**: Reference to project
- **worktreePath**: Git worktree path for isolation
- **branch**: Git branch name
- **baseBranch**: Base branch for worktree
- **prUrl**: Pull request URL
- **prNumber**: PR number
- **archivedAt**: Archive timestamp
- **createdAt/updatedAt**: Timestamps

#### `sub_chats` (Agent Conversations)
- **id**: Primary key
- **chatId**: Parent chat reference
- **name**: Sub-chat display name
- **sessionId**: Claude SDK session ID for resume
- **streamId**: Track in-progress streams
- **mode**: `"auto"` | `"plan"` (execution mode)
- **runtimeId**: ACP runtime binding (e.g., "claude", "goose", "codex") - **KEY FIELD FOR ACP**
- **messages**: JSON array of messages (AI SDK format)
- **createdAt/updatedAt**: Timestamps

#### `agent_tasks` (Agent Collaboration)
- **id**: Primary key
- **fromAgent**: Creator agent ID
- **toAgent**: Assignee agent ID
- **intent**: Task type (e.g., "implement_feature", "review_code")
- **content**: Task description
- **expect**: Expected output
- **status**: "pending" | "in_progress" | "completed" | "failed" | "cancelled"
- **result**: JSON result data
- **chatId/subChatId**: Context references
- **Timestamps**: createdAt, updatedAt, completedAt

#### `agent_messages` (Agent-to-Agent Communication)
- **id**: Primary key
- **taskId**: Reference to agent_tasks
- **fromAgent/toAgent**: Sender/recipient
- **content**: Message content
- **type**: "request" | "response" | "update" | "error" | "broadcast"
- **intent/expect/constraints**: Metadata
- **rawData**: Full context JSON
- **createdAt**: Timestamp

---

## 2. State Management

### Jotai Atoms (src/renderer/features/agents/atoms/)

#### Core State (index.ts)
- **selectedAgentChatIdAtom**: Currently selected chat ID
- **selectedChatIsRemoteAtom**: Whether selected chat is remote/sandbox
- **previousAgentChatIdAtom**: Navigation history
- **showNewChatFormAtom**: Toggle new chat form visibility
- **loadingSubChatsAtom**: Map<subChatId, parentChatId> for loading indicators

#### Per-SubChat State (atomFamily)
- **subChatModelIdAtomFamily**: Claude model selection per sub-chat
- **subChatCodexModelIdAtomFamily**: Codex model selection per sub-chat
- **subChatModeAtomFamily**: Execution mode per sub-chat
- **subChatRuntimeIdAtomFamily**: ACP runtime binding per sub-chat (runtime.ts)
- **previewPathAtomFamily**: Preview path per chat
- **viewportModeAtomFamily**: Desktop/mobile viewport mode
- **previewScaleAtomFamily**: Preview scale
- **mobileDeviceAtomFamily**: Mobile device settings

#### Runtime Atoms (runtime.ts)
- **availableRuntimesAtom**: List of available ACP runtimes
- **normalizedRuntimeIdAtom**: User's last selected runtime (with legacy migration)
- **subChatRuntimeIdAtomFamily**: 1:1 runtime binding per sub-chat
- **subChatRuntimeAtomFamily**: Derived atom returning full runtime object

### Zustand Stores (src/renderer/features/agents/stores/)

#### useAgentSubChatStore
Manages sub-chat tabs and organization:
- **State**:
  - `chatId`: Current parent chat context
  - `activeSubChatId`: Currently selected tab
  - `openSubChatIds`: Open tabs (preserves order)
  - `pinnedSubChatIds`: Pinned sub-chats
  - `allSubChats`: All sub-chats for history
  - `splitPaneIds`: Split view panes (max 4)
  - `splitRatios`: Pane width ratios
- **Actions**:
  - `setActiveSubChat`, `setOpenSubChats`, `addToOpenSubChats`
  - `removeFromOpenSubChats` (with cleanup of queue, streaming, Chat instance)
  - `togglePinSubChat`, `updateSubChatName/Mode/Timestamp`
  - `addToSplit`, `removeFromSplit`, `closeSplit`, `setSplitRatios`
- **Persistence**: localStorage (window-scoped keys)

#### agentChatStore
Module-level Chat object storage (outside React lifecycle):
- Maps: `chats`, `streamIds`, `parentChatIds`, `manuallyAborted`
- Methods: `get`, `set`, `has`, `delete`, `getStreamId`, `setStreamId`
- Cleanup: Calls `transport.cleanup()` on deletion

#### useMessageQueueStore
Manages message queuing for sub-chats (not shown but referenced)

#### useStreamingStatusStore
Tracks streaming status per sub-chat (not shown but referenced)

---

## 3. tRPC Routers

### agents.ts (ACP Agent Management)
Focuses on **runtime discovery and configuration**:

- **listRuntimes**: List all ACP runtimes (builtin + custom)
  - Returns: id, label, avatar_url, availability, command, binary_path, install_hint, auth_status, source
  - Normalizes NAPI camelCase → snake_case
  
- **getGlobalConfig**: Get global agent configuration
  - Returns: env_vars, provider, model, preferred_runtime
  
- **setGlobalConfig**: Save global configuration
  - Input: env_vars, provider, model, preferred_runtime
  
- **listCustomHarnesses**: List custom agent harnesses
  - Filters by source === "custom"
  
- **saveCustomHarness**: Create/update custom harness
  - Input: id, label, command, args, env, install_instructions_url, install_hint
  
- **deleteCustomHarness**: Delete custom harness by id
  
- **installRuntime**: Execute runtime install command
  - Input: runtimeId
  
- **getAgentConfig**: Get agent configuration by name
  - Input: name
  
- **saveAgentConfig**: Create/update agent config
  - Input: name, command, args, env, display_name, model, provider, agent_type, base_url, api_key, proxy, persona, avatar

### chats.ts (Chat & Sub-Chat Operations)
Focuses on **chat lifecycle and message management**:

#### Chat Operations
- **list/listArchived**: Query chats (filter by project, archive status)
- **get**: Get chat with all sub-chats and project
- **create**: Create chat with optional worktree
  - Input: projectId, name, model, initialMessage, initialMessageParts, baseBranch, branchType, useWorktree, mode, runtimeId
  - Creates initial sub-chat with user message
  - Handles worktree creation in background
- **rename/archive/restore/delete**: Chat lifecycle
- **archiveBatch**: Batch archive with terminal cleanup

#### Sub-Chat Operations
- **getSubChat**: Get single sub-chat with parent chat and project
- **createSubChat**: Create new sub-chat
  - Input: chatId, name, mode, runtimeId
- **updateSubChatRuntime**: Update ACP runtime binding
  - Input: subChatId, runtimeId
- **forkSubChat**: Fork from specific message
  - Copies .jsonl session files for resume
  - Generates fork name with [N] prefix
  - Sets shouldForkResume flag on last assistant message
- **updateSubChatMessages**: Update messages JSON
- **updateSubChatSession**: Update sessionId for Claude resume
- **updateSubChatMode**: Update mode ("auto" | "plan")
- **renameSubChat**: Rename sub-chat
- **deleteSubChat**: Delete sub-chat
- **rollbackToMessage**: Rollback to specific message by sdkMessageUuid
  - Handles git state rollback first
  - Truncates messages and sets shouldResume flag
- **generateSubChatName**: AI-generated name (Ollama offline, API online)

#### Git/PR Operations
- **getDiff/getParsedDiff**: Worktree diff with caching
- **generateCommitMessage**: AI commit message generation
- **getPrContext**: Branch info, uncommitted changes
- **updatePrInfo**: Update PR URL/number
- **getPrStatus**: GitHub PR status via gh CLI
- **mergePr**: Merge PR via gh CLI (with conflict detection)

#### Analytics/Export
- **getFileStats**: File change stats from Edit/Write tool calls
- **getPendingPlanApprovals**: Sub-chats with pending plan approvals
- **getWorktreeStatus**: Worktree existence and uncommitted changes
- **exportChat**: Export to JSON/Markdown/Text
- **getChatStats**: Message count, tool usage, token usage

### acp.ts (ACP Protocol Interface)
**Bridges frontend to Rust ACP SDK** via native binding:

#### Session Management
- **acpCreateSession**: Create new ACP session
- **acpSendPrompt**: Send prompt to agent
- **acpCloseSession**: Close session
- **acpPollEvents**: Poll for events (100ms interval)
- **acpRespondPermission**: Respond to permission request
- **acpListSessions**: List active sessions
- **acpResumeSession**: Resume existing session
- **acpDeleteSession**: Delete session
- **acpGetPersistedSessions**: Get persisted sessions
- **acpSaveSessionMeta**: Save session metadata
- **acpUpdateSessionTitle**: Update session title
- **acpSetSessionMode**: Set session mode
- **acpSetConfigOption**: Set config option

#### Pool Management (Multi-Agent)
- **acpPoolCreate**: Create agent pool
- **acpPoolSubmitTask**: Submit task to pool
- **acpPoolCancelTask**: Cancel task
- **acpPoolStatus**: Get pool status
- **acpPoolShutdown**: Shutdown pool
- **acpPoolList**: List pools

#### Event Translation
Translates ACP events to UI message chunks:
- `agent_message_chunk` → text-delta
- `agent_thought_chunk` → reasoning-delta
- `tool_call` → tool-input-start/available
- `tool_call_update` → tool-output
- `permission_request` → ask-user-question
- `usage_update` → message-metadata
- `closed` → finish
- `task_dispatched` → pool-task-dispatched

---

## 4. Components (src/renderer/features/agents/)

### Key Components

#### Layout & Navigation
- **AgentsSubChatsSidebar** (sidebar/agents-subchats-sidebar.tsx): Sub-chat list with tabs, pins, search
- **AgentsContent** (ui/agents-content.tsx): Main chat area
- **ActiveChat** (main/active-chat.tsx): Active sub-chat view
- **NewChatForm** (main/new-chat-form.tsx): Create new chat form

#### Agent Selection
- **AgentSelector** (components/agent-selector.tsx): ACP runtime selector
  - Shows available runtimes (availability === "available")
  - Binds to sub-chat via subChatRuntimeIdAtomFamily
  - Two modes: subChatId (existing) or defaultRuntimeId (new)

#### Sub-Chat Management
- **SubChatSelector** (ui/sub-chat-selector.tsx): Sub-chat tab selector
- **SubChatContextMenu** (ui/sub-chat-context-menu.tsx): Right-click menu
- **SubChatStatusCard** (ui/sub-chat-status-card.tsx): Status indicator
- **AgentsRenameSubChatDialog** (components/agents-rename-subchat-dialog.tsx): Rename dialog

#### Message Display
- **MessagesList** (main/messages-list.tsx): Message list
- **AssistantMessageItem** (main/assistant-message-item.tsx): Assistant message
- **AgentUserMessageBubble** (ui/agent-user-message-bubble.tsx): User message
- **AgentToolCall** (ui/agent-tool-call.tsx): Tool call display
- **AgentThinkingTool** (ui/agent-thinking-tool.tsx): Thinking process

#### Input & Interaction
- **ChatInputArea** (main/chat-input-area.tsx): Input area
- **AgentSendButton** (components/agent-send-button.tsx): Send button
- **AgentMentionsEditor** (mentions/agents-mentions-editor.tsx): File mentions

### Supporting Components
- **AgentChatCard**: Chat preview card
- **AgentsQuickSwitchDialog**: Quick switch between agents
- **SubchatsQuickSwitchDialog**: Quick switch between sub-chats
- **AgentsOnboardingDialog**: Onboarding for new agents
- **CreateBranchDialog**: Branch creation
- **PreviewSetupHoverCard**: Preview setup guidance

---

## 5. Runtime/Agent Concepts

### ACP Runtime (runtime-types.ts)
```typescript
interface AcpRuntime {
  id: string                    // "claude", "codex", "goose"
  label: string                 // "Claude Code", "OpenAI Codex"
  avatar_url: string            // Icon URL
  availability: RuntimeAvailability  // "available" | "not_installed" | "auth_required"
  command: string | null        // Startup command
  binary_path: string | null    // Binary location
  install_hint: string          // Installation hint text
  install_instructions_url: string
  auth_status: RuntimeAuthStatus  // "logged_in" | "logged_out" | "not_applicable" | "unknown"
  login_hint: string | null
  source: RuntimeSource         // "builtin" | "custom"
}
```

### Runtime Binding
- **Global**: preferred_runtime in global config
- **Per-SubChat**: runtimeId field in sub_chats table
- **1:1 Mapping**: Each sub-chat binds to one runtime (cannot switch mid-session)
- **Migration**: Old provider IDs ("claude-code") → new runtime IDs ("claude")

### Agent Mode
- **auto**: Default execution mode (agent acts autonomously)
- **plan**: Read-only planning mode (agent proposes, user approves)
- Stored in sub_chats.mode field
- Toggled via Shift+Tab in UI

---

## 6. Key Architectural Patterns

### State Separation
- **Jotai Atoms**: UI state, preferences, per-entity bindings
- **Zustand Stores**: Complex stateful logic (tabs, queues, streaming)
- **Module-level Stores**: Chat objects, stream IDs (outside React)

### Persistence Strategy
- **localStorage**: Window-scoped keys (windowId:prefix:key)
- **SQLite (Drizzle)**: Chats, sub-chats, messages, agent collaboration
- **Migration**: Legacy numeric window IDs → string window IDs

### Data Flow
1. **User Action** → Component → Zustand Store / Jotai Atom
2. **State Change** → tRPC Mutation → Backend (Rust/SQLite)
3. **Backend Event** → tRPC Subscription/Query → State Update
4. **ACP Events** → Native Binding → Event Translation → UI Update

### Cleanup Strategy
- **Sub-Chat Close**: Clear queue, streaming status, Chat instance, task snapshot cache, runtime caches
- **Archive**: Kill terminal processes (worktree-mode only), invalidate git cache
- **Delete**: Remove worktree, cleanup terminals, track analytics

---

## 7. Migration Considerations

### Current Frontend Concepts
- **Sub-Chat**: Single agent conversation with message history
- **Runtime**: ACP-compatible agent binary (claude, codex, goose)
- **Mode**: Execution mode (auto/plan)
- **Agent Tasks/Messages**: Collaboration between agents (not fully integrated in UI)

### Backend ACP Model Alignment
- **Agent**: ACP runtime with config (command, args, env, model, provider)
- **Session**: ACP session with events and state
- **Task**: Work item for agent execution
- **Pool**: Multi-agent orchestration

### Potential Migration Paths
1. **Sub-Chat → Session**: Map sub_chats to ACP sessions
2. **Runtime → Agent**: Map runtimes to agent configs
3. **Mode → Session Mode**: Map mode field to ACP session mode
4. **Agent Tasks**: Expose in UI for multi-agent workflows
5. **Pool Management**: Add UI for agent pool creation and task submission

### Integration Points
- **tRPC agents.ts**: Already aligned with ACP (listRuntimes, getAgentConfig)
- **tRPC acp.ts**: Full ACP protocol support (sessions, events, pools)
- **sub_chats.runtimeId**: Already tracks ACP runtime binding
- **agentChatStore**: Manages Chat objects (could integrate ACP sessions)

---

## 8. File Structure Summary

```
src/
├── main/
│   ├── lib/
│   │   ├── db/schema/
│   │   │   ├── index.ts              # chats, sub_chats tables
│   │   │   └── agent-collaboration.ts # agent_tasks, agent_messages
│   │   └── trpc/routers/
│   │       ├── agents.ts             # ACP runtime management
│   │       ├── chats.ts              # Chat/sub-chat operations
│   │       └── acp.ts                # ACP protocol bridge
│
└── renderer/
    └── features/
        ├── agents/
        │   ├── atoms/
        │   │   ├── index.ts          # Core atoms (mode, models, loading)
        │   │   └── runtime.ts        # ACP runtime atoms
        │   ├── stores/
        │   │   ├── sub-chat-store.ts # Tab management
        │   │   ├── agent-chat-store.ts # Chat objects
        │   │   ├── message-queue-store.ts
        │   │   └── streaming-status-store.ts
        │   ├── components/
        │   │   ├── agent-selector.tsx # Runtime selector
        │   │   └── ...
        │   ├── ui/
        │   │   ├── agents-content.tsx
        │   │   ├── sub-chat-*.tsx
        │   │   └── agent-*.tsx
        │   └── lib/
        │       ├── runtime-types.ts   # ACP runtime types
        │       └── agents-actions.ts  # Action handlers
        │
        └── sidebar/
            └── agents-subchats-sidebar.tsx  # Main sidebar
```

---

## 9. Key Takeaways

1. **Sub-Chat is the Core Unit**: Each sub-chat is an independent agent conversation with its own runtime binding, mode, and message history.

2. **ACP Integration is Ready**: Backend fully supports ACP protocol. Frontend has runtime selection, binding, and event translation.

3. **State is Well-Separated**: Jotai for UI state, Zustand for complex logic, module stores for Chat objects.

4. **Multi-Agent Foundation Exists**: agent_tasks and agent_messages tables are defined but not fully exposed in UI.

5. **Runtime is 1:1 with Sub-Chat**: Cannot switch runtime mid-session. New sub-chat required for different runtime.

6. **Mode Controls Autonomy**: "auto" = autonomous, "plan" = read-only with approval required.

7. **Cleanup is Critical**: Sub-chat close must cleanup queue, streaming, Chat instance, and caches to prevent memory leaks.

8. **Migration Path is Clear**: Map sub_chats → ACP sessions, runtimes → agent configs, expose agent_tasks in UI for multi-agent workflows.
