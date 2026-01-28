# murmur

Dead-simple local IPC for AI agents. Unix sockets, newline-delimited messages. No HTTP, no auth, no fluff.

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Listen on a channel (prints incoming messages to stdout, blocks)
murmur listen mychannel

# Send a message (retries for up to 5s if listener isn't up yet)
murmur send mychannel "hello from agent-1"

# Fail immediately if listener isn't up (no retry)
murmur send --no-wait mychannel "hello"

# Send and wait for a reply (one line back)
murmur send --reply mychannel '{"cmd": "status"}'

# Combine: wait for listener + get reply
murmur send --reply mychannel "ping"

# Pipe stdin
echo '{"task": "summarize", "id": 42}' | murmur send mychannel

# Bidirectional duplex (first caller binds, second connects)
murmur pair chat    # terminal 1 — binds, waits for peer
murmur pair chat    # terminal 2 — connects, both sides talk

# Pub/sub broadcast
murmur pub feed          # reads stdin, broadcasts to all subscribers
murmur sub feed          # prints broadcasts to stdout

# Housekeeping
murmur ls                # list active channels
murmur rm mychannel      # remove a channel socket
```

## Examples

Two agents talking:

```bash
# Terminal 1 - agent listens for work
murmur listen tasks | while read -r msg; do
  echo "got: $msg"
done

# Terminal 2 - coordinator sends work (waits for listener)
murmur send tasks "summarize document.pdf"
murmur send tasks "translate output to french"
```

Request/reply pattern:

```bash
# Terminal 1 - agent listens and replies
murmur listen tasks  # handle connections that expect replies in your app

# Terminal 2 - send and get reply
RESULT=$(murmur send --reply tasks '{"cmd": "summarize", "file": "doc.pdf"}')
echo "Agent replied: $RESULT"
```

Bidirectional duplex:

```bash
# Terminal 1
murmur pair chat

# Terminal 2
murmur pair chat
# Now both sides: stdin → socket, socket → stdout
```

Fan-out to multiple workers:

```bash
# Publisher
tail -f /var/log/events.json | murmur pub events

# Workers (each gets every message)
murmur sub events | worker-1
murmur sub events | worker-2
```

## Protocol

- Transport: Unix domain sockets at `/tmp/murmur-<channel>.sock`
- Framing: newline-delimited (`\n` terminated)
- Max message size: 1 MB
- Encoding: opaque bytes — use text, JSON, base64, whatever you want

## License

MIT
