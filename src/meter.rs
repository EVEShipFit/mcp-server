//! The CPU time each tool call takes, charged to the character who made it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::time::Duration;

use axum::http::request::Parts;
use nix::time::{ClockId, clock_gettime};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};

use crate::eve::Character;
use crate::limit::Limits;

const SLOW: Duration = Duration::from_secs(1);
const LOGGED_ARGUMENTS: usize = 4096;

/// The CPU time this thread has used.
fn thread_cpu() -> Option<Duration> {
    clock_gettime(ClockId::CLOCK_THREAD_CPUTIME_ID)
        .ok()
        .map(Duration::from)
}

/// A future that adds up the CPU time spent polling it, and hands each poll's share to `charge`.
struct CpuTimed<F, C> {
    inner: Pin<Box<F>>,
    spent: Duration,
    charge: C,
}

/// Counts one poll's CPU time when dropped, so a poll that panics still counts.
struct Poll<'a, C: Fn(Duration)> {
    start: Option<Duration>,
    spent: &'a mut Duration,
    charge: &'a C,
}

impl<C: Fn(Duration)> Drop for Poll<'_, C> {
    fn drop(&mut self) {
        if let (Some(start), Some(end)) = (self.start, thread_cpu()) {
            let spent = end.saturating_sub(start);
            *self.spent += spent;
            (self.charge)(spent);
        }
    }
}

impl<F: Future, C: Fn(Duration) + Unpin> Future for CpuTimed<F, C> {
    type Output = (F::Output, Duration);

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let poll = {
            let _poll = Poll {
                start: thread_cpu(),
                spent: &mut this.spent,
                charge: &this.charge,
            };
            this.inner.as_mut().poll(context)
        };
        poll.map(|output| (output, this.spent))
    }
}

fn cpu_timed<F: Future, C: Fn(Duration) + Unpin>(future: F, charge: C) -> CpuTimed<F, C> {
    CpuTimed {
        inner: Box::pin(future),
        spent: Duration::ZERO,
        charge,
    }
}

/// An MCP server whose tool calls are charged to the character's CPU limit, and logged when slow.
#[derive(Clone)]
pub struct Metered<H> {
    inner: H,
    limits: Arc<Limits>,
}

impl<H> Metered<H> {
    pub fn new(inner: H, limits: Arc<Limits>) -> Metered<H> {
        Metered { inner, limits }
    }
}

fn character(context: &RequestContext<RoleServer>) -> Option<Character> {
    context
        .extensions
        .get::<Parts>()?
        .extensions
        .get::<Character>()
        .cloned()
}

impl<H: ServerHandler> ServerHandler for Metered<H> {
    fn get_info(&self) -> ServerConfig {
        self.inner.get_info()
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.inner.get_tool(name)
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.inner.list_tools(request, context).await
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.inner.list_resources(request, context).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.inner.read_resource(request, context).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let character = character(&context);
        let name = request.name.clone();
        let arguments = request.arguments.clone();

        let limits = &self.limits;
        let id = character.as_ref().map(|character| character.id);
        let charge = move |spent| {
            if let Some(id) = id {
                limits.spend_cpu(id, spent);
            }
        };
        let (result, cpu) = cpu_timed(self.inner.call_tool(request, context), charge).await;

        if cpu >= SLOW {
            let who = character.map_or_else(
                || "someone".to_string(),
                |character| format!("{} ({})", character.name, character.id),
            );
            let mut arguments = serde_json::to_string(&arguments).unwrap_or_default();
            if arguments.len() > LOGGED_ARGUMENTS {
                let end = arguments.floor_char_boundary(LOGGED_ARGUMENTS);
                arguments.truncate(end);
                arguments.push('…');
            }
            eprintln!(
                "mcp-server: slow call: {name} took {} ms of CPU for {who}: {arguments}",
                cpu.as_millis()
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spin(cpu: Duration) {
        let start = thread_cpu().unwrap();
        while thread_cpu().unwrap() - start < cpu {
            std::hint::black_box(0u64);
        }
    }

    #[tokio::test]
    async fn counts_the_cpu_time_of_polls() {
        let charged = std::sync::Mutex::new(Duration::ZERO);
        let charge = |spent| *charged.lock().unwrap() += spent;
        let (_, spent) = cpu_timed(
            async {
                spin(Duration::from_millis(50));
                tokio::time::sleep(Duration::from_millis(100)).await;
                spin(Duration::from_millis(50));
            },
            charge,
        )
        .await;
        assert_eq!(*charged.lock().unwrap(), spent);
        assert!(spent >= Duration::from_millis(80), "{spent:?}");
        assert!(spent < Duration::from_millis(140), "{spent:?}");
    }

    #[derive(Clone)]
    struct Slow;

    impl ServerHandler for Slow {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            spin(Duration::from_millis(20));
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(rmcp::model::CallToolResult::success(vec![]).into())
        }
    }

    #[tokio::test]
    async fn holds_a_slot_until_the_tool_is_done() {
        use axum::Router;
        use axum::body::Body;
        use axum::extract::Request;
        use axum::http::{StatusCode, header};
        use axum::middleware::{Next, from_fn, from_fn_with_state};
        use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService,
        };
        use tower::ServiceExt;

        let limits = Arc::new(Limits::new(60, 10));
        let metered = Arc::clone(&limits);
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true);
        let mcp = StreamableHttpService::new(
            move || Ok(Metered::new(Slow, Arc::clone(&metered))),
            Arc::new(NeverSessionManager::default()),
            config,
        );
        let app = Router::new()
            .nest_service("/mcp", mcp)
            .layer(from_fn_with_state(Arc::clone(&limits), crate::limit::limit))
            .layer(from_fn(|mut request: Request, next: Next| async move {
                request.extensions_mut().insert(Character {
                    id: 1,
                    name: "Some Pilot".to_string(),
                });
                next.run(request).await
            }));
        let call = || {
            let request = Request::post("http://localhost/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header("MCP-Protocol-Version", "2025-06-18")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"slow","arguments":{}}}"#,
                ))
                .unwrap();
            app.clone().oneshot(request)
        };

        let (first, second, third) = tokio::join!(call(), call(), call());
        let mut statuses = [first, second, third].map(|response| response.unwrap().status());
        statuses.sort();
        assert_eq!(
            statuses,
            [
                StatusCode::OK,
                StatusCode::OK,
                StatusCode::TOO_MANY_REQUESTS
            ]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(call().await.unwrap().status(), StatusCode::OK);
    }
}
