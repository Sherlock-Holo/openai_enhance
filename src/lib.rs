#![feature(gen_blocks)]
#![feature(async_iterator)]
#![feature(async_for_loop)]

mod adapter;
mod cli;
mod sse;

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::Uri;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response, Sse};
use axum::{
    Json, Router,
    http::{HeaderMap, Method, StatusCode, header},
    routing::post,
};
use clap::Parser;
use futures_util::{FutureExt, Stream, TryStreamExt, select};
use reqwest::{Client, Url};
use serde::Serialize;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::signal::unix::{self, SignalKind};
use tower_http::cors::{AllowHeaders, AllowPrivateNetwork, Any, CorsLayer};
use tracing::level_filters::LevelFilter;
use tracing::{debug, error, info, instrument, subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Registry, fmt};

use crate::adapter::StreamAsyncIterAdapter;
use crate::cli::Cli;
use crate::sse::{Chunk, send_stream_request};

#[derive(Debug)]
struct ServerState {
    backend: Url,
    client: Client,
}

#[instrument(err(Debug))]
async fn handle_completion(
    state: State<Arc<ServerState>>,
    uri: Uri,
    headers: HeaderMap,
    Json(payload): Json<HashMap<String, Value>>,
) -> Result<Response, (StatusCode, String)> {
    let stream = payload
        .get("stream")
        .and_then(|stream| stream.as_bool())
        .unwrap_or_default();

    forward_request(state, uri.path(), Method::POST, headers, stream, payload).await
}

#[instrument(err(Debug))]
async fn handle_chat(
    state: State<Arc<ServerState>>,
    uri: Uri,
    headers: HeaderMap,
    Json(payload): Json<HashMap<String, Value>>,
) -> Result<Response, (StatusCode, String)> {
    let stream = payload
        .get("stream")
        .and_then(|stream| stream.as_bool())
        .unwrap_or_default();

    forward_request(state, uri.path(), Method::POST, headers, stream, payload).await
}

#[instrument(err(Debug), skip(body))]
async fn forward_request<T: Serialize + 'static>(
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
        return match send_stream_request(state.client.clone(), url, headers, body).await {
            Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),

            Ok(sse_stream_response) => {
                let resp_iter = StreamAsyncIterAdapter(supplement_tool_call_fields(
                    StreamAsyncIterAdapter(sse_stream_response),
                ))
                .and_then(async |chunk| Ok(Event::default().json_data(chunk)?))
                .inspect_err(|err| {
                    error!(%err, "sse stream error happened");
                });

                let sse = Sse::new(resp_iter);

                Ok(sse.into_response())
            }
        };
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

            let data = match response.bytes().await {
                Err(err) => {
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from(err.to_string()))
                        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()));
                }

                Ok(data) => data,
            };

            let mut value = match serde_json::from_slice::<Value>(&data) {
                Err(err) => {
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from(err.to_string()))
                        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()));
                }

                Ok(value) => value,
            };

            insert_index(&mut value);

            let data = match serde_json::to_vec(&value) {
                Err(err) => {
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from(err.to_string()))
                        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()));
                }

                Ok(data) => data,
            };

            let body = Body::from(data);
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

