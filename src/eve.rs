//! Logging a character in through EVE Online's SSO.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use serde::{Deserialize, Serialize};
use url::Url;

pub const SSO_URL: &str = "https://login.eveonline.com";
const TIMEOUT: Duration = Duration::from_secs(10);

const JWT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// The EVE Online application this server logs characters in with.
pub struct Eve {
    pub client_id: String,
    pub client_secret: String,
    pub sso_url: String,
    pub callback: String,
}

/// A logged-in character.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Character {
    pub id: u64,
    pub name: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Audience,
    sub: String,
    name: String,
    exp: u64,
}

impl Eve {
    /// Where to send a browser to log in, carrying `state` back to the callback.
    pub fn authorize_url(&self, state: &str) -> String {
        let mut url = Url::parse(&format!("{}/v2/oauth/authorize", self.sso_url))
            .expect("the SSO URL is valid");
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &self.callback)
            .append_pair("client_id", &self.client_id)
            .append_pair("state", state);
        url.into()
    }

    /// The character an authorization code from the SSO logged in.
    pub fn character(&self, code: &str) -> Result<Character, String> {
        let credentials = STANDARD.encode(format!("{}:{}", self.client_id, self.client_secret));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        let body = agent
            .post(format!("{}/v2/oauth/token", self.sso_url))
            .header("Authorization", format!("Basic {credentials}"))
            .send_form([("grant_type", "authorization_code"), ("code", code)])
            .and_then(|mut response| response.body_mut().read_to_string())
            .map_err(|error| format!("EVE SSO: {error}"))?;
        let response: TokenResponse =
            serde_json::from_str(&body).map_err(|error| format!("EVE SSO: {error}"))?;
        self.read(&response.access_token)
    }

    /// The character in an access token the SSO just handed over, after checking it is for us.
    fn read(&self, token: &str) -> Result<Character, String> {
        let payload = token
            .split('.')
            .nth(1)
            .ok_or("EVE SSO: the token is no JWT")?;
        let claims: Claims = JWT
            .decode(payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or("EVE SSO: the token has no readable claims")?;

        let host = Url::parse(&self.sso_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default();
        let issuer = claims.iss.trim_end_matches('/');
        if issuer != format!("https://{host}") && issuer != host {
            return Err(format!("EVE SSO: unexpected issuer {}", claims.iss));
        }
        let audiences = match &claims.aud {
            Audience::One(audience) => std::slice::from_ref(audience),
            Audience::Many(audiences) => audiences.as_slice(),
        };
        if !audiences.contains(&self.client_id) {
            return Err("EVE SSO: the token is for another application".to_string());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        if claims.exp <= now {
            return Err("EVE SSO: the token has expired".to_string());
        }
        let id = claims
            .sub
            .strip_prefix("CHARACTER:EVE:")
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| format!("EVE SSO: unexpected subject {}", claims.sub))?;
        Ok(Character {
            id,
            name: claims.name,
        })
    }
}

/// An unsigned JWT with these claims, as the SSO would hand one over.
#[cfg(test)]
pub fn fake_token(claims: &serde_json::Value) -> String {
    let encode = |value: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    };
    format!(
        "{}.{}.signature",
        encode(&serde_json::json!({"alg": "RS256"})),
        encode(claims)
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn eve() -> Eve {
        Eve {
            client_id: "our-app".to_string(),
            client_secret: "secret".to_string(),
            sso_url: SSO_URL.to_string(),
            callback: "https://mcp.example/callback".to_string(),
        }
    }

    fn claims() -> serde_json::Value {
        json!({
            "iss": "https://login.eveonline.com",
            "aud": ["our-app", "EVE Online"],
            "sub": "CHARACTER:EVE:2112625428",
            "name": "Some Pilot",
            "exp": 4_000_000_000u64,
        })
    }

    #[test]
    fn reads_the_character() {
        assert_eq!(
            eve().read(&fake_token(&claims())),
            Ok(Character {
                id: 2112625428,
                name: "Some Pilot".to_string()
            })
        );
    }

    #[test]
    fn rejects_tokens_not_for_us() {
        for (field, value) in [
            ("iss", json!("https://evil.example")),
            ("aud", json!(["other-app", "EVE Online"])),
            ("sub", json!("CORPORATION:EVE:1")),
            ("exp", json!(1)),
        ] {
            let mut claims = claims();
            claims[field] = value;
            assert!(eve().read(&fake_token(&claims)).is_err(), "{field}");
        }
    }

    #[test]
    fn sends_the_browser_to_the_sso() {
        let url = Url::parse(&eve().authorize_url("a state")).unwrap();
        assert_eq!(url.path(), "/v2/oauth/authorize");
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert!(query.contains(&("redirect_uri".into(), "https://mcp.example/callback".into())));
        assert!(query.contains(&("state".into(), "a state".into())));
    }
}
