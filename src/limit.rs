//! How often each character may call the tools, and how much CPU time they may use.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::eve::Character;

const PRUNE_ABOVE: usize = 10_000;
const AT_ONCE: usize = 2;

/// A bucket per character holding up to a minute's worth, refilled at a steady rate.
struct Buckets {
    per_minute: f64,
    levels: Mutex<HashMap<u64, (f64, Instant)>>,
}

impl Buckets {
    fn new(per_minute: f64) -> Buckets {
        Buckets {
            per_minute,
            levels: Mutex::new(HashMap::new()),
        }
    }

    fn per_second(&self) -> f64 {
        self.per_minute / 60.0
    }

    /// The character's level after refilling until `now`, for `change` to adjust.
    fn with_level<T>(&self, character: u64, now: Instant, change: impl FnOnce(&mut f64) -> T) -> T {
        let (capacity, per_second) = (self.per_minute, self.per_second());
        let refilled = |level: f64, since: Instant| {
            (level + now.saturating_duration_since(since).as_secs_f64() * per_second).min(capacity)
        };
        let mut levels = self
            .levels
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if levels.len() > PRUNE_ABOVE {
            levels.retain(|_, (level, since)| refilled(*level, *since) < capacity);
        }
        let (level, since) = levels.entry(character).or_insert((capacity, now));
        *level = refilled(*level, *since);
        *since = now;
        change(level)
    }

    fn wait_for(&self, missing: f64) -> Duration {
        Duration::from_secs_f64(missing / self.per_second())
    }

    /// Take `amount` when the bucket holds that much; otherwise how long until it does.
    fn take(&self, character: u64, amount: f64, now: Instant) -> Result<(), Duration> {
        let missing = self.with_level(character, now, |level| match *level >= amount {
            true => {
                *level -= amount;
                0.0
            }
            false => amount - *level,
        });
        match missing > 0.0 {
            true => Err(self.wait_for(missing)),
            false => Ok(()),
        }
    }

    /// Whether the bucket holds anything; otherwise how long until it does.
    fn check(&self, character: u64, now: Instant) -> Result<(), Duration> {
        let level = self.with_level(character, now, |level| *level);
        match level > 0.0 {
            true => Ok(()),
            false => Err(self.wait_for(-level + f64::EPSILON)),
        }
    }

    /// Take `amount`, going below empty if need be.
    fn spend(&self, character: u64, amount: f64, now: Instant) {
        self.with_level(character, now, |level| *level -= amount);
    }
}

/// The request and CPU limits of every character.
pub struct Limits {
    requests: Buckets,
    cpu: Buckets,
    running: Mutex<HashMap<u64, usize>>,
}

/// One of a character's requests that is still running; dropping it ends it.
#[derive(Clone)]
pub struct Running {
    _slot: Arc<Slot>,
}

struct Slot {
    limits: Arc<Limits>,
    character: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut running = self
            .limits
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(count) = running.get_mut(&self.character) {
            *count -= 1;
            if *count == 0 {
                running.remove(&self.character);
            }
        }
    }
}

