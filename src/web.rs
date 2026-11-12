//! The pages people see in a browser, in the look of eveship.fit.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

const LOGO: &[u8] = include_bytes!("../assets/logo.png");
const FAVICON: &[u8] = include_bytes!("../assets/favicon.ico");

const STYLE: &str = "
:root{--bg:#0b0d0f;--surface:#16191c;--raised:#1f2327;--border:#3a3f44;--text:#d8dde2;--muted:#8a9096;
--accent:#57a6be;--button:#366370;--input:#0b0c0c;--warning:#ff7f00;--tab:#2d2e2f;--tab-active:#263d47}
html,body{margin:0}
body{background:radial-gradient(ellipse 80% 60% at 50% 45%,rgb(40 70 90/22%),transparent 70%) fixed,var(--bg);
color:var(--text);font-family:\"Noto Sans Variable\",\"Noto Sans\",system-ui,-apple-system,\"Segoe UI\",sans-serif;
font-size:15px;line-height:1.55;min-height:100dvh;display:flex;flex-direction:column}
header{display:flex;align-items:center;gap:8px 24px;flex-wrap:wrap;justify-content:space-between;
min-width:0;padding:10px 20px;background:rgb(11 13 15/80%);border-bottom:1px solid rgb(255 255 255/6%)}
.title{font-size:14px;font-weight:normal;margin:0}
.brand{color:inherit;font-size:18px;font-weight:600;letter-spacing:.02em;text-decoration:none}
.brand img{margin-right:8px;vertical-align:-.4em}
.dash,.tagline{color:var(--muted)}
main{box-sizing:border-box;width:100%;max-width:46rem;margin:0 auto;padding:32px 20px;flex:1}
h1{font-size:26px;margin:0 0 12px}
h2{font-size:18px;margin:32px 0 8px}
h3{font-size:15px;margin:20px 0 6px}
p{margin:0 0 12px}
a{color:var(--accent)}
.muted{color:var(--muted)}
.card{background:var(--surface);border:1px solid var(--border);border-radius:8px;padding:20px;
box-shadow:0 16px 48px rgb(0 0 0/60%)}
.card h1{display:flex;align-items:center;gap:12px}
.dialog{max-width:34rem;margin:0 auto}
.warning{color:var(--warning)}
code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:13px}
code{background:var(--raised);border-radius:4px;padding:1px 5px}
pre{background:var(--input);border:1px solid var(--border);border-radius:6px;padding:10px 12px;
overflow-x:auto;margin:0 0 12px}
pre code{background:none;padding:0}
.url{font-size:16px;padding:12px 14px;user-select:all}
ul{margin:0 0 12px;padding-left:20px}
li{margin:2px 0}
button{background:var(--button);border:1px solid var(--accent);border-radius:999px;color:var(--text);
cursor:pointer;font:inherit;font-weight:600;padding:8px 20px}
button:hover,button:focus-visible{background:#43788a}
.tabs{display:flex;flex-wrap:wrap;gap:4px;margin:0 0 12px}
.tabs input{position:absolute;opacity:0;pointer-events:none}
.tabs label{background:var(--tab);border:1px solid var(--border);border-radius:6px 6px 0 0;
border-bottom:none;cursor:pointer;padding:6px 14px;user-select:none}
.tabs label:hover{background:var(--raised)}
.tabs input:checked+label{background:var(--tab-active);border-color:var(--accent)}
.tabs input:focus-visible+label{outline:2px solid var(--accent);outline-offset:2px}
.tabs .panel{display:none;order:1;width:100%;box-sizing:border-box;background:var(--surface);
border:1px solid var(--border);border-radius:0 6px 6px 6px;padding:16px 16px 4px;margin-top:-1px}
.tabs input:checked+label+.panel{display:block}
footer{color:var(--muted);font-size:13px;padding:16px 20px;text-align:center;
border-top:1px solid rgb(255 255 255/6%)}
";

/// Text made safe to put in HTML.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A page with the eveship.fit header and footer around `body`.
pub fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let document = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title><link rel=\"icon\" href=\"/favicon.ico\"><style>{STYLE}</style></head><body>\
         <header><p class=\"title\"><a class=\"brand\" href=\"https://eveship.fit\">\
         <img src=\"/logo.png\" width=\"28\" height=\"28\" alt=\"\">EVEShip.fit</a>\
         <span class=\"dash\"> - </span><span class=\"tagline\">EVE Online ship fits for your AI assistant</span>\
         </p></header>\
         <main>{body}</main>\
         <footer>Part of <a href=\"https://eveship.fit\">EVEShip.fit</a> · \
         <a href=\"https://github.com/EVEShipFit/mcp-server\">source on GitHub</a> · \
         EVE Online and all related materials are property of CCP Games.</footer>\
         </body></html>",
        title = escape(title),
    );
    let mut response = (status, Html(document)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; frame-ancestors 'none'",
        ),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A page saying only `message`.
