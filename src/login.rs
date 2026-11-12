//! Logging in with an EVE Online character: an OAuth authorization server in front of EVE's SSO.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tower_http::cors::{AllowHeaders, Any, CorsLayer};
use url::Url;

use crate::client::{self, Client};
use crate::eve::{Character, Eve};
use crate::token::{Kind, Signer, now};
use crate::web;

const REQUEST_LIFETIME: Duration = Duration::from_secs(10 * 60);
const CODE_LIFETIME: Duration = Duration::from_secs(60);
const ACCESS_LIFETIME: Duration = Duration::from_secs(60 * 60);
const SESSION_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MIN_KEY_LENGTH: usize = 32;
const MAX_STATE: usize = 1024;
const MAX_REGISTRATION: usize = 16 * 1024;
const DOCUMENT_FETCHES: usize = 8;
const LOGIN_COOKIE: &str = "__Host-login";

/// This server's address, keys and EVE application.
pub struct Login {
    base: String,
    signer: Signer,
    eve: Eve,
    documents: Semaphore,
}

/// A login on its way through the consent page.
#[derive(Serialize, Deserialize)]
struct Pending {
    client: String,
    redirect_uri: String,
    code_challenge: String,
    state: Option<String>,
}

/// A login the browser holding the login cookie agreed to, on its way through EVE's SSO.
#[derive(Serialize, Deserialize)]
struct Consented {
    pending: Pending,
    cookie: String,
}

/// What an authorization code, an access token and a refresh token grant.
#[derive(Serialize, Deserialize)]
struct Grant {
    audience: String,
    client: String,
    character: Character,
    session_ends: u64,
}

#[derive(Serialize, Deserialize)]
struct Code {
    grant: Grant,
    redirect_uri: String,
    code_challenge: String,
}

#[derive(Deserialize)]
struct Registration {
    redirect_uris: Vec<String>,
    client_name: Option<String>,
}

#[derive(Deserialize)]
struct LoginForm {
    pending: String,
}

fn hash(text: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(text.as_bytes()))
}

