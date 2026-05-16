use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// --- Core ID types ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatId(pub i64);

impl std::fmt::Display for ChatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<i64> for ChatId {
    fn from(id: i64) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub i32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileId(pub String);

// --- ParseMode ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ParseMode {
    Html,
    #[serde(rename = "MarkdownV2")]
    MarkdownV2,
}

impl ParseMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Html => "HTML",
            Self::MarkdownV2 => "MarkdownV2",
        }
    }
}

// --- ReplyParameters ---

#[derive(Debug, Clone, Serialize)]
pub struct ReplyParameters {
    pub message_id: i32,
}

impl ReplyParameters {
    #[must_use]
    pub fn new(message_id: MessageId) -> Self {
        Self {
            message_id: message_id.0,
        }
    }
}

// --- Keyboard types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

impl InlineKeyboardMarkup {
    #[must_use]
    pub fn new(inline_keyboard: Vec<Vec<InlineKeyboardButton>>) -> Self {
        Self { inline_keyboard }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_inline_query_current_chat: Option<String>,
}

impl InlineKeyboardButton {
    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            url: Some(url.into()),
            callback_data: None,
            switch_inline_query_current_chat: None,
        }
    }

    pub fn callback(text: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            url: None,
            callback_data: Some(data.into()),
            switch_inline_query_current_chat: None,
        }
    }

    pub fn switch_inline_query(text: impl Into<String>, query: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            url: None,
            callback_data: None,
            switch_inline_query_current_chat: Some(query.into()),
        }
    }
}

// --- InputFile ---

#[derive(Debug, Clone)]
pub enum InputFile {
    FileId(String),
    Memory { data: Vec<u8>, filename: String },
    Disk(PathBuf),
}

impl InputFile {
    #[must_use]
    pub fn file_id(id: FileId) -> Self {
        Self::FileId(id.0)
    }

    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::Disk(path.into())
    }

    pub fn memory(data: impl Into<Vec<u8>>) -> Self {
        Self::Memory {
            data: data.into(),
            filename: String::new(),
        }
    }

    #[must_use]
    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        if let Self::Memory { filename, .. } = &mut self {
            *filename = name.into();
        }
        self
    }
}

// --- Inline query result types ---

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum InlineQueryResult {
    #[serde(rename = "article")]
    Article(InlineQueryResultArticle),
}

#[derive(Debug, Clone, Serialize)]
pub struct InlineQueryResultArticle {
    pub id: String,
    pub title: String,
    pub input_message_content: InputMessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl InlineQueryResultArticle {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        input_message_content: InputMessageContent,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            input_message_content,
            description: None,
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum InputMessageContent {
    #[serde(rename = "text")]
    Text(InputMessageContentText),
}

impl InputMessageContent {
    pub fn text(message_text: impl Into<String>) -> Self {
        Self::Text(InputMessageContentText::new(message_text))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InputMessageContentText {
    pub message_text: String,
}

impl InputMessageContentText {
    pub fn new(message_text: impl Into<String>) -> Self {
        Self {
            message_text: message_text.into(),
        }
    }
}

// --- Update & response types ---

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
    #[serde(default)]
    pub inline_query: Option<InlineQuery>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    #[serde(rename = "message_id")]
    pub id: MessageId,
    pub chat: Chat,
    #[serde(default)]
    pub from: Option<User>,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub reply_to_message: Option<Box<Message>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: ChatId,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub message: Option<MaybeInaccessibleMessage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MaybeInaccessibleMessage {
    Regular(Message),
    Inaccessible { chat: Chat, date: i64 },
}

#[derive(Debug, Clone, Deserialize)]
pub struct InlineQuery {
    pub id: String,
    pub from: User,
    pub query: String,
}

// --- Telegram API response wrapper ---

#[derive(Debug, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<T>,
    #[serde(default)]
    pub error_code: Option<i32>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GetMeResult {
    pub id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: String,
}
