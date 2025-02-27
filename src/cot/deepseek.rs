use std::pin::pin;

use futures_util::{Stream, StreamExt};

use crate::sse::{Chunk, Delta};

const THINK_BEGIN_TAG: &str = "<think>";
const THINK_END_TAG: &str = "</think>";

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum ThinkTagState {
    Init,
    Begin,
    End,
    NoTag,
}

pub async gen fn extract_cot<S: Stream<Item = anyhow::Result<Chunk>>>(
    mut st: S,
) -> anyhow::Result<Chunk> {
    let mut state = ThinkTagState::Init;

    let mut st = pin!(st);
    while let Some(chunk) = st.next().await {
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

                            Some(content) => {
                                // for too short cot
                                if content.contains(THINK_END_TAG) {
                                    state = ThinkTagState::End;

                                    // ["reasoning_content", "content"]
                                    let mut split_contents = content.splitn(2, THINK_END_TAG);
                                    let reasoning_content =
                                        split_contents.next().unwrap().to_string();

                                    let mut reasoning_chunk = chunk.clone();
                                    reasoning_chunk.choices[0].delta = Delta {
                                        reasoning_content: Some(reasoning_content),
                                        content: None,
                                    };

                                    yield Ok(reasoning_chunk);

                                    match split_contents.next() {
                                        Some(content) => {
                                            chunk.choices[0].delta = Delta {
                                                reasoning_content: None,
                                                content: Some(content.to_string()),
                                            };
                                        }

                                        None => continue,
                                    }
                                }

                                yield Ok(chunk);
                            }
                        }
                    }
                }
            }

            ThinkTagState::Begin => {
                // ignore found think tag but content is null case, let client handle it
                if let Some(content) = &delta.content {
                    if !content.contains(THINK_END_TAG) {
                        chunk.choices[0].delta.reasoning_content =
                            chunk.choices[0].delta.content.take();

                        yield Ok(chunk);
                        continue;
                    }

                    state = ThinkTagState::End;

                    // ["reasoning_content", "content"]
                    let mut split_contents = content.splitn(2, THINK_END_TAG);
                    let reasoning_content = split_contents.next().unwrap();

                    let mut reasoning_chunk = chunk.clone();
                    reasoning_chunk.choices[0].delta = Delta {
                        reasoning_content: Some(reasoning_content.to_string()),
                        content: None,
                    };

                    yield Ok(reasoning_chunk);

                    match split_contents.next() {
                        Some(content) => {
                            chunk.choices[0].delta = Delta {
                                reasoning_content: None,
                                content: Some(content.to_string()),
                            };
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
