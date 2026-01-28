# murmur

Dead-simple local IPC for AI agents. Unix sockets, newline-delimited messages. No HTTP, no auth, no fluff.

## Install

```bash
cargo install --path .
```

## Quick Start

```bash
# Start a channel (first caller becomes server, stays running)
murmur mychannel

# In another terminal, send messages
murmur send mychannel "hello from agent-1"
murmur send mychannel "another message"
```

The `murmur <channel>` command prints instructions for other agents and handles everything automatically.

## Usage

```bash
# Connect to a channel (server mode if first, client mode if exists)
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

# Bidirectional 1:1 duplex (first caller binds, second connects, both exit on close)
murmur pair chat

# Pub/sub broadcast
murmur pub feed          # reads stdin, broadcasts to all subscribers
murmur sub feed          # prints broadcasts to stdout

# Housekeeping
murmur ls                # list active channels
murmur rm mychannel      # remove a channel socket
```

## Examples

### Agent-to-agent communication (recommended)

```bash
# Terminal 1 - start a channel
murmur work
# Prints instructions, waits for connections, stays running

# Terminal 2 - send messages (can connect/disconnect multiple times)
murmur send work "summarize document.pdf"
murmur send work "translate output to french"

# Or join for bidirectional chat
murmur work
# Now both sides can send/receive
```

### Request/reply pattern

```bash
# Terminal 1 - agent listens and replies
murmur listen tasks  # handle connections that expect replies in your app

# Terminal 2 - send and get reply
RESULT=$(murmur send --reply tasks '{"cmd": "summarize", "file": "doc.pdf"}')
echo "Agent replied: $RESULT"
```

### 1:1 Bidirectional duplex

```bash
# Terminal 1
murmur pair chat

# Terminal 2
murmur pair chat
# Now both sides: stdin → socket, socket → stdout
# Exits when either side disconnects
```

### Fan-out to multiple workers

```bash
# Publisher
tail -f /var/log/events.json | murmur pub events

# Workers (each gets every message)
murmur sub events | worker-1
murmur sub events | worker-2
```

## Protocol

- Transport: Unix domain sockets at `/tmp/murmur-<channel>.sock` (canonicalized to `/private/tmp` on macOS)
- Framing: newline-delimited (`\n` terminated)
- Max message size: 1 MB
- Encoding: opaque bytes — use text, JSON, base64, whatever you want

## License

MIT
