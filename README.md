# murmur

Dead-simple local IPC for AI agents. Unix sockets, newline-delimited messages. No HTTP, no auth, no fluff.

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Listen on a channel (prints incoming messages to stdout)
murmur listen mychannel

# Send a message
murmur send mychannel "hello from agent-1"

# Pipe stdin
echo '{"task": "summarize", "id": 42}' | murmur send mychannel

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

# Terminal 2 - coordinator sends work
murmur send tasks "summarize document.pdf"
murmur send tasks "translate output to french"
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
