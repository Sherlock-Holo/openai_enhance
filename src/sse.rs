use std::async_iter::AsyncIterator;

use reqwest::{Client, Method, Request, RequestBuilder, Url, header};
use reqwest_eventsource::{Error, Event, EventSource};
use serde::Serialize;

use crate::adapter::StreamAsyncIterAdapter;
use crate::ext_types::chat::CreateChatCompletionStreamResponse;

const END_SSE_DATA: &str = "[DONE]";

pub async fn send_stream_request<T: Serialize>(
    client: Client,
    url: Url,
    body: T,
) -> anyhow::Result<
    impl AsyncIterator<Item = anyhow::Result<CreateChatCompletionStreamResponse>> + use<T>,
> {
    let request = Request::new(Method::POST, url);
    let builder = RequestBuilder::from_parts(client, request)
        .header(header::CONTENT_TYPE, "application/json")
        .json(&body);

    let event_source = EventSource::new(builder)?;

    let stream = async gen {
        let event_source = StreamAsyncIterAdapter(event_source);
        for await event in event_source {
            match event {
                Ok(Event::Message(event)) => {
                    if event.data == END_SSE_DATA {
                        break;
                    }
                    match serde_json::from_str::<CreateChatCompletionStreamResponse>(&event.data) {
                        Ok(chunk) => yield Ok(chunk),
                        Err(e) => yield Err(e.into()),
                    }
                }
                Ok(Event::Open) => continue,
                Err(Error::StreamEnded) => break,
                Err(e) => yield Err(e.into()),
            }
        }
    };

    Ok(stream)
}
