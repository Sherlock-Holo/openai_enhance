use async_openai::types::{
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestAssistantMessageContentPart, ChatCompletionRequestDeveloperMessage,
    ChatCompletionRequestDeveloperMessageContent, ChatCompletionRequestMessage,
    ChatCompletionRequestMessageContentPartText, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart, Prompt,
};
use tiktoken_rs::CoreBPE;

#[derive(Debug)]
pub enum MessageType<'a> {
    Prompt(&'a mut Prompt),
    Chat(&'a mut Vec<ChatCompletionRequestMessage>),
}

pub fn truncate_messages(bpe: &CoreBPE, messages: MessageType, max_token: usize) {
    match messages {
        MessageType::Prompt(prompt) => truncate_prompt(bpe, prompt, max_token),
        MessageType::Chat(messages) => truncate_chat_messages(bpe, messages, max_token),
    }
}

fn decode_tokens_to_string(bpe: &CoreBPE, tokens: Vec<u32>) -> String {
    let mut result = String::new();
    for bytes in bpe._decode_native_and_split(tokens) {
        result.push_str(&String::from_utf8_lossy(&bytes));
    }

    result
}

fn truncate_prompt(bpe: &CoreBPE, prompt: &mut Prompt, max_token: usize) {
    match prompt {
        Prompt::String(s) => {
            let tokens = bpe.encode_with_special_tokens(s);
            if tokens.len() > max_token {
                let start = tokens.len() - max_token;
                *s = decode_tokens_to_string(bpe, tokens[start..].to_vec());
            }
        }

        Prompt::StringArray(arr) => {
            let mut total_tokens = 0;
            let mut truncated_arr = Vec::new();

            for s in arr.iter().rev() {
                let tokens = bpe.encode_with_special_tokens(s);
                if total_tokens + tokens.len() <= max_token {
                    total_tokens += tokens.len();
                    truncated_arr.push(s.clone());
                } else {
                    let remaining = max_token - total_tokens;
                    if remaining > 0 {
                        let start = tokens.len() - remaining;
                        truncated_arr.push(decode_tokens_to_string(bpe, tokens[start..].to_vec()));
                    }
                    break;
                }
            }
            truncated_arr.reverse();
            *arr = truncated_arr;
        }
        Prompt::IntegerArray(_) => {}
        Prompt::ArrayOfIntegerArray(_) => {}
    }
}

fn truncate_chat_messages(
    bpe: &CoreBPE,
    messages: &mut Vec<ChatCompletionRequestMessage>,
    max_token: usize,
) {
    let mut total_tokens = 0;
    let mut truncated_messages = Vec::new();
    let mut system_message = None;

    // Save system message if exists
    if !messages.is_empty() {
        if let ChatCompletionRequestMessage::System(_) = &messages[0] {
            system_message = Some(messages[0].clone());
        }
    }

    for msg in messages.iter().rev() {
        match msg {
            ChatCompletionRequestMessage::System(_) => continue,
            ChatCompletionRequestMessage::Tool(_) | ChatCompletionRequestMessage::Function(_) => {
                truncated_messages.push(msg.clone());
                continue;
            }

            _ => {
                if process_message(
                    bpe,
                    msg,
                    &mut total_tokens,
                    max_token,
                    &mut truncated_messages,
                ) {
                    break;
                }
            }
        }
    }

    // Reverse messages to restore original order
    truncated_messages.reverse();

    // Prepend system message if present
    if let Some(sys_msg) = system_message {
        truncated_messages.insert(0, sys_msg);
    }

    *messages = truncated_messages;
}

fn process_message(
    bpe: &CoreBPE,
    msg: &ChatCompletionRequestMessage,
    total_tokens: &mut usize,
    max_token: usize,
    truncated_messages: &mut Vec<ChatCompletionRequestMessage>,
) -> bool {
    match msg {
        ChatCompletionRequestMessage::User(m) => {
            process_user_message(bpe, m, total_tokens, max_token, truncated_messages)
        }
        ChatCompletionRequestMessage::Assistant(m) => {
            process_assistant_message(bpe, m, total_tokens, max_token, truncated_messages)
        }
        ChatCompletionRequestMessage::Developer(m) => {
            process_developer_message(bpe, m, total_tokens, max_token, truncated_messages)
        }

        _ => false,
    }
}

fn process_text<T: Clone>(
    bpe: &CoreBPE,
    text: &str,
    total_tokens: &mut usize,
    max_token: usize,
    create_message: impl FnOnce(String) -> T,
) -> (Option<T>, bool) {
    let tokens = bpe.encode_with_special_tokens(text);
    if *total_tokens + tokens.len() <= max_token {
        *total_tokens += tokens.len();
        (Some(create_message(text.to_string())), false)
    } else {
        let remaining = max_token - *total_tokens;
        if remaining > 0 {
            let start = tokens.len() - remaining;
            let mut truncated_text = String::new();
            for bytes in bpe._decode_native_and_split(tokens[start..].to_vec()) {
                truncated_text.push_str(&String::from_utf8_lossy(&bytes));
            }
            (Some(create_message(truncated_text)), true)
        } else {
            (None, true)
        }
    }
}

fn process_user_message(
    bpe: &CoreBPE,
    m: &ChatCompletionRequestUserMessage,
    total_tokens: &mut usize,
    max_token: usize,
    truncated_messages: &mut Vec<ChatCompletionRequestMessage>,
) -> bool {
    match &m.content {
        ChatCompletionRequestUserMessageContent::Text(s) => {
            let (msg, should_break) =
                process_text(bpe, s.as_str(), total_tokens, max_token, |text| {
                    let mut new_msg = m.clone();
                    new_msg.content = ChatCompletionRequestUserMessageContent::Text(text);
                    ChatCompletionRequestMessage::User(new_msg)
                });

            if let Some(msg) = msg {
                truncated_messages.push(msg);
            }

            should_break
        }

        ChatCompletionRequestUserMessageContent::Array(arr) => {
            let mut truncated_arr = Vec::new();
            let mut should_break = false;

            for content in arr.iter().rev() {
                match content {
                    ChatCompletionRequestUserMessageContentPart::Text(s) => {
                        let tokens = bpe.encode_with_special_tokens(&s.text);
                        if *total_tokens + tokens.len() <= max_token {
                            *total_tokens += tokens.len();
                            truncated_arr.push(content.clone());
                        } else {
                            let remaining = max_token - *total_tokens;
                            if remaining > 0 {
                                let start = tokens.len() - remaining;
                                let truncated_text =
                                    decode_tokens_to_string(bpe, tokens[start..].to_vec());
                                truncated_arr.push(
                                    ChatCompletionRequestUserMessageContentPart::Text(
                                        ChatCompletionRequestMessageContentPartText {
                                            text: truncated_text,
                                        },
                                    ),
                                );
                            }
                            should_break = true;
                            break;
                        }
                    }
                    other => truncated_arr.push(other.clone()),
                }
            }

            truncated_arr.reverse();

            let mut new_msg = m.clone();
            new_msg.content = ChatCompletionRequestUserMessageContent::Array(truncated_arr);
            truncated_messages.push(ChatCompletionRequestMessage::User(new_msg));

            should_break
        }
    }
}

fn process_assistant_message(
    bpe: &CoreBPE,
    m: &ChatCompletionRequestAssistantMessage,
    total_tokens: &mut usize,
    max_token: usize,
    truncated_messages: &mut Vec<ChatCompletionRequestMessage>,
) -> bool {
    if let Some(content) = &m.content {
        match content {
            ChatCompletionRequestAssistantMessageContent::Text(s) => {
                let (msg, should_break) =
                    process_text(bpe, s.as_str(), total_tokens, max_token, |text| {
                        let mut new_msg = m.clone();
                        new_msg.content =
                            Some(ChatCompletionRequestAssistantMessageContent::Text(text));
                        ChatCompletionRequestMessage::Assistant(new_msg)
                    });
                if let Some(msg) = msg {
                    truncated_messages.push(msg);
                }

                should_break
            }

            ChatCompletionRequestAssistantMessageContent::Array(arr) => {
                let mut truncated_arr = Vec::new();
                let mut should_break = false;

                for content in arr.iter().rev() {
                    match content {
                        ChatCompletionRequestAssistantMessageContentPart::Text(s) => {
                            let tokens = bpe.encode_with_special_tokens(&s.text);
                            if *total_tokens + tokens.len() <= max_token {
                                *total_tokens += tokens.len();
                                truncated_arr.push(content.clone());
                            } else {
                                let remaining = max_token - *total_tokens;
                                if remaining > 0 {
                                    let start = tokens.len() - remaining;
                                    let truncated_text =
                                        decode_tokens_to_string(bpe, tokens[start..].to_vec());
                                    truncated_arr.push(
                                        ChatCompletionRequestAssistantMessageContentPart::Text(
                                            ChatCompletionRequestMessageContentPartText {
                                                text: truncated_text,
                                            },
                                        ),
                                    );
                                }

                                should_break = true;
                                break;
                            }
                        }

                        other => truncated_arr.push(other.clone()),
                    }
                }

                truncated_arr.reverse();

                let mut new_msg = m.clone();
                new_msg.content = Some(ChatCompletionRequestAssistantMessageContent::Array(
                    truncated_arr,
                ));
                truncated_messages.push(ChatCompletionRequestMessage::Assistant(new_msg));

                should_break
            }
        }
    } else {
        truncated_messages.push(ChatCompletionRequestMessage::Assistant(m.clone()));

        false
    }
}

fn process_developer_message(
    bpe: &CoreBPE,
    m: &ChatCompletionRequestDeveloperMessage,
    total_tokens: &mut usize,
    max_token: usize,
    truncated_messages: &mut Vec<ChatCompletionRequestMessage>,
) -> bool {
    match &m.content {
        ChatCompletionRequestDeveloperMessageContent::Text(text) => {
            let tokens = bpe.encode_with_special_tokens(text);
            if *total_tokens + tokens.len() <= max_token {
                *total_tokens += tokens.len();
                truncated_messages.push(ChatCompletionRequestMessage::Developer(m.clone()));

                false
            } else {
                let remaining = max_token - *total_tokens;
                if remaining > 0 {
                    let start = tokens.len() - remaining;
                    let truncated_text = decode_tokens_to_string(bpe, tokens[start..].to_vec());
                    let mut new_msg = m.clone();
                    new_msg.content =
                        ChatCompletionRequestDeveloperMessageContent::Text(truncated_text);
                    truncated_messages.push(ChatCompletionRequestMessage::Developer(new_msg));
                }

                true
            }
        }

        ChatCompletionRequestDeveloperMessageContent::Array(arr) => {
            let mut truncated_arr = Vec::new();
            let mut should_break = false;

            for content in arr.iter().rev() {
                let tokens = bpe.encode_with_special_tokens(&content.text);
                if *total_tokens + tokens.len() <= max_token {
                    *total_tokens += tokens.len();
                    truncated_arr.push(content.clone());
                } else {
                    let remaining = max_token - *total_tokens;
                    if remaining > 0 {
                        let start = tokens.len() - remaining;
                        let truncated_text = decode_tokens_to_string(bpe, tokens[start..].to_vec());
                        truncated_arr.push(ChatCompletionRequestMessageContentPartText {
                            text: truncated_text,
                        });
                    }

                    should_break = true;

                    break;
                }
            }

            truncated_arr.reverse();

            let mut new_msg = m.clone();
            new_msg.content = ChatCompletionRequestDeveloperMessageContent::Array(truncated_arr);
            truncated_messages.push(ChatCompletionRequestMessage::Developer(new_msg));

            should_break
        }
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::{
        ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
    };
    use tiktoken_rs::{CoreBPE, o200k_base};

    use super::*;

    fn get_test_bpe() -> CoreBPE {
        o200k_base().unwrap()
    }

    #[test]
    fn test_decode_tokens_to_string() {
        let bpe = get_test_bpe();
        let text = "Hello, world!";
        let tokens = bpe.encode_with_special_tokens(text);
        let decoded = decode_tokens_to_string(&bpe, tokens);
        assert_eq!(decoded, text);
    }

    #[test]
    fn test_truncate_prompt_string() {
        let bpe = get_test_bpe();
        let long_text = "This is a very long text that should be truncated. ".repeat(100);
        let mut prompt = Prompt::String(long_text.clone());

        truncate_prompt(&bpe, &mut prompt, 100);

        if let Prompt::String(truncated) = prompt {
            let tokens = bpe.encode_with_special_tokens(&truncated);
            assert!(tokens.len() <= 100);
            assert!(truncated.len() < long_text.len());

            dbg!(truncated);
        } else {
            panic!("Expected Prompt::String");
        }
    }

    #[test]
    fn test_truncate_prompt_string_array() {
        let bpe = get_test_bpe();
        let texts = vec![
            "First message".to_string(),
            "Second message".to_string(),
            "Third message".to_string(),
        ];
        let mut prompt = Prompt::StringArray(texts.clone());

        truncate_prompt(&bpe, &mut prompt, 5);

        if let Prompt::StringArray(truncated) = prompt {
            assert!(truncated.len() <= texts.len());
            let total_tokens: usize = truncated
                .iter()
                .map(|s| bpe.encode_with_special_tokens(s).len())
                .sum();
            assert!(total_tokens <= 5);

            dbg!(truncated);
        } else {
            panic!("Expected Prompt::StringArray");
        }
    }

    #[test]
    fn test_truncate_prompt_string_array_preserve_end() {
        let bpe = get_test_bpe();
        let texts = vec![
            "First message that should be truncated".to_string(),
            "Second message that should be truncated".to_string(),
            "Last important message that should be preserved".to_string(),
        ];
        let mut prompt = Prompt::StringArray(texts.clone());

        truncate_prompt(&bpe, &mut prompt, 10);

        if let Prompt::StringArray(truncated) = prompt {
            assert!(truncated.len() <= texts.len());
            let total_tokens: usize = truncated
                .iter()
                .map(|s| bpe.encode_with_special_tokens(s).len())
                .sum();
            assert!(total_tokens <= 10);

            // Verify that the last message is preserved
            assert!(truncated.last().unwrap().contains("Last important message"));

            dbg!(truncated);
        } else {
            panic!("Expected Prompt::StringArray");
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_truncate_chat_messages() {
        let bpe = get_test_bpe();
        let mut messages = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(
                    "You are a helpful assistant.".to_string(),
                ),
                name: None,
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(
                    "This is a very long user message that should be truncated. ".repeat(100),
                ),
                name: None,
            }),
            ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                    "This is a very long assistant message that should be truncated. ".repeat(100),
                )),
                name: None,
                tool_calls: None,
                function_call: None,
                refusal: None,
                audio: None,
            }),
        ];

        truncate_chat_messages(&bpe, &mut messages, 10);

        // Verify that system message is preserved
        assert!(matches!(
            messages[0],
            ChatCompletionRequestMessage::System(_)
        ));

        // Verify that messages are correctly truncated
        let total_tokens: usize = messages
            .iter()
            .filter_map(|msg| match msg {
                ChatCompletionRequestMessage::User(m) => match &m.content {
                    ChatCompletionRequestUserMessageContent::Text(s) => Some(s),
                    _ => None,
                },
                ChatCompletionRequestMessage::Assistant(m) => match &m.content {
                    Some(ChatCompletionRequestAssistantMessageContent::Text(s)) => Some(s),
                    _ => None,
                },
                _ => None,
            })
            .map(|s| bpe.encode_with_special_tokens(s).len())
            .sum();

        assert!(total_tokens <= 10);

        dbg!(messages);
    }

    #[test]
    fn test_truncate_chat_messages_with_array_content() {
        let bpe = get_test_bpe();
        let mut messages = vec![ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Array(vec![
                    ChatCompletionRequestUserMessageContentPart::Text(
                        ChatCompletionRequestMessageContentPartText {
                            text: "First part".to_string(),
                        },
                    ),
                    ChatCompletionRequestUserMessageContentPart::Text(
                        ChatCompletionRequestMessageContentPartText {
                            text: "Second part".to_string(),
                        },
                    ),
                ]),
                name: None,
            },
        )];

        truncate_chat_messages(&bpe, &mut messages, 5);

        if let ChatCompletionRequestMessage::User(msg) = &messages[0] {
            if let ChatCompletionRequestUserMessageContent::Array(parts) = &msg.content {
                let total_tokens: usize = parts
                    .iter()
                    .filter_map(|part| match part {
                        ChatCompletionRequestUserMessageContentPart::Text(s) => Some(&s.text),
                        _ => None,
                    })
                    .map(|s| bpe.encode_with_special_tokens(s).len())
                    .sum();
                assert!(total_tokens <= 5);

                dbg!(msg);
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected User message");
        }
    }
}
