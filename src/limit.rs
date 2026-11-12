//! How often each character may call the tools.

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

/// A bucket of requests per character, refilled at a steady rate.
pub struct Limits {
    per_minute: u32,
    buckets: Mutex<HashMap<u64, (f64, Instant)>>,
}

impl Limits {
    pub fn new(per_minute: u32) -> Limits {
        Limits {
            per_minute,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn per_second(&self) -> f64 {
        f64::from(self.per_minute) / 60.0
    }

    /// Take one request from the character's bucket; when empty, how long until there is one.
    fn take(&self, character: u64, now: Instant) -> Result<(), Duration> {
        let capacity = f64::from(self.per_minute);
        let per_second = self.per_second();
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if buckets.len() > PRUNE_ABOVE {
            buckets.retain(|_, (tokens, since)| {
                *tokens + now.duration_since(*since).as_secs_f64() * per_second < capacity
            });
        }

        let (tokens, since) = buckets.entry(character).or_insert((capacity, now));
        *tokens = (*tokens + now.duration_since(*since).as_secs_f64() * per_second).min(capacity);
        *since = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            return Ok(());
        }
        Err(Duration::from_secs_f64((1.0 - *tokens) / per_second))
    }
}

/// Answer 429 to a character that calls more often than the limit.
pub async fn limit(State(limits): State<Arc<Limits>>, request: Request, next: Next) -> Response {
    let Some(character) = request.extensions().get::<Character>() else {
        return next.run(request).await;
    };
    match limits.take(character.id, Instant::now()) {
        Ok(()) => next.run(request).await,
        Err(wait) => {
            let seconds = wait.as_secs_f64().ceil().max(1.0) as u64;
            let message = format!(
                "{} calls a minute at most; try again in {seconds} seconds",
                limits.per_minute
            );
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
        let limits = Limits::new(60);
        let start = Instant::now();
        for _ in 0..60 {
            assert_eq!(limits.take(1, start), Ok(()));
        }
        let wait = limits.take(1, start).unwrap_err();
        assert!((0.9..=1.0).contains(&wait.as_secs_f64()), "{wait:?}");

        assert_eq!(limits.take(2, start), Ok(()));
        assert_eq!(limits.take(1, start + Duration::from_secs(1)), Ok(()));
        assert!(limits.take(1, start + Duration::from_secs(1)).is_err());
    }

    #[test]
    fn refills_no_further_than_full() {
        let limits = Limits::new(10);
        let start = Instant::now();
        assert_eq!(limits.take(1, start), Ok(()));
        let later = start + Duration::from_secs(3600);
        for _ in 0..10 {
            assert_eq!(limits.take(1, later), Ok(()));
        }
        assert!(limits.take(1, later).is_err());
    }

    #[test]
    fn forgets_characters_that_are_back_to_full() {
        let limits = Limits::new(60);
        let start = Instant::now();
        for character in 0..=PRUNE_ABOVE as u64 {
            limits.take(character, start).unwrap();
        }
        limits
            .take(u64::MAX, start + Duration::from_secs(60))
            .unwrap();
        assert_eq!(limits.buckets.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn answers_too_many_requests() {
        use axum::Router;
        use axum::body::Body;
        use axum::middleware::from_fn_with_state;
        use axum::routing::post;
        use tower::ServiceExt;

        let app = Router::new()
            .route("/mcp", post(|| async { "ok" }))
            .layer(from_fn_with_state(Arc::new(Limits::new(2)), limit))
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
        assert_eq!(call().await.status(), StatusCode::OK);
        assert_eq!(call().await.status(), StatusCode::OK);
        let refused = call().await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(refused.headers()[header::RETRY_AFTER], "30");
        let body = http_body_util::BodyExt::collect(refused.into_body())
            .await
            .unwrap()
            .to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["error"]["message"],
            "2 calls a minute at most; try again in 30 seconds"
        );
    }
}
