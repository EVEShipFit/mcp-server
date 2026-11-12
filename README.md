# EVEShipFit MCP server

An [MCP](https://modelcontextprotocol.io) server, over HTTP, that calculates EVE Online ship fits.
It serves the tools of [esf-mcp](https://github.com/EVEShipFit/dogma-engine/tree/main/crates/esf-mcp), statelessly.

## Tools

- `calculate_fit`: DPS, volley, EHP, resists, capacitor, speed, targeting, fitting use, and every rule the fit breaks.
- `get_attributes`: the raw attributes of the ship, an item or a charge, optionally with what modified them.
- `find_type`: search items by name.
- `find_variations`: the meta variants of an item; given a fit, each is swapped in and recalculated.
- `compare_fits`: every stat that differs between two fits.
- `convert_fit`: to esf/1, EFT or ESI fitting JSON.

Fits are esf/1, EFT, DNA, ESI fitting JSON or ESI killmail JSON.
The [esf/1 specification](https://github.com/EVEShipFit/dogma-engine/blob/main/crates/esf-mcp/esf-spec.md) is offered as the resource `esf://spec`.
Skills default to all level 5.

## Run

```bash
docker run -p 8080:8080 -v esf-mcp-cache:/cache ghcr.io/eveshipfit/mcp-server
```

On first start it downloads the SDE (about 25 MB) into `/cache`; the volume keeps it across restarts.
A bind mount instead must be writable by uid 65532.
Every hour it checks whether a new SDE is due, and switches to it without a restart.

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

`CACHE_DIR` moves the cache; by default it is in the system's temporary directory.
