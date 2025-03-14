#![feature(gen_blocks)]
#![feature(async_iterator)]
#![feature(async_for_loop)]

mod adapter;
mod cli;
mod cot;
mod ext_types;
mod sse;
mod truncate;

use std::fmt::Debug;
use std::io;
use std::sync::Arc;

use async_openai::types::{CreateChatCompletionRequest, CreateCompletionRequest};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::Uri;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response, Sse};
use axum::{
    Json, Router,
    http::{HeaderMap, Method, StatusCode, header},
    routing::post,
};
use clap::Parser;
use educe::Educe;
use futures_util::{FutureExt, TryStreamExt, select};
use reqwest::{Client, Url};
use serde::Serialize;
use tiktoken_rs::{CoreBPE, o200k_base};
use tokio::net::TcpListener;
use tokio::signal::unix::{self, SignalKind};
use tower_http::cors::{AllowHeaders, AllowPrivateNetwork, Any, CorsLayer};
use tracing::level_filters::LevelFilter;
use tracing::{error, info, instrument};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Registry, fmt};

use crate::adapter::StreamAsyncIterAdapter;
use crate::cli::{Cli, CotParser};
use crate::cot::deepseek;
use crate::sse::send_stream_request;
use crate::truncate::{MessageType, truncate_messages};

#[derive(Educe)]
#[educe(Debug)]
struct ServerState {
    backend: Url,
    client: Client,
    input_max_token: Option<usize>,
    #[educe(Debug(ignore))]
    bpe: CoreBPE,
    cot_parser: Option<CotParser>,
}

#[instrument(level = "debug", ret, err(Debug))]
async fn handle_completion(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(mut payload): Json<CreateCompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(max_token) = state.input_max_token {
        truncate_messages(
            &state.bpe,
            MessageType::Prompt(&mut payload.prompt),
            max_token,
        );
    }

    forward_request(
        state,
        "/v1/completions",
        Method::POST,
        headers,
        payload.stream.unwrap_or_default(),
        payload,
    )
    .await
}

#[instrument(level = "debug", ret, err(Debug))]
async fn handle_chat(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(mut payload): Json<CreateChatCompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(max_token) = state.input_max_token {
        truncate_messages(
            &state.bpe,
            MessageType::Chat(&mut payload.messages),
            max_token,
        );
    }

    forward_request(
        state,
        "/v1/chat/completions",
        Method::POST,
        headers,
        payload.stream.unwrap_or_default(),
        payload,
    )
    .await
}

#[instrument(level = "debug", ret, err(Debug))]
async fn forward_request<T: Serialize + Debug + 'static>(
    state: State<Arc<ServerState>>,
    path: &str,
    method: Method,
    mut headers: HeaderMap,
    streaming: bool,
    body: T,
) -> Result<Response, (StatusCode, String)> {
    headers = retain_headers(headers);

    let url = state
        .backend
        .join(path)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;

    if streaming {
        match state.cot_parser {
            Some(CotParser::Deepseek) => {
                return match send_stream_request(state.client.clone(), url, body).await {
                    Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),

                    Ok(sse_stream_response) => {
                        let chunks = deepseek::extract_cot(sse_stream_response);
                        let adapter = StreamAsyncIterAdapter(chunks)
                            .and_then(async |chunk| Ok(Event::default().json_data(chunk)?))
                            .inspect_err(|err| {
                                error!(%err, "SSE stream error occurred");
                            });

                        let sse = Sse::new(adapter);

                        Ok(sse.into_response())
                    }
                };
            }

            None => {}
        }
    }

    match state
        .client
        .request(method, url)
        .headers(headers)
        .json(&body)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.bytes_stream();
            let body = Body::from_stream(body);

            let mut builder = Response::builder().status(status);

            for (k, v) in headers {
                if let Some(k) = k {
                    builder = builder.header(k, v);
                }
            }

            builder
                .body(body)
                .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
        }

        Err(err) => Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from(err.to_string()))
            .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

fn retain_headers(headers: HeaderMap) -> HeaderMap {
    headers
        .into_iter()
        .filter_map(|(k, v)| match k {
            Some(header::AUTHORIZATION) => Some((header::AUTHORIZATION, v)),
            _ => None,
        })
        .collect::<HeaderMap>()
}

#[instrument(err(Debug), skip(body))]
async fn proxy_handler(
    state: State<Arc<ServerState>>,
    method: Method,
    req_uri: Uri,
    mut headers: HeaderMap,
    body: Body,
) -> Result<Response, (StatusCode, String)> {
    headers = retain_headers(headers);

    let mut url = state.backend.clone();
    url.set_path(req_uri.path());

    let response = match state
        .client
        .request(method, url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await
    {
        Err(err) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(err.to_string()))
                .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()));
        }

        Ok(resp) => resp,
    };

    let status = response.status();
    let headers = response.headers().clone();
    let body = Body::from_stream(response.bytes_stream());
    let mut builder = Response::builder().status(status);

    for (k, v) in headers {
        if let Some(k) = k {
            builder = builder.header(k, v);
        }
    }

    builder
        .body(body)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_log(cli.debug);

    info!("starting openai limiter");

    let bpe = o200k_base()?;

    let cors = CorsLayer::new()
        // allow `GET` and `POST` when accessing the resource
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(AllowHeaders::any())
        .allow_private_network(AllowPrivateNetwork::yes())
        // allow requests from any origin
        .allow_origin(Any);

    let app = Router::new()
        .route(
            "/v1/completions",
            post(handle_completion).fallback(proxy_handler),
        )
        .route(
            "/v1/chat/completions",
            post(handle_chat).fallback(proxy_handler),
        )
        .fallback(proxy_handler)
        .layer(DefaultBodyLimit::disable())
        .layer(cors)
        .with_state(Arc::new(ServerState {
            backend: cli.backend.parse()?,
            client: Default::default(),
            input_max_token: cli.input_max_token,
            bpe,
            cot_parser: cli.cot_parser,
        }));

    let listener = TcpListener::bind(cli.listen).await?;

    select! {
        res = axum::serve(listener, app).into_future().fuse() => res?,
        _ = signal_stop().fuse() => {}
    }

    Ok(())
}

async fn signal_stop() {
    let mut term = unix::signal(SignalKind::terminate()).unwrap();
    let mut interrupt = unix::signal(SignalKind::interrupt()).unwrap();

    select! {
        _ = term.recv().fuse() => {}
        _ = interrupt.recv().fuse() => {}
    }
}

fn init_log(debug: bool) {
    let layer = fmt::layer()
        .pretty()
        .with_target(true)
        .with_writer(io::stderr);

    let level = if debug {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };

    let targets = Targets::new()
        .with_default(LevelFilter::DEBUG)
        .with_target("hickory_resolver", LevelFilter::OFF);

    Registry::default()
        .with(targets)
        .with(layer)
        .with(level)
        .init();
}
