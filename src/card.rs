//! The MCP Server Card, at `<mcp url>/server-card`.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use sha2::{Digest, Sha256};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer, ExposeHeaders};

pub const PATH: &str = "/mcp/server-card";
const MEDIA_TYPE: &str = "application/mcp-server-card+json";

#[derive(Clone)]
struct Card {
    body: String,
    etag: HeaderValue,
}

/// The card of the MCP server at `mcp_url`, which runs esf-mcp `version`.
pub fn routes(mcp_url: &str, version: &str) -> Router {
    let site = mcp_url.strip_suffix("/mcp").unwrap_or(mcp_url);
    let body = json!({
        "$schema": "https://static.modelcontextprotocol.io/schemas/v1/server-card.schema.json",
        "name": "fit.eveship/mcp",
        "version": version,
        "title": "EVEShip.fit",
        "description": "Calculates EVE Online ship fits: damage, tank, capacitor, speed, targeting and fitting.",
        "websiteUrl": format!("{site}/"),
        "repository": { "url": "https://github.com/EVEShipFit/mcp-server", "source": "github" },
        "icons": [{ "src": format!("{site}/logo.png"), "mimeType": "image/png", "sizes": ["64x64"] }],
        "remotes": [{ "type": "streamable-http", "url": mcp_url }],
    })
    .to_string();
    let etag = format!(
        "\"{}\"",
        URL_SAFE_NO_PAD.encode(Sha256::digest(body.as_bytes()))
    );
    let card = Card {
        body,
        etag: HeaderValue::from_str(&etag).expect("an ETag of base64 characters"),
    };
    Router::new()
        .route(PATH, get(serve))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::any())
                .allow_methods(AllowMethods::list([axum::http::Method::GET]))
                .allow_headers(AllowHeaders::list([
                    header::CONTENT_TYPE,
                    header::IF_NONE_MATCH,
                ]))
                .expose_headers(ExposeHeaders::list([header::ETAG])),
        )
        .with_state(card)
}

async fn serve(State(card): State<Card>, headers: HeaderMap) -> Response {
    let etag = card.etag.to_str().unwrap_or_default();
    let fresh = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|tag| tag.trim().trim_start_matches("W/"))
        .any(|tag| tag == "*" || tag == etag);
    let mut response = if fresh {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        card.body.into_response()
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    headers.insert(header::ETAG, card.etag);
    response
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn app() -> Router {
        routes("https://mcp.example.com/mcp", "15.0.0")
    }

    #[tokio::test]
    async fn describes_the_server() {
        let response = app()
            .oneshot(Request::get(PATH).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers().clone();
        assert_eq!(headers[header::CONTENT_TYPE], MEDIA_TYPE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let card: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(card["version"], "15.0.0");
        assert_eq!(card["websiteUrl"], "https://mcp.example.com/");
        assert_eq!(
            card["remotes"],
            json!([{ "type": "streamable-http", "url": "https://mcp.example.com/mcp" }])
        );
        assert!(card["description"].as_str().unwrap().len() <= 100);

        let etag = headers[header::ETAG].to_str().unwrap();
        for (if_none_match, status) in [
            (etag.to_string(), StatusCode::NOT_MODIFIED),
            (format!("\"other\", W/{etag}"), StatusCode::NOT_MODIFIED),
            ("*".to_string(), StatusCode::NOT_MODIFIED),
            ("\"other\"".to_string(), StatusCode::OK),
        ] {
            let response = app()
                .oneshot(
                    Request::get(PATH)
                        .header(header::IF_NONE_MATCH, &if_none_match)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{if_none_match}");
        }
    }

    #[tokio::test]
    async fn lets_any_site_read_it() {
        let response = app()
            .oneshot(
                Request::get(PATH)
                    .header(header::ORIGIN, "https://eveship.fit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_EXPOSE_HEADERS],
            "etag"
        );
    }
}