pub fn message(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<div class=\"card dialog\"><h1><img src=\"/logo.png\" width=\"48\" height=\"48\" alt=\"\">EVEShip.fit</h1><p>{}</p></div>",
        escape(message)
    );
    page(status, "EVEShip.fit", &body)
}

fn asset(bytes: &'static [u8], content_type: &'static str) -> Response {
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    response
}

/// What the front page tells people.
#[derive(Clone)]
pub struct About {
    pub mcp_url: String,
    pub requests_per_minute: u32,
    pub cpu_seconds_per_minute: u32,
}

/// The front page, the logo and the favicon.
pub fn routes(about: About) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/logo.png", get(|| async { asset(LOGO, "image/png") }))
        .route(
            "/favicon.ico",
            get(|| async { asset(FAVICON, "image/x-icon") }),
        )
        .with_state(about)
}

/// Tabs that work without scripts: a radio button per tab, its label, and its panel.
fn tabs(tabs: &[(&str, &str, String)]) -> String {
    let mut html = String::from("<div class=\"tabs\">");
    for (index, (id, label, panel)) in tabs.iter().enumerate() {
        let checked = if index == 0 { " checked" } else { "" };
        html.push_str(&format!(
            "<input type=\"radio\" name=\"assistant\" id=\"tab-{id}\"{checked}>\
             <label for=\"tab-{id}\">{label}</label><div class=\"panel\">{panel}</div>"
        ));
    }
    html.push_str("</div>");
    html
}

