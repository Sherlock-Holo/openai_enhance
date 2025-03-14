use async_openai::types::{
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestAssistantMessageContentPart, ChatCompletionRequestDeveloperMessage,
    ChatCompletionRequestDeveloperMessageContent, ChatCompletionRequestMessage,
    ChatCompletionRequestMessageContentPartText, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart, Prompt,
};
use tiktoken_rs::CoreBPE;

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

fn truncate_prompt(bpe: &CoreBPE, prompt: &mut Prompt, max_token: usize) {
    match prompt {
        Prompt::String(s) => {
            let tokens = bpe.encode_with_special_tokens(s);
            if tokens.len() > max_token {
                let start = tokens.len() - max_token;
                let mut result = String::new();
                for bytes in bpe._decode_native_and_split(tokens[start..].to_vec()) {
                    result.push_str(&String::from_utf8_lossy(&bytes));
                }
                *s = result;
            }
        }

        Prompt::StringArray(arr) => {
            let mut total_tokens = 0;
            let mut truncated_arr = Vec::new();

            for s in arr.iter() {
                let tokens = bpe.encode_with_special_tokens(s);
                if total_tokens + tokens.len() <= max_token {
                    total_tokens += tokens.len();
                    truncated_arr.push(s.clone());
                } else {
                    let remaining = max_token - total_tokens;
                    if remaining > 0 {
                        let start = tokens.len() - remaining;
                        let mut result = String::new();
                        for bytes in bpe._decode_native_and_split(tokens[start..].to_vec()) {
                            result.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        truncated_arr.push(result);
                    }
                    break;
                }
            }
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
                                let mut truncated_text = String::new();
                                for bytes in bpe._decode_native_and_split(tokens[start..].to_vec())
                                {
                                    truncated_text.push_str(&String::from_utf8_lossy(&bytes));
                                }
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
                                    let mut truncated_text = String::new();
                                    for bytes in
                                        bpe._decode_native_and_split(tokens[start..].to_vec())
                                    {
                                        truncated_text.push_str(&String::from_utf8_lossy(&bytes));
                                    }
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
                    let mut truncated_text = String::new();
                    for bytes in bpe._decode_native_and_split(tokens[start..].to_vec()) {
                        truncated_text.push_str(&String::from_utf8_lossy(&bytes));
                    }
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
                        let mut truncated_text = String::new();
                        for bytes in bpe._decode_native_and_split(tokens[start..].to_vec()) {
                            truncated_text.push_str(&String::from_utf8_lossy(&bytes));
                        }
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