fn is_local(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// One value per name; `None` when a name repeats, which OAuth does not allow.
fn single(pairs: Vec<(String, String)>) -> Option<HashMap<String, String>> {
    let mut fields = HashMap::new();
    for (name, value) in pairs {
        if fields.insert(name, value).is_some() {
            return None;
        }
    }
    Some(fields)
}

impl Login {
    pub fn new(
        public_url: &str,
        signing_key: &str,
        eve_client_id: String,
        eve_client_secret: String,
        sso_url: String,
    ) -> Result<Login, String> {
        let url = Url::parse(public_url).map_err(|error| format!("PUBLIC_URL: {error}"))?;
        if url.scheme() != "https" && !(url.scheme() == "http" && is_local(&url)) {
            return Err("PUBLIC_URL: has to be HTTPS".to_string());
        }
        if url.path() != "/" || url.query().is_some() {
            return Err("PUBLIC_URL: has to be just scheme and host".to_string());
        }
        let sso = Url::parse(&sso_url).map_err(|error| format!("EVE_SSO_URL: {error}"))?;
        if sso.scheme() != "https" && !(sso.scheme() == "http" && is_local(&sso)) {
            return Err("EVE_SSO_URL: has to be HTTPS".to_string());
        }
        if signing_key.len() < MIN_KEY_LENGTH {
            return Err(format!(
                "SIGNING_KEY: has to be at least {MIN_KEY_LENGTH} characters"
            ));
        }
        let base = url.as_str().trim_end_matches('/').to_string();
        Ok(Login {
            eve: Eve {
                client_id: eve_client_id,
                client_secret: eve_client_secret,
                sso_url: sso_url.trim_end_matches('/').to_string(),
                callback: format!("{base}/callback"),
            },
            signer: Signer::new(signing_key.as_bytes()),
            documents: Semaphore::new(DOCUMENT_FETCHES),
            base,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Where MCP clients connect.
    pub fn resource(&self) -> String {
        self.url("/mcp")
    }

    fn resource_metadata(&self) -> String {
        self.url("/.well-known/oauth-protected-resource/mcp")
    }

    fn tokens(&self, grant: &Grant) -> Response {
        let left = Duration::from_secs(grant.session_ends.saturating_sub(now()));
        if left.is_zero() {
            return oauth_error("invalid_grant", "the login has expired; log in again");
        }
        let access_lifetime = ACCESS_LIFETIME.min(left);
        let access = self.signer.sign(Kind::Access, access_lifetime, grant);
        let refresh = self.signer.sign(Kind::Refresh, left, grant);
        no_store(Json(json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": access_lifetime.as_secs(),
            "refresh_token": refresh,
        })))
    }

    /// Back to the client with `pairs`, `state` and who sent it.
    fn redirect_back(
        &self,
        redirect_uri: &str,
        pairs: &[(&str, &str)],
        state: Option<&str>,
    ) -> Response {
        let Ok(mut url) = Url::parse(redirect_uri) else {
            return page(StatusCode::BAD_REQUEST, "This app's address is invalid.");
        };
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs(pairs);
            if let Some(state) = state {
                query.append_pair("state", state);
            }
            query.append_pair("iss", &self.base);
        }
        Redirect::to(url.as_str()).into_response()
    }
}

/// The OAuth endpoints, and the metadata that points MCP clients at them.
pub fn routes(login: Arc<Login>) -> Router {
    let open = Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
        .route(
            "/register",
            post(register).layer(DefaultBodyLimit::max(MAX_REGISTRATION)),
        )
        .route("/token", post(token))
        .layer(cors());
    Router::new()
        .route("/authorize", get(authorize))
        .route("/login", post(start_login))
        .route("/callback", get(callback))
        .merge(open)
        .with_state(login)
}

/// Lets browser-based MCP clients call in, with their token in a header.
pub fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(AllowHeaders::mirror_request())
        .expose_headers([header::WWW_AUTHENTICATE, header::RETRY_AFTER])
}

/// Let a request through only with an access token for this server; it then carries the [`Character`].
pub async fn require(
    State(login): State<Arc<Login>>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim().to_string());
    let grant = token
        .as_deref()
        .and_then(|token| login.signer.verify::<Grant>(Kind::Access, token));
    match grant {
        Some(grant) if grant.audience == login.resource() => {
            request.extensions_mut().insert(grant.character);
            next.run(request).await
        }
        _ => {
            let mut challenge =
                format!("Bearer resource_metadata=\"{}\"", login.resource_metadata());
            if token.is_some() {
                challenge.push_str(", error=\"invalid_token\"");
            }
            let mut response = StatusCode::UNAUTHORIZED.into_response();
            if let Ok(value) = HeaderValue::from_str(&challenge) {
                response
                    .headers_mut()
                    .insert(header::WWW_AUTHENTICATE, value);
            }
            response
        }
    }
}

async fn protected_resource(State(login): State<Arc<Login>>) -> Json<serde_json::Value> {
    Json(json!({
        "resource": login.resource(),
        "authorization_servers": [login.base],
        "bearer_methods_supported": ["header"],
        "resource_name": "EVEShip.fit",
    }))
}

