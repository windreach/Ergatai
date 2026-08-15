# Phase 2 Implementation Summary

## Overview
Successfully implemented Phase 2 of the Ergatai CLI TUI rewrite, adding Claude Code-style rendering with markdown support, collapsible thinking blocks, and tool call cards.

## Files Created

### 1. `/root/ergatai/crates/ergatai-cli/src/ui/widgets/mod.rs`
- Module aggregator for widget components
- Exports: `markdown`, `thinking`, `tool_card`

### 2. `/root/ergatai/crates/ergatai-cli/src/ui/widgets/markdown.rs`
- Wraps `tui_markdown::from_str()` for markdown rendering
- Returns `Text<'_>` that borrows from input
- Simple pass-through for now, can be extended with caching/debouncing later

### 3. `/root/ergatai/crates/ergatai-cli/src/ui/widgets/thinking.rs`
- `render_thinking_into(text, thinking, collapsed)` - renders thinking blocks
- **Collapsed mode**: Single line indicator "💭 Thinking… (Ctrl-T to expand)"
- **Expanded mode**: Full text with dim color and "│" prefix per line
- Uses italic styling for collapsed indicator

### 4. `/root/ergatai/crates/ergatai-cli/src/ui/widgets/tool_card.rs`
- `render_tool_card_into(text, tc)` - renders tool call cards
- **Collapsed mode**: Status icon + tool icon + name + 1-line summary
- **Expanded mode**: Pretty-printed input JSON + output (capped at 20 lines)
- `tool_icon(name)` - returns emoji based on tool type:
  - bash/shell → ⚡
  - read → 📖
  - write/edit → ✏️
  - search/grep → 🔍
  - task/agent → 🤖
  - default → 🔧
- `summarize_tool_input(name, input)` - extracts key info (command, file path, query)
- Status icons: ● (running), ✓ (success), ✗ (failed), ⊘ (denied)

## Files Modified

### 1. `/root/ergatai/crates/ergatai-cli/src/ui/app.rs`
**Changes:**
- Extended `Message::Assistant` with `thinking: String` and `tool_calls: Vec<ToolCall>`
- Added `ToolCall` struct with fields: `id`, `name`, `input`, `output`, `status`, `expanded`
- Added `ToolOutput` struct with `text` and `is_error`
- Added `ToolStatus` enum: `Running`, `Success`, `Failed`, `Denied`
- Added `AppState::collapsed_thinking: bool` (defaults to `true`)
- New methods:
  - `append_thinking_chunk(text)` - appends thinking to current assistant message
  - `add_tool_call(tc)` - adds tool call to current assistant message
  - `update_tool_call(id, result, is_error)` - updates tool output by ID
  - `toggle_thinking()` - toggles global thinking visibility

### 2. `/root/ergatai/crates/ergatai-cli/src/ui/mod.rs`
**Changes:**
- Added `pub mod widgets;` to export widget module

### 3. `/root/ergatai/crates/ergatai-cli/src/ui/render/message.rs`
**Changes:**
- Updated `render_into()` signature to accept `collapsed_thinking: bool`
- Assistant message rendering now:
  1. Renders header with optional "●" indicator
  2. Renders thinking block (if any) via `thinking::render_thinking_into()`
  3. Renders tool cards (if any) via `tool_card::render_tool_card_into()`
  4. Renders main content as markdown via `markdown::render_markdown()`
  5. Converts borrowed `Text<'_>` to owned `Line<'static>` for aggregation
- Handles empty content with "…" placeholder during streaming

### 4. `/root/ergatai/crates/ergatai-cli/src/ui/render/messages.rs`
**Changes:**
- Updated call to `message::render_into()` to pass `app.collapsed_thinking`

### 5. `/root/ergatai/crates/ergatai-cli/src/ui/render/mod.rs`
**Changes:**
- Added Ctrl-T keybinding to toggle thinking visibility
- Updated `handle_acp_event()` to handle:
  - `agent_thought_chunk` - calls `append_thinking_chunk()`
  - `tool_call` - parses and calls `add_tool_call()`
  - `tool_call_update` - parses and calls `update_tool_call()`
- Added `parse_tool_call(data)` - parses tool call JSON (handles both snake_case and camelCase)
- Added `parse_tool_call_update(data)` - parses tool update JSON

## Key Implementation Details

### Markdown Rendering
- Uses `tui_markdown::from_str()` which returns `Text<'_>` (borrows from input)
- Converts to owned `Text<'static>` by cloning each span's content
- Called fresh on every frame (fast enough for streaming chat)
- Handles partial markdown correctly during streaming

### Thinking Blocks
- Default collapsed (global toggle via Ctrl-T)
- Collapsed: italic dim text with hint
- Expanded: dim text with "│" prefix per line
- No duration tracking yet (Phase 2 scope)

### Tool Cards
- Default collapsed (no expansion UI in Phase 2)
- Status-based styling (yellow=running, green=success, red=failed, gray=denied)
- Smart summaries: extracts command/file path/query based on tool name
- Falls back to truncated JSON string for unknown tools

### Event Handling
- Supports both snake_case (`tool_name`) and camelCase (`toolName`) field names
- Gracefully handles missing fields with defaults
- Tool updates search backwards through messages for matching ID

## Build Results

✅ **Build**: Success (1 warning about unused `Denied` variant)
✅ **Tests**: 6/6 passing (all existing `parse_input` tests)
✅ **Clippy**: 0 errors in ergatai-cli (14 pre-existing warnings in ergatai-core)

## API Surprises

1. **tui-markdown signature**: `from_str(&str) -> Text<'_>` returns borrowed text, not owned
   - Solution: Convert to owned by cloning span contents during aggregation
   - Performance: Fast enough for streaming (no caching needed yet)

2. **Lifetime issues**: Initial implementation had lifetime errors when pushing borrowed references into `Text<'static>`
   - Solution: Use `.clone()` on strings that reference the ToolCall struct
   - All other strings created via `format!()` are already owned

3. **JSON field variations**: ACP events use both snake_case and camelCase
   - Solution: Check both variants with `.or_else()` fallback chain

## Testing Recommendations

Future phases should add tests for:
- `parse_tool_call()` with various JSON formats
- `parse_tool_call_update()` with missing fields
- `summarize_tool_input()` for different tool types
- `render_tool_card_into()` output verification
- Thinking block collapse/expand state

## Next Steps (Phase 3+)

- Tool card expansion UI (mouse click or keyboard navigation)
- Thinking duration tracking and display
- Better tool input summarization for more tool types
- Caching markdown rendering for long messages
- Syntax highlighting improvements
- Real-time tool execution progress indicators
