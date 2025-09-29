use std::future::ready;

use axum::http::HeaderMap;
use futures_util::{Stream, TryStreamExt};
use reqwest::{Client, Method, Request, RequestBuilder, Url};
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const END_SSE_DATA: &str = "[DONE]";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub object: String,
    pub created: u32,
    pub model: String,
    pub choices: Vec<Value>,
}

pub async fn send_stream_request<T: Serialize>(
    client: Client,
    url: Url,
    headers: HeaderMap,
    body: T,
) -> anyhow::Result<impl Stream<Item = anyhow::Result<Chunk>> + use<T>> {
    let request = Request::new(Method::POST, url);
    let builder = RequestBuilder::from_parts(client, request)
        .header("Content-Type", "application/json")
        .headers(headers)
        .json(&body);

    let event_source = EventSource::new(builder)?;

    let stream = event_source
        .try_filter_map(|event| {
            ready(match event {
                Event::Open => Ok(None),
                Event::Message(event) => Ok(Some(event)),
            })
        })
        .try_take_while(|event| ready(Ok(event.data != END_SSE_DATA)))
        .map_err(anyhow::Error::from)
        .and_then(async |event| Ok(serde_json::from_str::<Chunk>(&event.data)?));

    Ok(stream)
}
