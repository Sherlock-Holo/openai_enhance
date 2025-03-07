use std::async_iter::AsyncIterator;

use crate::ext_types::chat::CreateChatCompletionStreamResponse;

const THINK_BEGIN_TAG: &str = "<think>";
const THINK_END_TAG: &str = "</think>";

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum ThinkTagState {
    Init,
    Begin { trimmed_follow_new_line: bool }, // for some
    End,
    NoTag,
}

pub async gen fn extract_cot<
    S: AsyncIterator<Item = anyhow::Result<CreateChatCompletionStreamResponse>>,
>(
    st: S,
) -> anyhow::Result<CreateChatCompletionStreamResponse> {
    let mut state = ThinkTagState::Init;

    for await chunk in st {
        let mut chunk = match chunk {
            Err(err) => {
                yield Err(err);
                return;
            }

            Ok(chunk) => chunk,
        };

        if chunk.choices.is_empty() {
            yield Err(anyhow::anyhow!("empty choice"));
            return;
        }

        let delta = &chunk.choices[0].delta;

        // skip empty chunk
        if delta
            .reasoning_content
            .as_ref()
            .map(|s| s.is_empty())
            .unwrap_or_default()
            && delta
                .content
                .as_ref()
                .map(|s| s.is_empty())
                .unwrap_or_default()
        {
            continue;
        }

        match state {
            ThinkTagState::Init => {
                if delta.reasoning_content.is_some() {
                    state = ThinkTagState::End;

                    yield Ok(chunk);
                    continue;
                }

                match &delta.content {
                    None => {
                        yield Err(anyhow::anyhow!("reasoning_content or content is empty"));
                        return;
                    }

                    Some(content) => {
                        match content.strip_prefix(THINK_BEGIN_TAG) {
                            None => {
                                state = ThinkTagState::NoTag;

                                yield Ok(chunk);
                                continue;
                            }

                            Some(mut content) => {
                                state = ThinkTagState::Begin {
                                    trimmed_follow_new_line: false,
                                };

                                let trimmed_content = content.trim_start();
                                if trimmed_content != content {
                                    content = trimmed_content;
                                    state = ThinkTagState::Begin {
                                        trimmed_follow_new_line: true,
                                    };
                                }

                                if !content.contains(THINK_END_TAG) {
                                    let reasoning_content = content.to_string();
                                    chunk.choices[0].delta.reasoning_content =
                                        Some(reasoning_content);
                                    chunk.choices[0].delta.content = None;

                                    yield Ok(chunk);
                                    continue;
                                }

                                // for too short cot
                                state = ThinkTagState::End;

                                // ["reasoning_content", "content"]
                                let mut split_contents = content.splitn(2, THINK_END_TAG);
                                let reasoning_content = split_contents.next().unwrap().to_string();

                                let mut reasoning_chunk = chunk.clone();

                                reasoning_chunk.choices[0].delta.reasoning_content =
                                    Some(reasoning_content);
                                reasoning_chunk.choices[0].delta.content = None;

                                yield Ok(reasoning_chunk);

                                match split_contents.next() {
                                    Some(content) => {
                                        chunk.choices[0].delta.content =
                                            Some(content.trim_start().to_string());
                                    }

                                    None => continue,
                                }

                                yield Ok(chunk);
                            }
                        }
                    }
                }
            }

            ThinkTagState::Begin {
                trimmed_follow_new_line,
            } => {
                // ignore found think tag but content is null case, let client handle it
                if let Some(content) = &delta.content {
                    if !content.contains(THINK_END_TAG) {
                        let mut content = chunk.choices[0].delta.content.take();
                        if let Some(content) = content.as_mut() {
                            if !trimmed_follow_new_line {
                                state = ThinkTagState::Begin {
                                    trimmed_follow_new_line: true,
                                };
                                *content = content.trim_start().to_string();
                            }
                        }

                        chunk.choices[0].delta.reasoning_content = content;

                        yield Ok(chunk);
                        continue;
                    }

                    state = ThinkTagState::End;

                    // ["reasoning_content", "content"]
                    let mut split_contents = content.splitn(2, THINK_END_TAG);
                    let reasoning_content = split_contents.next().unwrap().to_string();

                    let mut reasoning_chunk = chunk.clone();

                    reasoning_chunk.choices[0].delta.reasoning_content = Some(reasoning_content);
                    reasoning_chunk.choices[0].delta.content = None;

                    yield Ok(reasoning_chunk);

                    match split_contents.next() {
                        Some(content) => {
                            let content = content.to_string();
                            chunk.choices[0].delta.reasoning_content = None;
                            chunk.choices[0].delta.content = Some(content);
                        }

                        None => continue,
                    }
                }

                yield Ok(chunk);
                continue;
            }

            ThinkTagState::End | ThinkTagState::NoTag => {
                yield Ok(chunk);
                continue;
            }
        }
    }
}
