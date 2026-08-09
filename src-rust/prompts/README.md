# Prompts Directory

Agent prompt templates for Ergatai's orchestration system.

## Files

### `dag_orchestration.md`

**Purpose**: Injected into agent sessions when executing DAG tasks.

**When**: Automatically prepended to the instruction when `node_id` is present (DAG task).

**Contains**:
- Available agents list (auto-populated at runtime)
- Communication methods (@mentions, DAG templates)
- Best practices for multi-agent collaboration
- Output format guidelines

**Template Variables**:
- `{{agent_list}}` — replaced with discovered agents at runtime

## Usage

These prompts are loaded via `include_str!()` at compile time and injected into agent sessions through ACP protocol.

Example from `agent_launcher.rs`:
```rust
let dag_prompt = include_str!("../../prompts/dag_orchestration.md");
// ... populate template variables ...
let full_instruction = format!("{}\n\n---\n\n{}", dag_prompt, user_instruction);
```

## Adding New Prompts

1. Create `.md` file in this directory
2. Use `{{variable}}` for runtime substitution
3. Load via `include_str!("../../prompts/your_prompt.md")`
4. Inject into agent session via ACP `SendPrompt`

## Design Principles

- **Self-contained**: Each prompt should be complete and understandable on its own
- **Template-friendly**: Use `{{variable}}` for dynamic content
- **Agent-focused**: Written from the agent's perspective (second person)
- **Concise**: Agents have limited context, keep prompts focused
