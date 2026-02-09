# murmur

Dead-simple local IPC for AI agents. Unix sockets, newline-delimited messages. No HTTP, no auth, no fluff.

## Install

```bash
cargo install --path .
```

## Quick Start

```bash
# Start a channel (first caller becomes host, stays running)
murmur mychannel

# In another terminal, send messages
murmur send mychannel "hello from agent-1"
murmur send mychannel "another message"
```

## Multi-Agent Coordination

Start the orchestrator, then agents coordinate automatically via editor hooks:

```bash
# Terminal 1 — start the orchestrator
murmur orchestrate

# Terminal 2 — spawn agents in tmux panes
murmur spawn agent-1 -- claude
murmur spawn agent-2 -- claude
```

Agents get file locking (no edit conflicts), message delivery (via `additionalContext` injection), and agent discovery — all through the hook system with zero config in the agent itself.

### Hook Setup

Add to `.claude/settings.json` (works for both Claude Code and Cursor):

```json
{
  "hooks": {
    "PreToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "murmur hook"}]}],
    "PostToolUse": [{"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "murmur hook", "async": true}]}],
    "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "murmur hook", "async": true}]}]
  }
}
```

### MCP Server

For agents that need to proactively send messages (not just receive via hooks):

```json
{
  "mcpServers": {
    "murmur": { "command": "murmur", "args": ["mcp-server"] }
  }
}
```

Exposes `send_message`, `check_messages`, `list_agents`, and `broadcast` tools.

## Usage

```bash
# Connect to a channel (host mode if first, peer mode if exists)
murmur mychannel

# Listen on a channel (prints incoming messages to stdout, blocks)
murmur listen mychannel

# Send a message (retries for up to 5s if listener isn't up yet)
murmur send mychannel "hello from agent-1"

# Fail immediately if listener isn't up (no retry)
murmur send --no-wait mychannel "hello"

# Send and wait for a reply (one line back)
murmur send --reply mychannel '{"cmd": "status"}'

# Pipe stdin
echo '{"task": "summarize", "id": 42}' | murmur send mychannel

# Start the orchestrator (file locks, message queues, agent registry)
murmur orchestrate

# Editor hook (reads hook JSON from stdin, talks to orchestrator)
murmur hook

# MCP server over stdio (JSON-RPC 2.0)
murmur mcp-server

# Spawn an agent in a new tmux pane
murmur spawn agent-2 -- claude

# Housekeeping
murmur ls                # list active channels
murmur rm mychannel      # remove a channel socket
```

## Examples

### Agent-to-agent communication

```bash
# Terminal 1 — start a channel
murmur work
# Prints instructions, waits for connections, stays running

# Terminal 2 — send messages
murmur send work "summarize document.pdf"
murmur send work "translate output to french"

# Or join for bidirectional chat
murmur work
# Now both sides can send/receive, all messages broadcast to all peers
```

### Request/reply pattern

```bash
# Terminal 1 — agent listens and replies
murmur listen tasks

# Terminal 2 — send and get reply
RESULT=$(murmur send --reply tasks '{"cmd": "summarize", "file": "doc.pdf"}')
echo "Agent replied: $RESULT"
```

### Coordinated multi-agent workflow

```bash
# Start orchestrator
murmur orchestrate

# Spawn two agents that auto-coordinate
murmur spawn frontend -- claude --prompt "build the React UI for the auth page"
murmur spawn backend -- claude --prompt "build the API endpoints for auth"

# Send a message from one agent to another (via MCP or directly)
echo '{"action":"send","from":"me","to":"frontend","message":"backend API is ready at /api/auth"}' \
  | murmur send orchestrator
```

File locking prevents conflicts — if `frontend` tries to edit a file `backend` is writing, the hook blocks with exit code 2 and tells it who holds the lock.

### Cursor + Claude Code on the same project

Run both editors on the same codebase, coordinated through murmur:

```bash
# 1. Start the orchestrator
murmur orchestrate

# 2. Configure hooks in .claude/settings.json (both editors read this)
cat > .claude/settings.json << 'EOF'
{
  "hooks": {
    "PreToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "murmur hook"}]}],
    "PostToolUse": [{"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "murmur hook", "async": true}]}],
    "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "murmur hook", "async": true}]}]
  }
}
EOF

# 3. Open Cursor on the project — its agent gets MURMUR_AGENT_ID from session
#    Open Claude Code in a terminal — same hooks, same orchestrator

# 4. Send a message from Claude Code's agent to Cursor's agent
#    (via MCP tool, or directly)
echo '{"action":"send","from":"claude","to":"cursor-session-abc","message":"I finished the API, you can start on the frontend now"}' \
  | murmur send orchestrator

# The next time Cursor's agent runs any tool, murmur hook injects the message
# into additionalContext — the agent sees it as part of its context.
```

What happens automatically:
- **File locking**: If Cursor tries to edit `src/auth.rs` while Claude Code is writing it, the hook exits 2 and tells Cursor who holds the lock. Cursor's agent retries or works on something else.
- **Message delivery**: Messages sent to an agent queue up and get delivered on the next `PreToolUse` hook via `additionalContext`.
- **Agent discovery**: Any agent can call `list_agents` (via MCP) to see who's active.

### Using tmux to manage multiple agents

```bash
# Start orchestrator in its own pane, then spawn agents
tmux new-session -d -s murmur 'murmur orchestrate'

# Spawn Claude Code agents in new panes
murmur spawn frontend -- claude --prompt "build the auth UI components"
murmur spawn backend -- claude --prompt "build the auth API endpoints"
murmur spawn tests -- claude --prompt "write integration tests for auth"

# All three agents coordinate automatically:
# - file locks prevent edit conflicts
# - agents can message each other via MCP tools
# - the orchestrator tracks who's active
```

## Protocol

- Transport: Unix domain sockets at `/tmp/murmur-<channel>.sock` (canonicalized to `/private/tmp` on macOS)
- Framing: newline-delimited (`\n` terminated)
- Max message size: 1 MB
- Encoding: opaque bytes — use text, JSON, base64, whatever you want

## License

MIT
