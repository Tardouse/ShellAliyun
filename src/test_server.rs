use http_body_util::{BodyExt, Full};
use hyper::{
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::{convert::Infallible, sync::Arc};
use tokio::{net::TcpListener, task::JoinHandle};

pub type Reply = Response<Full<Bytes>>;

pub struct TestServer {
    pub origin: String,
    task: JoinHandle<()>,
}

impl TestServer {
    pub async fn start(
        handler: impl Fn(Request<Bytes>, &str) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let local_origin = origin.clone();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let handler = Arc::clone(&handler);
                let origin = local_origin.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let handler = Arc::clone(&handler);
                        let origin = origin.clone();
                        async move {
                            let (parts, body) = request.into_parts();
                            let bytes = body.collect().await.unwrap().to_bytes();
                            Ok::<_, Infallible>(handler(Request::from_parts(parts, bytes), &origin))
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        Self { origin, task }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn json_reply(value: Value) -> Reply {
    Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(value.to_string())))
        .unwrap()
}

pub fn request_json(request: &Request<Bytes>) -> Value {
    serde_json::from_slice(request.body()).unwrap()
}
