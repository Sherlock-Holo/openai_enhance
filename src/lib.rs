mod sse;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::Uri;
use axum::response::Response;
use axum::{
    Json, Router,
    http::{HeaderMap, Method, StatusCode, header},
    routing::{any, post},
};
use clap::Parser;
use clap::builder::styling;
use educe::Educe;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tiktoken_rs::{CoreBPE, Rank, o200k_base};
use tokio::net::TcpListener;
use tracing::level_filters::LevelFilter;
use tracing::{info, instrument, subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Registry, fmt};

#[derive(Educe)]
#[educe(Debug)]
struct ServerState {
    backend: String,
    client: Client,
    max_token: usize,
    #[educe(Debug(ignore))]
    bpe: CoreBPE,
}

#[derive(Debug, Deserialize, Serialize)]
struct CompletionRequest {
    model: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,

    #[serde(flatten)]
    other_fields: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: VecDeque<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Message {
    role: String,
    content: String,
}

enum MessageType<'a> {
    Single(&'a mut String),
    Multiple(&'a mut VecDeque<Message>),
}

fn truncate_messages(bpe: &CoreBPE, messages: MessageType, max_token: usize) {
    match messages {
        MessageType::Single(message) => {
            let tokens = bpe.encode_with_special_tokens(message);
            if tokens.len() <= max_token {
                return;
            }

            info!(
                tokens_len = tokens.len(),
                max_token, "truncating single message"
            );

            truncate_message(bpe, max_token, message, tokens);
        }

        MessageType::Multiple(messages) => {
            let mut token_list = messages
                .iter()
                .map(|message| bpe.encode_with_special_tokens(&message.content))
                .collect::<VecDeque<_>>();

            let mut sum = token_list.iter().map(|tokens| tokens.len()).sum::<usize>();
            if sum <= max_token {
                return;
            }

            while sum > max_token {
                assert!(!token_list.is_empty());

                let token_len = token_list[0].len();
                if sum - token_len > max_token {
                    if token_list.len() > 1 {
                        sum -= token_len;
                        messages.pop_front();
                        token_list.pop_front();

                        info!("drop front message");

                        continue;
                    }

                    info!(sum, max_token, "truncating multiple message to single");

                    return truncate_messages(
                        bpe,
                        MessageType::Single(&mut messages[0].content),
                        max_token,
                    );
                }

                let new_len = sum - max_token;
                let tokens = token_list.pop_front().unwrap();

                info!(
                    sum,
                    max_token,
                    new_front_len = new_len,
                    "truncating front multiple message"
                );

                truncate_message(bpe, new_len, &mut messages[0].content, tokens);

                return;
            }
        }
    }
}

fn truncate_message(bpe: &CoreBPE, max_token: usize, content: &mut String, tokens: Vec<Rank>) {
    let mut tokens = VecDeque::from(tokens);
    tokens.drain(..max_token);
    content.clear();

    for data in bpe._decode_native_and_split(tokens.into()) {
        content.push_str(&String::from_utf8_lossy(&data));
    }
}

#[instrument(err(Debug))]
async fn handle_completion(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(mut payload): Json<CompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    truncate_messages(
        &state.bpe,
        MessageType::Single(&mut payload.prompt),
        state.max_token,
    );

    forward_request(state, "/v1/completions", Method::POST, headers, payload).await
}

#[instrument(err(Debug))]
async fn handle_chat(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(mut payload): Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    truncate_messages(
        &state.bpe,
        MessageType::Multiple(&mut payload.messages),
        state.max_token,
    );

    forward_request(
        state,
        "/v1/chat/completions",
        Method::POST,
        headers,
        payload,
    )
    .await
}

#[instrument(err(Debug), skip(body))]
async fn forward_request<T: Serialize>(
    state: State<Arc<ServerState>>,
    path: &str,
    method: Method,
    mut headers: HeaderMap,
    body: T,
) -> Result<Response, (StatusCode, String)> {
    headers = retain_headers(headers);
    let target_url = format!("{}{path}", state.backend);

    match state
        .client
        .request(method, &target_url)
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
    uri: Uri,
    mut headers: HeaderMap,
    body: Body,
) -> Result<Response, (StatusCode, String)> {
    headers = retain_headers(headers);
    let target_url = format!("{}{}", state.backend, uri.path());

    let response = match state
        .client
        .request(method, &target_url)
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

const STYLES: styling::Styles = styling::Styles::styled()
    .header(styling::AnsiColor::Green.on_default().bold())
    .usage(styling::AnsiColor::Green.on_default().bold())
    .literal(styling::AnsiColor::Blue.on_default().bold())
    .placeholder(styling::AnsiColor::Cyan.on_default());

#[derive(Debug, Parser)]
#[command(styles = STYLES)]
struct Cli {
    #[arg(short, long)]
    /// listen addr
    listen: String,

    #[arg(short, long)]
    /// backend addr
    backend: String,

    #[arg(short, long, default_value_t = 8192)]
    /// max token
    max_token: usize,

    #[arg(short, long)]
    /// enable debug log
    debug: bool,
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_log(cli.debug);

    info!("starting openai limiter, max token {}", cli.max_token);

    let bpe = o200k_base()?;
    let app = Router::new()
        .route("/v1/completions", post(handle_completion))
        .route("/v1/chat/completions", post(handle_chat))
        .fallback(any(proxy_handler))
        .with_state(Arc::new(ServerState {
            backend: cli.backend,
            client: Default::default(),
            max_token: cli.max_token,
            bpe,
        }));

    let listener = TcpListener::bind(cli.listen).await?;

    axum::serve(listener, app).await?;

    Ok(())
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
