//! The MCP clients that log in: registered here, or described by a metadata document.

use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use url::Url;

use crate::token::{Kind, Signer};

const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(5);
const DOCUMENT_LIMIT: u64 = 16 * 1024;
const REGISTRATION_LIFETIME: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);
const MAX_NAME: usize = 100;
const MAX_REDIRECTS: usize = 10;
const MAX_REDIRECT_LENGTH: usize = 2048;
const BLOCKED_SCHEMES: [&str; 14] = [
    "javascript",
    "data",
    "file",
    "vbscript",
    "blob",
    "about",
    "ftp",
    "ws",
    "wss",
    "intent",
    "chrome",
    "jar",
    "view-source",
    "mailto",
];
const LOOPBACK: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// What a client is called, and where it may be sent back to.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub name: String,
    pub redirect_uris: Vec<String>,
}

#[derive(Deserialize)]
struct Document {
    client_id: String,
    client_name: Option<String>,
    redirect_uris: Vec<String>,
}

/// Whether a client may be sent back to this URI: HTTPS, loopback HTTP, or an app's own scheme.
pub fn valid_redirect(uri: &str) -> bool {
    if uri.len() > MAX_REDIRECT_LENGTH {
        return false;
    }
    let Ok(url) = Url::parse(uri) else {
        return false;
    };
    let scheme = url.scheme();
    if url.fragment().is_some()
        || !uri.starts_with(&format!("{scheme}://"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    match scheme {
        "https" => url.host().is_some(),
        "http" => url.host_str().is_some_and(|host| LOOPBACK.contains(&host)),
        _ => !BLOCKED_SCHEMES.contains(&scheme) && !scheme.starts_with("ms-"),
    }
}

fn is_loopback(url: &Url) -> bool {
    url.scheme() == "http" && url.host_str().is_some_and(|host| LOOPBACK.contains(&host))
}

/// Whether `given` is one of the client's redirect URIs; on loopback HTTP any port is.
pub fn redirect_matches(client: &Client, given: &str) -> bool {
    let Ok(given_url) = Url::parse(given) else {
        return false;
    };
    client.redirect_uris.iter().any(|registered| {
        if registered == given {
            return true;
        }
        let Ok(registered) = Url::parse(registered) else {
            return false;
        };
        is_loopback(&registered)
            && is_loopback(&given_url)
            && registered.host_str() == given_url.host_str()
            && registered.path() == given_url.path()
            && registered.query() == given_url.query()
    })
}

/// A client from its registration, checked and trimmed to sane sizes.
pub fn checked(name: Option<String>, redirect_uris: Vec<String>) -> Result<Client, String> {
    if redirect_uris.is_empty() || redirect_uris.len() > MAX_REDIRECTS {
        return Err(format!("between 1 and {MAX_REDIRECTS} redirect URIs"));
    }
    if !redirect_uris.iter().all(|uri| valid_redirect(uri)) {
        return Err(
            "redirect URIs have to be HTTPS, loopback HTTP, or an app's own scheme".to_string(),
        );
    }
    let name = name
        .map(|name| name.trim().chars().take(MAX_NAME).collect::<String>())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "An unnamed app".to_string());
    Ok(Client {
        name,
        redirect_uris,
    })
}

/// A client registered here, as its own client id.
pub fn register(signer: &Signer, client: &Client) -> String {
    signer.sign(Kind::Client, REGISTRATION_LIFETIME, client)
}

/// The URL of a client metadata document, if the client id is one.
pub fn document_url(client_id: &str) -> Option<Url> {
    let url = Url::parse(client_id).ok()?;
    let public_domain = matches!(url.host(), Some(url::Host::Domain(domain))
        if domain != "localhost" && !domain.ends_with(".localhost") && !domain.ends_with(".localhost."));
    let plain = url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.as_str() == client_id
        && !client_id.contains("/./")
        && !client_id.contains("/../");
    (url.scheme() == "https" && public_domain && plain && url.path() != "/").then_some(url)
}

/// The client behind a client id.
pub fn resolve(signer: &Signer, client_id: &str) -> Result<Client, String> {
    if let Some(url) = document_url(client_id) {
        return fetch_document(&url);
    }
    signer
        .verify(Kind::Client, client_id)
        .ok_or_else(|| "this app is not registered here".to_string())
}

/// Resolves only to addresses on the public internet.
#[derive(Debug, Default)]
struct PublicResolver(DefaultResolver);

impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addresses = self.0.resolve(uri, config, timeout)?;
        match addresses.iter().all(|address| is_public(address.ip())) {
            true => Ok(addresses),
            false => Err(ureq::Error::HostNotFound),
        }
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (18..20).contains(&b)))
        }
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => is_public(IpAddr::V4(ip)),
            None => {
                let first = ip.segments()[0];
                !(ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || (first & 0xfe00) == 0xfc00
                    || (first & 0xffc0) == 0xfe80)
            }
        },
    }
}

