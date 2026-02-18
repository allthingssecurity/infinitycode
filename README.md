# InfinityCode (AgentFS)

A Rust workspace for a durable SQLite-backed filesystem and audit layer for AI agents.

This repo ships three binaries:

- `infinity` - AgentFS CLI (filesystem, KV, audit, integrity, snapshots)
- `infinity-agent` - interactive coding agent CLI with AgentFS-backed sessions
- `agentfs-mcp` - MCP JSON-RPC server over stdio

## Prerequisites

- Rust + Cargo (stable)
- macOS/Linux shell
- For `infinity-agent`: Claude auth via `infinity-agent login` or `ANTHROPIC_API_KEY`

## Install

### Option 1: Build from source

```bash
git clone https://github.com/allthingssecurity/infinitycode.git
cd infinitycode
cargo build --release --workspace
```

Binaries will be available at:

- `target/release/infinity`
- `target/release/infinity-agent`
- `target/release/agentfs-mcp`

Optional global install:

```bash
install -m 755 target/release/infinity /usr/local/bin/infinity
install -m 755 target/release/infinity-agent /usr/local/bin/infinity-agent
install -m 755 target/release/agentfs-mcp /usr/local/bin/agentfs-mcp
```

### Option 2: Use GitHub release binaries

1. Open the latest release: `https://github.com/allthingssecurity/infinitycode/releases/latest`
2. Download the matching binary for your OS/CPU.
3. Make it executable and move it into your PATH:

```bash
chmod +x infinity*
mv infinity* /usr/local/bin/
```

## Quick Start

### 1) Initialize a database

```bash
infinity init ./infinity.db --durability normal
```

### 2) Filesystem operations

```bash
infinity fs mkdir ./infinity.db /docs
infinity fs write ./infinity.db /docs/hello.txt "hello world"
infinity fs ls ./infinity.db /
infinity fs cat ./infinity.db /docs/hello.txt
```

### 3) Key-value operations

```bash
infinity kv set ./infinity.db app.name infinity
infinity kv get ./infinity.db app.name
```

### 4) Database info and timeline

```bash
infinity info ./infinity.db
infinity timeline ./infinity.db --limit 50
```

### 5) Agent CLI

```bash
infinity-agent login
infinity-agent chat --db ./infinity.db
infinity-agent chat --db ./infinity.db -p "summarize current project"
infinity-agent sessions --db ./infinity.db
```

### 6) MCP server

```bash
agentfs-mcp
```

The MCP server communicates using JSON-RPC over stdio.

## Command Reference

Top-level commands:

- `infinity init <PATH>`
- `infinity info <PATH>`
- `infinity fs <subcommand>`
- `infinity kv <subcommand>`
- `infinity tools <subcommand>`
- `infinity timeline <PATH>`
- `infinity integrity <subcommand>`
- `infinity gc <PATH>`
- `infinity snapshot <PATH> <OUT>`
- `infinity checkpoint <PATH>`
- `infinity migrate <PATH>`
- `infinity sessions <subcommand>`
- `infinity analytics <subcommand>`

Help commands:

```bash
infinity --help
infinity fs --help
infinity kv --help
infinity-agent --help
```

## Build and Release Binaries

Use these commands to create a GitHub release with binaries:

```bash
# From repo root
cargo build --release --workspace

mkdir -p dist
cp target/release/infinity dist/infinity-darwin-arm64
cp target/release/infinity-agent dist/infinity-agent-darwin-arm64
cp target/release/agentfs-mcp dist/agentfs-mcp-darwin-arm64

VERSION=v0.1.0
gh release create "$VERSION" \
  dist/infinity-darwin-arm64 \
  dist/infinity-agent-darwin-arm64 \
  dist/agentfs-mcp-darwin-arm64 \
  --title "InfinityCode $VERSION" \
  --notes "Initial release: infinity, infinity-agent, and agentfs-mcp binaries."
```

For Linux, build on Linux and upload matching artifacts (for example `*-linux-x86_64`).
