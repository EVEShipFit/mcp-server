//! Tokens that carry their own claims, signed so only this server can make them.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// What a token is for; a token of one kind is never accepted as another.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Client,
    Request,
    Consented,
    Code,
    Access,
    Refresh,
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    kind: Kind,
    expires: u64,
    claims: T,
}

/// Signs and checks tokens with one secret key.
#[derive(Clone)]
pub struct Signer {
    key: Vec<u8>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl Signer {
    pub fn new(key: &[u8]) -> Signer {
        Signer { key: key.to_vec() }
    }

    fn mac(&self) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC takes a key of any length")
    }

    /// A token of `kind` holding `claims`, valid for `lifetime`.
    pub fn sign(&self, kind: Kind, lifetime: Duration, claims: &impl Serialize) -> String {
        let envelope = Envelope {
            kind,
            expires: now() + lifetime.as_secs(),
            claims,
        };
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&envelope).expect("claims serialize"));
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{payload}.{signature}")
    }

    /// The claims of a token of `kind` this server signed and that has not expired.
    pub fn verify<T: DeserializeOwned>(&self, kind: Kind, token: &str) -> Option<T> {
        let (payload, signature) = token.split_once('.')?;
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.verify_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?)
            .ok()?;

        let envelope: Envelope<T> =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        (envelope.kind == kind && envelope.expires > now()).then_some(envelope.claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(60 * 60);

    #[test]
    fn verifies_its_own_tokens() {
        let signer = Signer::new(b"a key that is long enough to sign");
        let token = signer.sign(Kind::Access, HOUR, &"claims");
        assert_eq!(
            signer.verify::<String>(Kind::Access, &token).as_deref(),
            Some("claims")
        );
    }

    #[test]
    fn rejects_other_kinds_keys_and_edits() {
        let signer = Signer::new(b"a key that is long enough to sign");
        let token = signer.sign(Kind::Code, HOUR, &"claims");
        assert_eq!(signer.verify::<String>(Kind::Access, &token), None);
        assert_eq!(
            Signer::new(b"another key entirely, also long").verify::<String>(Kind::Code, &token),
            None
        );

        let (payload, signature) = token.split_once('.').unwrap();
        let other = signer.sign(Kind::Code, HOUR, &"other");
        let (other_payload, _) = other.split_once('.').unwrap();
        assert_eq!(
            signer.verify::<String>(Kind::Code, &format!("{other_payload}.{signature}")),
            None
        );
        assert_eq!(signer.verify::<String>(Kind::Code, payload), None);
    }

    #[test]
    fn rejects_expired_tokens() {
        let signer = Signer::new(b"a key that is long enough to sign");
        let token = signer.sign(Kind::Access, Duration::ZERO, &"claims");
        assert_eq!(signer.verify::<String>(Kind::Access, &token), None);
    }
}