impl Limits {
    /// `requests` a minute, and `cpu_seconds` of CPU time a minute.
    pub fn new(requests: u32, cpu_seconds: u32) -> Limits {
        Limits {
            requests: Buckets::new(f64::from(requests)),
            cpu: Buckets::new(f64::from(cpu_seconds)),
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Start one of the character's requests, unless it already runs as many as it may.
    fn start(self: &Arc<Self>, character: u64) -> Option<Running> {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let count = running.entry(character).or_default();
        if *count >= AT_ONCE {
            return None;
        }
        *count += 1;
        Some(Running {
            _slot: Arc::new(Slot {
                limits: Arc::clone(self),
                character,
            }),
        })
    }

    /// Let a request in, or say how long to wait and why.
    fn admit(&self, character: u64, now: Instant) -> Result<(), (Duration, String)> {
        self.cpu.check(character, now).map_err(|wait| {
            let message = format!(
                "used more than {} CPU seconds a minute",
                self.cpu.per_minute
            );
            (wait, message)
        })?;
        self.requests.take(character, 1.0, now).map_err(|wait| {
            let message = format!("{} calls a minute at most", self.requests.per_minute);
            (wait, message)
        })
    }

    /// Charge the CPU time a call took.
    pub fn spend_cpu(&self, character: u64, cpu: Duration) {
        self.cpu.spend(character, cpu.as_secs_f64(), Instant::now());
    }
}

/// Answer 429 to a character over its request or CPU limit, or running too many requests at once.
///
/// The request carries its [`Running`] into the tool call, which holds it until done.
pub async fn limit(
    State(limits): State<Arc<Limits>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(character) = request
        .extensions()
        .get::<Character>()
        .map(|character| character.id)
    else {
        return next.run(request).await;
    };
    let admitted = limits.admit(character, Instant::now()).and_then(|()| {
        limits.start(character).ok_or_else(|| {
            let reason = format!("{AT_ONCE} calls at once at most");
            (Duration::from_secs(1), reason)
        })
    });
    match admitted {
        Ok(running) => {
            request.extensions_mut().insert(running);
            next.run(request).await
        }
        Err((wait, reason)) => {
            let seconds = wait.as_secs_f64().ceil().max(1.0) as u64;
            let message = format!("{reason}; try again in {seconds} seconds");
            let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": message}});
            let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_a_burst_then_the_rate() {
        let buckets = Buckets::new(60.0);
        let start = Instant::now();
        for _ in 0..60 {
            assert_eq!(buckets.take(1, 1.0, start), Ok(()));
        }
        let wait = buckets.take(1, 1.0, start).unwrap_err();
        assert!((0.9..=1.0).contains(&wait.as_secs_f64()), "{wait:?}");

        assert_eq!(buckets.take(2, 1.0, start), Ok(()));
        assert_eq!(buckets.take(1, 1.0, start + Duration::from_secs(1)), Ok(()));
        assert!(
            buckets
                .take(1, 1.0, start + Duration::from_secs(1))
                .is_err()
        );
    }

    #[test]
    fn refills_no_further_than_full() {
        let buckets = Buckets::new(10.0);
        let start = Instant::now();
        assert_eq!(buckets.take(1, 1.0, start), Ok(()));
        let later = start + Duration::from_secs(3600);
        for _ in 0..10 {
            assert_eq!(buckets.take(1, 1.0, later), Ok(()));
        }
        assert!(buckets.take(1, 1.0, later).is_err());
    }

    #[test]
    fn lets_spending_go_below_empty() {
        let buckets = Buckets::new(10.0);
        let start = Instant::now();
        assert_eq!(buckets.check(1, start), Ok(()));
        buckets.spend(1, 25.0, start);
        let wait = buckets.check(1, start).unwrap_err();
        assert!((89.9..=90.1).contains(&wait.as_secs_f64()), "{wait:?}");
        assert!(buckets.check(1, start + Duration::from_secs(89)).is_err());
        assert_eq!(buckets.check(1, start + Duration::from_secs(91)), Ok(()));
    }

    #[test]
    fn forgets_characters_that_are_back_to_full() {
        let buckets = Buckets::new(60.0);
        let start = Instant::now();
        for character in 0..=PRUNE_ABOVE as u64 {
            buckets.take(character, 1.0, start).unwrap();
        }
        buckets
            .take(u64::MAX, 1.0, start + Duration::from_secs(60))
            .unwrap();
        assert_eq!(buckets.levels.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn answers_too_many_requests() {
        use axum::Router;
        use axum::body::Body;
        use axum::middleware::from_fn_with_state;
        use axum::routing::post;
        use tower::ServiceExt;

        let limits = Arc::new(Limits::new(2, 10));
        let app = Router::new()
            .route("/mcp", post(|| async { "ok" }))
            .layer(from_fn_with_state(Arc::clone(&limits), limit))
            .layer(axum::middleware::from_fn(
                |mut request: Request, next: Next| async move {
                    request.extensions_mut().insert(Character {
                        id: 1,
                        name: "Some Pilot".to_string(),
                    });
                    next.run(request).await
                },
            ));
        let call = || async {
            let request = Request::post("/mcp").body(Body::empty()).unwrap();
            app.clone().oneshot(request).await.unwrap()
        };
        let message = |response: Response| async move {
            let body = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            body["error"]["message"].as_str().unwrap().to_string()
        };

        assert_eq!(call().await.status(), StatusCode::OK);
        assert_eq!(call().await.status(), StatusCode::OK);
        let refused = call().await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(refused.headers()[header::RETRY_AFTER], "30");
        assert_eq!(
            message(refused).await,
            "2 calls a minute at most; try again in 30 seconds"
        );

        limits.spend_cpu(1, Duration::from_secs(15));
        let refused = call().await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(refused.headers()[header::RETRY_AFTER], "30");
        assert_eq!(
            message(refused).await,
            "used more than 10 CPU seconds a minute; try again in 30 seconds"
        );
    }
}