async gen fn supplement_tool_call_fields(
    adapter: StreamAsyncIterAdapter<impl Stream<Item = anyhow::Result<Chunk>>>,
) -> anyhow::Result<Chunk> {
    let mut tool_call_id = None;
    let mut tool_call_index = None;
    let mut function_name = None;

    for await chunk in adapter {
        let mut chunk: Chunk = match chunk {
            Err(err) => {
                yield Err(err);
                return;
            }

            Ok(chunk) => chunk,
        };

        let choice = match chunk.choices.first_mut() {
            None => {
                yield Ok(chunk);
                continue;
            }
            Some(choice) => choice,
        };

        let delta = match choice.get_mut("delta") {
            None => {
                yield Ok(chunk);
                continue;
            }
            Some(delta) => delta,
        };

        let tool_calls = match delta.get_mut("tool_calls") {
            None => {
                yield Ok(chunk);
                continue;
            }

            Some(tool_calls) => match tool_calls.as_array_mut() {
                None => {
                    yield Ok(chunk);
                    continue;
                }
                Some(tool_calls) => tool_calls,
            },
        };

        debug!(?tool_calls, "found tool calls");

        let tool_call = match tool_calls.first_mut() {
            None => {
                yield Ok(chunk);
                continue;
            }

            Some(tool_call) => match tool_call.as_object_mut() {
                None => {
                    yield Ok(chunk);
                    continue;
                }

                Some(tool_call) => tool_call,
            },
        };

        debug!(?tool_call, "found tool call");

        match (&mut tool_call_id, tool_call.get_mut("id")) {
            (None, Some(id)) => match id.as_str() {
                None => {}

                Some(id) => {
                    tool_call_id = Some(id.to_string());

                    debug!(%id, "store tool call id");
                }
            },

            (Some(id), None) => {
                tool_call.insert("id".to_string(), id.as_str().into());

                debug!(?tool_call, "supplement tool call id");
            }

            (None, None) => {
                debug!(?tool_call, "no tool call id");
            }

            (Some(tool_call_id), Some(id)) if id.as_str() == Some("") => {
                tool_call.insert("id".to_string(), tool_call_id.as_str().into());

                debug!(?tool_call, "replace tool call empty id");
            }

            (tool_call_id, id) => {
                debug!(?tool_call_id, ?id, "other id state");
            }
        }

        match (tool_call_index, tool_call.get_mut("index")) {
            (None, Some(index)) => match index.as_i64() {
                None => {}

                Some(index) => {
                    tool_call_index = Some(index);

                    debug!(%index, "store tool call index");
                }
            },

            (Some(index), None) => {
                tool_call.insert("index".to_string(), index.into());

                debug!(?tool_call, "supplement tool call index");
            }

            (None, None) => {
                tool_call_index = Some(0);
                tool_call.insert("index".to_string(), 0.into());

                debug!(?tool_call, "supplement tool call index default 0");
            }

            (tool_call_index, index) => {
                debug!(?tool_call_index, ?index, ?index, "other index state");
            }
        }

        let function = match tool_call
            .get_mut("function")
            .and_then(|function| function.as_object_mut())
        {
            None => {
                yield Ok(chunk);
                continue;
            }

            Some(function) => function,
        };

        match (&mut function_name, function.get_mut("name")) {
            (None, Some(name)) => match name.as_str() {
                None => {}

                Some(name) => {
                    function_name = Some(name.to_string());

                    debug!(%name, "store tool call name");
                }
            },

            (Some(name), None) => {
                function.insert("name".to_string(), name.as_str().into());

                debug!(?function, "supplement tool call name");
            }

            (None, None) => {
                debug!(?function, "no tool call name");
            }

            (Some(function_name), Some(name)) if name.as_str() == Some("") => {
                function.insert("name".to_string(), function_name.as_str().into());

                debug!(?tool_call, "replace tool call empty name");
            }

            (function_name, name) => {
                debug!(?function_name, ?name, "other name state");
            }
        }

        yield Ok(chunk);
    }
}

fn insert_index(value: &mut Value) -> Option<()> {
    value
        .get_mut("choices")?
        .as_array_mut()?
        .first_mut()?
        .get_mut("tool_calls")?
        .as_array_mut()?
        .first_mut()?
        .as_object_mut()?
        .entry("index")
        .or_insert(0.into());

    Some(())
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

    let cors = CorsLayer::new()
        // allow `GET` and `POST` when accessing the resource
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(AllowHeaders::any())
        .allow_private_network(AllowPrivateNetwork::yes())
        // allow requests from any origin
        .allow_origin(Any);

    let app = Router::new()
        .route(
            "/completions",
            post(handle_completion).fallback(proxy_handler),
        )
        .route(
            "/chat/completions",
            post(handle_chat).fallback(proxy_handler),
        )
        .fallback(proxy_handler)
        .layer(cors)
        .with_state(Arc::new(ServerState {
            backend: cli.backend.parse()?,
            client: Default::default(),
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
    let layered = Registry::default().with(targets).with(layer).with(level);

    subscriber::set_global_default(layered).unwrap();
}
