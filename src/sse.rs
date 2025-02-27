use std::future::ready;

use futures_util::{Stream, TryStreamExt};
use reqwest::{Client, Method, Request, RequestBuilder, Url};
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const END_SSE_DATA: &str = "[DONE]";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, Ord, PartialOrd, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: i64,
    pub delta: Delta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub object: String,
    pub created: u32,
    pub model: String,
    pub choices: Vec<Choice>,
}

pub async fn send_stream_request<T: Serialize>(
    client: Client,
    url: Url,
    body: T,
) -> anyhow::Result<impl Stream<Item = anyhow::Result<Chunk>> + use<T>> {
    let request = Request::new(Method::POST, url);
    let builder = RequestBuilder::from_parts(client, request)
        .header("Content-Type", "application/json")
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