async fn index(State(about): State<About>) -> Response {
    let url = escape(&about.mcp_url);
    let requests = about.requests_per_minute;
    let cpu = about.cpu_seconds_per_minute;
    let tabs = tabs(&[
        (
            "claude",
            "Claude",
            "<p>In claude.ai or the Claude desktop app: Customize → Connectors → <em>+ Add</em> → \
             <em>Add custom connector</em>. Give it a name, paste the address above, and press Continue, \
             then Add; your browser opens to log in.</p>\
             <p class=\"muted\">On Team and Enterprise, an owner adds it first under Organization settings → Connectors.</p>"
                .to_string(),
        ),
        (
            "claude-code",
            "Claude Code",
            format!(
                "<pre><code>claude mcp add --transport http eveship-fit {url}</code></pre>\
                 <p>Then run <code>/mcp</code>, pick <code>eveship-fit</code> and log in.</p>"
            ),
        ),
        (
            "chatgpt",
            "ChatGPT",
            "<p>Go to <a href=\"https://chatgpt.com/plugins\">chatgpt.com/plugins</a>, press +, then \
             <em>Add custom MCP server</em>. Give it a name, paste the address above, choose OAuth, \
             and select <em>Create as a plugin</em>.</p>\
             <p class=\"muted\">Your plan or workspace may need to allow custom MCP servers.</p>"
                .to_string(),
        ),
        (
            "cursor",
            "Cursor",
            format!(
                "<p>In <code>~/.cursor/mcp.json</code>, or <code>.cursor/mcp.json</code> in a project:</p>\
                 <pre><code>{{\n  \"mcpServers\": {{\n    \"eveship-fit\": {{ \"url\": \"{url}\" }}\n  }}\n}}</code></pre>"
            ),
        ),
        (
            "vscode",
            "VS Code",
            format!(
                "<p>In <code>.vscode/mcp.json</code>, or <em>MCP: Add Server</em> from the command palette:</p>\
                 <pre><code>{{\n  \"servers\": {{\n    \"eveship-fit\": {{ \"type\": \"http\", \"url\": \"{url}\" }}\n  }}\n}}</code></pre>"
            ),
        ),
        (
            "antigravity",
            "Antigravity",
            format!(
                "<p>In <code>~/.gemini/config/mcp_config.json</code>:</p>\
                 <pre><code>{{\n  \"mcpServers\": {{\n    \"eveship-fit\": {{ \"serverUrl\": \"{url}\" }}\n  }}\n}}</code></pre>"
            ),
        ),
        (
            "gemini",
            "Gemini CLI",
            format!(
                "<p>In <code>~/.gemini/settings.json</code>:</p>\
                 <pre><code>{{\n  \"mcpServers\": {{\n    \"eveship-fit\": {{ \"httpUrl\": \"{url}\" }}\n  }}\n}}</code></pre>\
                 <p>Then run <code>/mcp auth eveship-fit</code> to log in.</p>"
            ),
        ),
    ]);
    let body = format!(
        "<h1>EVE Online ship fits, for your AI</h1>\
<p>This is the <a href=\"https://modelcontextprotocol.io\">MCP</a> server of \
<a href=\"https://eveship.fit\">EVEShip.fit</a>. Add it to an AI assistant like Claude or ChatGPT, \
and it calculates fits with the same engine as eveship.fit: damage, tank, capacitor, speed, \
targeting, fitting, and every rule a fit breaks.</p>\
<p>You log in once with your EVE Online character. That only tells us who you are; \
we ask for no scopes and read nothing from your account.</p>\
<p>The server address:</p>\
<pre class=\"url\"><code>{url}</code></pre>\
\
<h2>Add it to your assistant</h2>\
{tabs}\
<p class=\"muted\">Anything else that speaks MCP over HTTP, with OAuth, works with the address above.</p>\
\
<h2>What it can do</h2>\
<ul>\
<li><code>calculate_fit</code>: DPS, volley, EHP, resists, capacitor, speed, targeting, fitting use, \
and every rule the fit breaks.</li>\
<li><code>compare_fits</code>: every stat that differs between two fits.</li>\
<li><code>find_variations</code>: the meta variants of an item; given a fit, each one swapped in and recalculated.</li>\
<li><code>get_attributes</code>: the raw attributes of the ship, an item or a charge, and what changed them.</li>\
<li><code>find_type</code>: search items by name.</li>\
<li><code>convert_fit</code>: between esf/1, EFT and ESI fitting JSON.</li>\
</ul>\
<p>Fits can be pasted as EFT (what EVE copies to the clipboard), DNA, ESI JSON or esf/1. \
Skills count as all level 5, unless you tell your assistant otherwise.</p>\
<p>Try it: paste a fit from EVE and ask <em>\"What is the DPS and tank of this fit, \
and what would a faction damage mod change?\"</em></p>\
\
<h2>Fair use</h2>\
<p class=\"muted\">Each character can make {requests} requests a minute and use {cpu} seconds of CPU a minute, \
which is far more than a conversation needs. Calls that take unusually long are logged, with the fit, \
so we can make them faster.</p>"
    );
    page(StatusCode::OK, "EVEShip.fit for your AI", &body)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    async fn get(path: &str) -> (StatusCode, String, Vec<u8>) {
        let app = routes(About {
            mcp_url: "https://mcp.example.com/mcp".to_string(),
            requests_per_minute: 60,
            cpu_seconds_per_minute: 10,
        });
        let response = app
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let content_type = response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_string();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, content_type, body.to_vec())
    }

    #[tokio::test]
    async fn explains_how_to_connect() {
        let (status, content_type, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"));
        let body = String::from_utf8(body).unwrap();
        assert!(
            body.contains(
                "claude mcp add --transport http eveship-fit https://mcp.example.com/mcp"
            )
        );
        assert!(body.contains("href=\"https://eveship.fit\""));
        assert!(body.contains("60 requests a minute and use 10 seconds of CPU"));
    }

    #[tokio::test]
    async fn serves_the_logo_and_favicon() {
        let (status, content_type, body) = get("/logo.png").await;
        assert_eq!(
            (status, content_type.as_str()),
            (StatusCode::OK, "image/png")
        );
        assert_eq!(body, LOGO);
        let (status, content_type, _) = get("/favicon.ico").await;
        assert_eq!(
            (status, content_type.as_str()),
            (StatusCode::OK, "image/x-icon")
        );
    }

    #[test]
    fn escapes_text() {
        assert_eq!(
            escape("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
    }
}