async fn authorization_server(State(login): State<Arc<Login>>) -> Json<serde_json::Value> {
    Json(json!({
        "issuer": login.base,
        "authorization_endpoint": login.url("/authorize"),
        "token_endpoint": login.url("/token"),
        "registration_endpoint": login.url("/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
    }))
}

async fn register(State(login): State<Arc<Login>>, body: Bytes) -> Response {
    let Ok(registration) = serde_json::from_slice::<Registration>(&body) else {
        return oauth_error(
            "invalid_client_metadata",
            "expected JSON with redirect_uris",
        );
    };
    let client = match client::checked(registration.client_name, registration.redirect_uris) {
        Ok(client) => client,
        Err(error) => return oauth_error("invalid_redirect_uri", &error),
    };
    let client_id = client::register(&login.signer, &client);
    let body = json!({
        "client_id": client_id,
        "client_name": client.name,
        "redirect_uris": client.redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    (StatusCode::CREATED, no_store(Json(body))).into_response()
}

async fn authorize(
    State(login): State<Arc<Login>>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let Some(query) = single(pairs) else {
        return page(
            StatusCode::BAD_REQUEST,
            "This login link repeats a parameter.",
        );
    };
    let (Some(client_id), Some(redirect_uri)) = (query.get("client_id"), query.get("redirect_uri"))
    else {
        return page(StatusCode::BAD_REQUEST, "This login link is incomplete.");
    };
    let client = match client_of(&login, client_id).await {
        Ok(client) => client,
        Err(error) => {
            return page(
                StatusCode::BAD_REQUEST,
                &format!("This app cannot log in: {error}."),
            );
        }
    };
    if !client::redirect_matches(&client, redirect_uri) {
        return page(
            StatusCode::BAD_REQUEST,
            "This app asked to be sent back somewhere it did not register.",
        );
    }

    let problem = if query.get("response_type").map(String::as_str) != Some("code") {
        Some("This app asked for a kind of login this server does not do.")
    } else if query.get("code_challenge_method").map(String::as_str) != Some("S256")
        || !query.get("code_challenge").is_some_and(|challenge| {
            challenge.len() == 43
                && challenge
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
    {
        Some("This app did not secure its login with PKCE.")
    } else if query
        .get("resource")
        .is_some_and(|resource| *resource != login.resource())
    {
        Some("This app asked to log in to another server.")
    } else if query
        .get("state")
        .is_some_and(|state| state.len() > MAX_STATE)
    {
        Some("This login link is too long.")
    } else {
        None
    };
    if let Some(problem) = problem {
        return page(StatusCode::BAD_REQUEST, problem);
    }

    let pending = Pending {
        client: hash(client_id),
        redirect_uri: redirect_uri.clone(),
        code_challenge: query["code_challenge"].clone(),
        state: query.get("state").cloned(),
    };
    let pending = login.signer.sign(Kind::Request, REQUEST_LIFETIME, &pending);
    consent(&login, &client, redirect_uri, &pending)
}

async fn client_of(login: &Arc<Login>, client_id: &str) -> Result<Client, String> {
    if client::document_url(client_id).is_none() {
        return client::resolve(&login.signer, client_id);
    }
    let Ok(_permit) = login.documents.try_acquire() else {
        return Err("too many logins at once; try again in a moment".to_string());
    };
    let (signer, client_id) = (login.signer.clone(), client_id.to_string());
    tokio::task::spawn_blocking(move || client::resolve(&signer, &client_id))
        .await
        .map_err(|_| "something went wrong".to_string())?
}

async fn start_login(
    State(login): State<Arc<Login>>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let same_origin = headers
        .get(header::ORIGIN)
        .and_then(|origin| origin.to_str().ok())
        .is_some_and(|origin| origin == login.base);
    if !same_origin {
        return page(
            StatusCode::FORBIDDEN,
            "Log in from the page that asked you to.",
        );
    }
    let Some(pending) = login.signer.verify::<Pending>(Kind::Request, &form.pending) else {
        return page(
            StatusCode::BAD_REQUEST,
            "This login took too long; start it again from your app.",
        );
    };

    let mut nonce = [0u8; 32];
    if getrandom::fill(&mut nonce).is_err() {
        return page(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.");
    }
    let cookie = URL_SAFE_NO_PAD.encode(nonce);
    let consented = Consented {
        pending,
        cookie: hash(&cookie),
    };
    let state = login
        .signer
        .sign(Kind::Consented, REQUEST_LIFETIME, &consented);

    let mut response = Redirect::to(&login.eve.authorize_url(&state)).into_response();
    let set_cookie = format!(
        "{LOGIN_COOKIE}={cookie}; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        REQUEST_LIFETIME.as_secs()
    );
    if let Ok(value) = HeaderValue::from_str(&set_cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

fn login_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == LOGIN_COOKIE)
        .map(|(_, value)| value)
}

async fn callback(
    State(login): State<Arc<Login>>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let query = single(pairs).unwrap_or_default();
    let consented = query
        .get("state")
        .and_then(|state| login.signer.verify::<Consented>(Kind::Consented, state))
        .filter(|consented| {
            login_cookie(&headers).is_some_and(|cookie| hash(cookie) == consented.cookie)
        });
    let Some(Consented { pending, .. }) = consented else {
        return page(
            StatusCode::BAD_REQUEST,
            "This login did not start in this browser, or took too long; start it again from your app.",
        );
    };

    let mut response = finish(&login, pending, query.get("code").cloned()).await;
    let clear = format!("{LOGIN_COOKIE}=; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
    if let Ok(value) = HeaderValue::from_str(&clear) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

async fn finish(login: &Arc<Login>, pending: Pending, code: Option<String>) -> Response {
    let back = |pairs: &[(&str, &str)]| {
        login.redirect_back(&pending.redirect_uri, pairs, pending.state.as_deref())
    };
    let Some(code) = code else {
        return back(&[("error", "access_denied")]);
    };

    let eve_login = Arc::clone(login);
    let character = match tokio::task::spawn_blocking(move || eve_login.eve.character(&code)).await
    {
        Ok(Ok(character)) => character,
        Ok(Err(error)) => {
            eprintln!("mcp-server: {error}");
            return back(&[("error", "server_error")]);
        }
        Err(_) => return back(&[("error", "server_error")]),
    };
    eprintln!(
        "mcp-server: {} ({}) logged in",
        character.name, character.id
    );

    let code = Code {
        grant: Grant {
            audience: login.resource(),
            client: pending.client.clone(),
            character,
            session_ends: now() + SESSION_LIFETIME.as_secs(),
        },
        redirect_uri: pending.redirect_uri.clone(),
        code_challenge: pending.code_challenge.clone(),
    };
    let code = login.signer.sign(Kind::Code, CODE_LIFETIME, &code);
    back(&[("code", &code)])
}

async fn token(
    State(login): State<Arc<Login>>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let Some(form) = single(pairs) else {
        return oauth_error("invalid_request", "a parameter is repeated");
    };
    let field = |name: &str| form.get(name).map(String::as_str);
    if field("resource").is_some_and(|resource| resource != login.resource()) {
        return oauth_error("invalid_target", "tokens are only for this server");
    }
    match field("grant_type") {
        Some("authorization_code") => {
            let Some(code) =
                field("code").and_then(|code| login.signer.verify::<Code>(Kind::Code, code))
            else {
                return oauth_error("invalid_grant", "the code is invalid or expired");
            };
            let challenge = hash(field("code_verifier").unwrap_or_default());
            if field("client_id").map(hash) != Some(code.grant.client.clone())
                || field("redirect_uri") != Some(code.redirect_uri.as_str())
                || challenge != code.code_challenge
            {
                return oauth_error("invalid_grant", "the code was not issued for this request");
            }
            login.tokens(&code.grant)
        }
        Some("refresh_token") => {
            let grant = field("refresh_token")
                .and_then(|token| login.signer.verify::<Grant>(Kind::Refresh, token))
                .filter(|grant| grant.audience == login.resource())
                .filter(|grant| field("client_id").is_none_or(|id| hash(id) == grant.client));
            match grant {
                Some(grant) => login.tokens(&grant),
                None => oauth_error("invalid_grant", "the refresh token is invalid or expired"),
            }
        }
        _ => oauth_error(
            "unsupported_grant_type",
            "only authorization_code and refresh_token",
        ),
    }
}

fn oauth_error(error: &str, description: &str) -> Response {
    let body = json!({"error": error, "error_description": description});
    (StatusCode::BAD_REQUEST, no_store(Json(body))).into_response()
}

fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn page(status: StatusCode, message: &str) -> Response {
    web::message(status, message)
}

fn consent(login: &Login, client: &Client, redirect_uri: &str, pending: &str) -> Response {
    let destination = Url::parse(redirect_uri)
        .ok()
        .map(|url| match url.origin() {
            url::Origin::Tuple(..) => url.origin().ascii_serialization(),
            url::Origin::Opaque(_) => {
                format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default())
            }
        })
        .unwrap_or_else(|| redirect_uri.to_string());
    let local = Url::parse(redirect_uri).is_ok_and(|url| is_local(&url));
    let warning = match local {
        true => {
            "<p class=\"warning\">That is an app on this computer. If you did not just start one, close this page.</p>"
        }
        false => "",
    };
    let body = format!(
        "<div class=\"card dialog\">\
         <h1><img src=\"/logo.png\" width=\"48\" height=\"48\" alt=\"\">Log in to EVEShip.fit</h1>\
         <p><strong>{name}</strong> wants to calculate ship fits for you with \
         <a href=\"https://eveship.fit\">EVEShip.fit</a>.</p>\
         <p>You log in with your EVE Online character. That only tells us who you are; \
         we ask for no scopes and read nothing from your account.</p>\
         <p>Afterwards you are sent back to <strong>{destination}</strong>.</p>{warning}\
         <form method=\"post\" action=\"{action}\">\
         <input type=\"hidden\" name=\"pending\" value=\"{pending}\">\
         <button type=\"submit\">Log in with EVE Online</button></form>\
         <p class=\"muted\" style=\"margin:16px 0 0\">Did you not start this? Close this page.</p>\
         </div>",
        name = web::escape(&client.name),
        destination = web::escape(&destination),
        action = web::escape(&login.url("/login")),
        pending = web::escape(pending),
    );
    web::page(StatusCode::OK, "Log in to EVEShip.fit", &body)
}

#[cfg(test)]
mod tests {
    use axum::Extension;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::any;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    const PUBLIC: &str = "http://localhost:8080";
    const REDIRECT: &str = "http://localhost:9999/callback";
    const VERIFIER: &str = "a-code-verifier-that-is-long-enough-for-pkce-s256";

    async fn fake_eve() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let token = move |headers: HeaderMap| async move {
            let credentials = base64::engine::general_purpose::STANDARD.encode("our-app:secret");
            let expected = format!("Basic {credentials}");
            let given = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok());
            if given != Some(expected.as_str()) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let claims = json!({
                "iss": address.ip().to_string(),
                "aud": ["our-app", "EVE Online"],
                "sub": "CHARACTER:EVE:2112625428",
                "name": "Some Pilot",
                "exp": 4_000_000_000u64,
            });
            Json(json!({"access_token": crate::eve::fake_token(&claims)})).into_response()
        };
        let app = Router::new().route("/v2/oauth/token", post(token));
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{address}")
    }

    async fn app() -> Router {
        let login = Arc::new(
            Login::new(
                PUBLIC,
                "a signing key that is long enough",
                "our-app".to_string(),
                "secret".to_string(),
                fake_eve().await,
            )
            .unwrap(),
        );
        Router::new()
            .route(
                "/mcp",
                any(|Extension(character): Extension<Character>| async move { character.name }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&login),
                require,
            ))
            .merge(routes(login))
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    fn get(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    fn form(uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        Request::post(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, PUBLIC)
            .body(Body::from(body))
            .unwrap()
    }

    fn query(location: &str, name: &str) -> Option<String> {
        Url::parse(location)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    async fn register(app: &Router) -> String {
        let request = Request::post("/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"redirect_uris": [REDIRECT], "client_name": "<b>Test</b>"}).to_string(),
            ))
            .unwrap();
        let (status, _, body) = send(app, request).await;
        assert_eq!(status, StatusCode::CREATED);
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["client_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn authorize_uri(client_id: &str, redirect_uri: &str) -> String {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()));
        let mut url = Url::parse("http://localhost/authorize").unwrap();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", "client-state")
            .append_pair("resource", &format!("{PUBLIC}/mcp"));
        format!("/authorize?{}", url.query().unwrap())
    }

    /// Log a character in, as a client would, up to the authorization code.
    fn pending_of(page: &str) -> String {
        page.split("name=\"pending\" value=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap()
            .to_string()
    }

    /// Consent to a login, as a browser would; the state for EVE and the login cookie.
    async fn consent_to(app: &Router, client_id: &str) -> (String, String) {
        let (status, _, page) = send(app, get(&authorize_uri(client_id, REDIRECT))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("&lt;b&gt;Test&lt;/b&gt;"), "{page}");
        assert!(page.contains("http://localhost:9999"), "{page}");
        let pending = pending_of(&page);

        let (status, headers, _) = send(app, form("/login", &[("pending", &pending)])).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let sso = headers[header::LOCATION].to_str().unwrap();
        assert_eq!(
            query(sso, "redirect_uri").as_deref(),
            Some("http://localhost:8080/callback")
        );
        let cookie = headers[header::SET_COOKIE].to_str().unwrap();
        assert!(
            cookie.contains("Secure; HttpOnly; SameSite=Lax; Path=/"),
            "{cookie}"
        );
        let cookie = cookie.split(';').next().unwrap().to_string();
        (query(sso, "state").unwrap(), cookie)
    }

    fn callback(state: &str, cookie: Option<&str>) -> Request<Body> {
        let mut request = Request::get(format!("/callback?code=eve-code&state={state}"));
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, format!("other=1; {cookie}"));
        }
        request.body(Body::empty()).unwrap()
    }

    /// Log a character in, as a client would, up to the authorization code.
    async fn code(app: &Router, client_id: &str) -> String {
        let (state, cookie) = consent_to(app, client_id).await;
        let (status, headers, _) = send(app, callback(&state, Some(&cookie))).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(
            headers[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        let back = headers[header::LOCATION].to_str().unwrap();
        assert!(back.starts_with(REDIRECT), "{back}");
        assert_eq!(query(back, "state").as_deref(), Some("client-state"));
        assert_eq!(query(back, "iss").as_deref(), Some(PUBLIC));
        query(back, "code").unwrap()
    }

    async fn tokens(app: &Router, fields: &[(&str, &str)]) -> (StatusCode, serde_json::Value) {
        let (status, _, body) = send(app, form("/token", fields)).await;
        (status, serde_json::from_str(&body).unwrap())
    }

    async fn call_mcp(app: &Router, token: Option<&str>) -> (StatusCode, HeaderMap, String) {
        let mut request = Request::post("/mcp");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        send(app, request.body(Body::empty()).unwrap()).await
    }

    #[tokio::test]
    async fn logs_a_character_in() {
        let app = app().await;
        let client_id = register(&app).await;
        let code = code(&app, &client_id).await;

        let exchange = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
            ("resource", "http://localhost:8080/mcp"),
        ];
        let mut wrong_verifier = exchange;
        wrong_verifier[4] = (
            "code_verifier",
            "another-verifier-that-is-long-enough-for-pkce",
        );
        let (status, body) = tokens(&app, &wrong_verifier).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );

        let (status, body) = tokens(&app, &exchange).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let access = body["access_token"].as_str().unwrap();
        let (status, _, name) = call_mcp(&app, Some(access)).await;
        assert_eq!((status, name.as_str()), (StatusCode::OK, "Some Pilot"));

        let refresh = body["refresh_token"].as_str().unwrap();
        let (status, body) = tokens(
            &app,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", &client_id),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, _, _) = call_mcp(&app, body["access_token"].as_str()).await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = tokens(
            &app,
            &[("grant_type", "refresh_token"), ("refresh_token", access)],
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );
    }

    #[tokio::test]
    async fn asks_for_a_token() {
        let app = app().await;
        for token in [None, Some("not-a-token")] {
            let (status, headers, _) = call_mcp(&app, token).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            let challenge = headers[header::WWW_AUTHENTICATE].to_str().unwrap();
            assert!(
                challenge.starts_with(
                    "Bearer resource_metadata=\"http://localhost:8080/.well-known/oauth-protected-resource/mcp\""
                ),
                "{challenge}"
            );
            assert_eq!(challenge.contains("invalid_token"), token.is_some());
        }

        let request = Request::post("/mcp")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .body(Body::empty())
            .unwrap();
        let (status, headers, _) = send(&app, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            !headers[header::WWW_AUTHENTICATE]
                .to_str()
                .unwrap()
                .contains("invalid_token")
        );

        let (_, _, body) = send(&app, get("/.well-known/oauth-protected-resource/mcp")).await;
        let metadata: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(metadata["resource"], "http://localhost:8080/mcp");
        assert_eq!(metadata["authorization_servers"][0], PUBLIC);
        let (_, _, body) = send(&app, get("/.well-known/oauth-authorization-server")).await;
        let metadata: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(metadata["issuer"], PUBLIC);
        assert_eq!(metadata["code_challenge_methods_supported"][0], "S256");
    }

    #[tokio::test]
    async fn refuses_strangers() {
        let app = app().await;
        let client_id = register(&app).await;

        let (status, _, _) = send(
            &app,
            get(&authorize_uri(
                &client_id,
                "http://localhost:6666/elsewhere",
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _) = send(&app, get(&authorize_uri("made-up", REDIRECT))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let without_pkce = authorize_uri(&client_id, REDIRECT)
            .replace("code_challenge_method=S256", "code_challenge_method=plain");
        let (status, headers, _) = send(&app, get(&without_pkce)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!headers.contains_key(header::LOCATION));
        let repeated = format!("{}&state=again", authorize_uri(&client_id, REDIRECT));
        let (status, _, _) = send(&app, get(&repeated)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (_, _, page) = send(&app, get(&authorize_uri(&client_id, REDIRECT))).await;
        let pending = pending_of(&page);
        let mut cross_site = form("/login", &[("pending", &pending)]);
        cross_site.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        let (status, _, _) = send(&app, cross_site).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Sending someone to EVE with a login they never agreed to here gets nowhere.
        let (status, _, _) = send(&app, callback(&pending, None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (state, cookie) = consent_to(&app, &client_id).await;
        let (status, _, _) = send(&app, callback(&state, None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _) = send(&app, callback(&state, Some("__Host-login=guessed"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _) = send(&app, callback(&state, Some(&cookie))).await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        let request = Request::post("/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"redirect_uris": ["http://evil.example/callback"]}).to_string(),
            ))
            .unwrap();
        let (status, _, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_redirect_uri"), "{body}");
        let request = Request::post("/register")
            .body(Body::from("not json"))
            .unwrap();
        let (status, _, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_client_metadata"), "{body}");
    }

    #[tokio::test]
    async fn accepts_bearer_in_any_case() {
        let app = app().await;
        let client_id = register(&app).await;
        let code = code(&app, &client_id).await;
        let (_, body) = tokens(
            &app,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("client_id", &client_id),
                ("redirect_uri", REDIRECT),
                ("code_verifier", VERIFIER),
            ],
        )
        .await;
        let request = Request::post("/mcp")
            .header(
                header::AUTHORIZATION,
                format!("bearer {}", body["access_token"].as_str().unwrap()),
            )
            .body(Body::empty())
            .unwrap();
        let (status, _, _) = send(&app, request).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn ends_a_session_after_its_time() {
        let login = Login::new(
            PUBLIC,
            "a signing key that is long enough",
            "our-app".to_string(),
            "secret".to_string(),
            "https://login.eveonline.com".to_string(),
        )
        .unwrap();
        let grant = |session_ends| Grant {
            audience: login.resource(),
            client: hash("client"),
            character: Character {
                id: 1,
                name: "Some Pilot".to_string(),
            },
            session_ends,
        };
        assert_eq!(login.tokens(&grant(now() + 60)).status(), StatusCode::OK);
        assert_eq!(
            login.tokens(&grant(now())).status(),
            StatusCode::BAD_REQUEST
        );
    }
}