fn fetch_document(url: &Url) -> Result<Client, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(DOCUMENT_TIMEOUT))
        .max_redirects(0)
        .build();
    let agent = ureq::Agent::with_parts(
        config,
        DefaultConnector::default(),
        PublicResolver::default(),
    );
    let body = agent
        .get(url.as_str())
        .header("Accept", "application/json")
        .call()
        .and_then(|mut response| {
            response
                .body_mut()
                .with_config()
                .limit(DOCUMENT_LIMIT)
                .read_to_string()
        })
        .map_err(|_| "its client metadata document could not be read".to_string())?;
    read_document(url, &body)
}

fn read_document(url: &Url, body: &str) -> Result<Client, String> {
    let document: Document = serde_json::from_str(body)
        .map_err(|_| "its client metadata document is invalid".to_string())?;
    if document.client_id != url.as_str() {
        return Err("its client metadata document names another client id".to_string());
    }
    let mut client = checked(document.client_name, document.redirect_uris)
        .map_err(|error| format!("its client metadata document is invalid: {error}"))?;
    client.name = format!("{} ({})", client.name, url.host_str().unwrap_or_default());
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_safe_redirects_only() {
        for uri in [
            "https://client.example/callback",
            "http://localhost:33418/callback",
            "http://127.0.0.1:5173/",
            "cursor://anysphere.cursor-mcp/oauth/callback",
        ] {
            assert!(valid_redirect(uri), "{uri}");
        }
        for uri in [
            "http://client.example/callback",
            "javascript:alert(1)",
            "data:text/html,hi",
            "intent://scan/#Intent;end",
            "ms-settings://display",
            "mailto:someone@example.com",
            "custom:no-slashes",
            "https://user@client.example/callback",
            "https://client.example/callback#fragment",
            "not a url",
        ] {
            assert!(!valid_redirect(uri), "{uri}");
        }
    }

    #[test]
    fn matches_loopback_redirects_on_any_port() {
        let client = Client {
            name: "Test".to_string(),
            redirect_uris: vec![
                "http://127.0.0.1:3000/callback".to_string(),
                "https://client.example/callback".to_string(),
            ],
        };
        assert!(redirect_matches(&client, "http://127.0.0.1:3000/callback"));
        assert!(redirect_matches(&client, "http://127.0.0.1:41234/callback"));
        assert!(redirect_matches(&client, "https://client.example/callback"));
        assert!(!redirect_matches(&client, "http://127.0.0.1:41234/other"));
        assert!(!redirect_matches(&client, "http://localhost:3000/callback"));
        assert!(!redirect_matches(
            &client,
            "https://client.example:8443/callback"
        ));
    }

    #[test]
    fn checks_registrations() {
        let uri = || "https://client.example/cb".to_string();
        let client = checked(Some("x".repeat(500)), vec![uri()]).unwrap();
        assert_eq!(client.name.len(), MAX_NAME);
        assert!(checked(None, vec![]).is_err());
        assert!(checked(None, vec![uri(); MAX_REDIRECTS + 1]).is_err());
        let long = format!("https://client.example/{}", "x".repeat(MAX_REDIRECT_LENGTH));
        assert!(checked(None, vec![long]).is_err());
    }

    #[test]
    fn resolves_registered_clients() {
        let signer = Signer::new(b"a key that is long enough to sign");
        let client = Client {
            name: "Claude Code".to_string(),
            redirect_uris: vec!["http://localhost:33418/callback".to_string()],
        };
        let client_id = register(&signer, &client);
        assert_eq!(resolve(&signer, &client_id), Ok(client));
        assert!(resolve(&signer, "made-up").is_err());
    }

    #[test]
    fn reads_metadata_documents() {
        let url = Url::parse("https://client.example/oauth/client.json").unwrap();
        let body = r#"{"client_id": "https://client.example/oauth/client.json", "client_name": "Example", "redirect_uris": ["http://127.0.0.1:3000/callback"]}"#;
        assert_eq!(
            read_document(&url, body),
            Ok(Client {
                name: "Example (client.example)".to_string(),
                redirect_uris: vec!["http://127.0.0.1:3000/callback".to_string()],
            })
        );

        let other = body.replace(
            "https://client.example/oauth/client.json",
            "https://evil.example/client.json",
        );
        assert!(read_document(&url, &other).is_err());
        let unsafe_redirect = body.replace("http://127.0.0.1:3000/callback", "javascript:alert(1)");
        assert!(read_document(&url, &unsafe_redirect).is_err());
    }

    #[test]
    fn tells_document_urls_apart() {
        assert!(document_url("https://client.example/client.json").is_some());
        for client_id in [
            "https://client.example/",
            "http://client.example/client.json",
            "https://127.0.0.1/client.json",
            "https://localhost/client.json",
            "https://app.localhost/client.json",
            "https://client.example:6379/client.json",
            "https://user@client.example/client.json",
            "https://client.example/client.json?x=1",
            "https://client.example/a/../client.json",
            "eyJraW5kIjoiY2xpZW50In0.c2ln",
        ] {
            assert!(document_url(client_id).is_none(), "{client_id}");
        }
    }

    #[test]
    fn keeps_to_the_public_internet() {
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
    }
}
