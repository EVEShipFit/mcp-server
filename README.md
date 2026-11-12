# EVEShipFit MCP server

An [MCP](https://modelcontextprotocol.io) server, over HTTP, that calculates EVE Online ship fits.
It serves the tools of [esf-mcp](https://github.com/EVEShipFit/dogma-engine/tree/main/crates/esf-mcp), statelessly.

## Run

```bash
docker run -p 8080:8080 ghcr.io/eveshipfit/mcp-server
```

The image holds the SDE of the day it was built; it is rebuilt daily.

- `POST /mcp`: the MCP endpoint.
- `GET /healthz`: answers `ok` once the server is up.

Connect Claude Code to it:

```bash
claude mcp add --transport http esf http://localhost:8080/mcp
```

## Develop

```bash
cargo run
```

This downloads the newest SDE on first start; `SDE_DIR` points to a directory with `sde.dat` and `names.dat` instead.
