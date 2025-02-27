use std::future::ready;

use futures_util::{Stream, TryStreamExt};
use reqwest::{Client, Method, Request, RequestBuilder, Url};
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
struct Delta {
    pub content: String,
}

#[derive(Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    FunctionCall,
}

#[derive(Serialize, Deserialize)]
struct Choice {
    pub index: i64,
    pub delta: Delta,
    pub logprobs: Option<Value>,
    pub finish_reason: Option<FinishReason>,
    pub stop_reason: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Chunk {
    pub id: String,
    pub object: String,
    pub created: u32,
    pub model: String,
    pub choices: Vec<Choice>,
}

async fn send_stream_request<T: Serialize>(
    client: Client,
    url: &str,
    body: T,
) -> anyhow::Result<impl Stream<Item = anyhow::Result<Chunk>>> {
    let request = Request::new(Method::POST, Url::parse(url)?);
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
        .try_take_while(|event| ready(Ok(event.data != "[DONE]")))
        .map_err(anyhow::Error::from)
        .and_then(async |event| Ok(serde_json::from_str::<Chunk>(&event.data)?));

    Ok(stream)
}
