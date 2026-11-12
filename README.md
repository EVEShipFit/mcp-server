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

Everyone who uses the server logs in with an EVE Online character first.
Create an application at [developers.eveonline.com](https://developers.eveonline.com) with no scopes, and its callback set to `<PUBLIC_URL>/callback`.

```bash
docker run -p 8080:8080 -v esf-mcp-cache:/cache \
  -e PUBLIC_URL=https://mcp.example.com \
  -e EVE_CLIENT_ID=... -e EVE_CLIENT_SECRET=... \
  -e SIGNING_KEY="$(openssl rand -base64 32)" \
  ghcr.io/eveshipfit/mcp-server
```

| Setting | |
|---|---|
| `PUBLIC_URL` | The address clients reach the server at; HTTPS, or `http://localhost`. |
| `EVE_CLIENT_ID`, `EVE_CLIENT_SECRET` | The EVE Online application. |
| `SIGNING_KEY` | At least 32 characters. Run several instances with the same key; changing it logs everyone out. |
| `EVE_SSO_URL` | Default `https://login.eveonline.com`. |
| `RATE_LIMIT` | Requests a minute each character may make; default 60. Counted per instance. |
| `CPU_LIMIT` | Seconds of CPU time a minute each character's tool calls may use; default 10. A call that takes longer may run, after which every request of that character waits until the budget is back. Counted per instance. |
| `NO_LOGIN` | `true` serves without login, for use on your own machine. |
| `CACHE_DIR` | Where the SDE is kept; `/cache` in the image. |

On first start it downloads the SDE (about 25 MB) into `/cache`; the volume keeps it across restarts.
A bind mount instead must be writable by uid 65532.
Every hour it checks whether a new SDE is due, and switches to it without a restart.
A character runs at most 2 requests at once.
A tool call that takes more than a second of CPU is logged with who made it and its arguments.

- `POST /mcp`: the MCP endpoint.
- `GET /healthz`: answers `ok` once the server is up.
- `/.well-known/oauth-protected-resource`, `/.well-known/oauth-authorization-server`, `/register`, `/authorize`, `/token`: how MCP clients log in.

Connect Claude Code to it; it opens the browser to log in:

```bash
claude mcp add --transport http esf https://mcp.example.com/mcp
```

## Develop

```bash
cargo run -- --no-login
```

By default the cache is in the system's temporary directory.
