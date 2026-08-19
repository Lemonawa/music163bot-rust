use std::future::{Future, IntoFuture};
use std::pin::Pin;

use serde::Deserialize;

use super::error::{ResponseResult, TelegramError};
use super::types::{
    BotCommand, ChatId, ChatMember, InlineKeyboardMarkup, InlineQueryResult, InputFile, Message,
    MessageId, ParseMode, ReplyParameters, Update, User,
};

#[derive(Debug, Clone)]
pub struct TelegramBot {
    token: String,
    client: reqwest::Client,
    api_url: String,
}

impl TelegramBot {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            client: reqwest::Client::new(),
            api_url: "https://api.telegram.org".to_string(),
        }
    }

    pub fn with_client(token: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            token: token.into(),
            client,
            api_url: "https://api.telegram.org".to_string(),
        }
    }

    #[must_use]
    pub fn set_api_url(mut self, url: &reqwest::Url) -> Self {
        let mut s = url.to_string();
        // Remove trailing slash for consistent formatting
        if s.ends_with('/') {
            s.pop();
        }
        self.api_url = s;
        self
    }

    #[must_use]
    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    fn method_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_url, self.token, method)
    }

    async fn call_api<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: &impl serde::Serialize,
    ) -> ResponseResult<T> {
        let url = self.method_url(method);
        let resp = self.client.post(&url).json(params).send().await?;
        let body: ApiResponse<T> = resp.json().await?;
        match body {
            ApiResponse::Ok { result } => Ok(result),
            ApiResponse::Err {
                error_code,
                description,
            } => Err(TelegramError::Api {
                error_code,
                description,
            }),
        }
    }

    #[must_use]
    pub fn get_me(&self) -> GetMeRequest<'_> {
        GetMeRequest { bot: self }
    }

    #[must_use]
    pub fn get_updates(
        &self,
        offset: i64,
        timeout: u32,
        allowed_updates: &[&str],
    ) -> GetUpdatesRequest<'_> {
        GetUpdatesRequest {
            bot: self,
            offset,
            timeout,
            allowed_updates: allowed_updates
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        }
    }

    pub fn send_message(
        &self,
        chat_id: impl Into<ChatId>,
        text: impl Into<String>,
    ) -> SendMessageRequest<'_> {
        SendMessageRequest {
            bot: self,
            chat_id: chat_id.into(),
            text: text.into(),
            parse_mode: None,
            reply_parameters: None,
            disable_link_preview: false,
            reply_markup: None,
        }
    }

    pub fn send_audio(&self, chat_id: impl Into<ChatId>, audio: InputFile) -> SendAudioRequest<'_> {
        SendAudioRequest {
            bot: self,
            chat_id: chat_id.into(),
            audio,
            caption: None,
            reply_parameters: None,
            reply_markup: None,
            parse_mode: None,
        }
    }

    pub fn edit_message_text(
        &self,
        chat_id: impl Into<ChatId>,
        message_id: MessageId,
        text: impl Into<String>,
    ) -> EditMessageTextRequest<'_> {
        EditMessageTextRequest {
            bot: self,
            chat_id: chat_id.into(),
            message_id,
            text: text.into(),
            parse_mode: None,
            reply_markup: None,
        }
    }

    pub fn delete_message(
        &self,
        chat_id: impl Into<ChatId>,
        message_id: MessageId,
    ) -> DeleteMessageRequest<'_> {
        DeleteMessageRequest {
            bot: self,
            chat_id: chat_id.into(),
            message_id,
        }
    }

    pub fn answer_callback_query(
        &self,
        callback_query_id: impl Into<String>,
    ) -> AnswerCallbackQueryRequest<'_> {
        AnswerCallbackQueryRequest {
            bot: self,
            callback_query_id: callback_query_id.into(),
            text: None,
        }
    }

    pub fn answer_inline_query(
        &self,
        inline_query_id: impl Into<String>,
        results: Vec<InlineQueryResult>,
    ) -> AnswerInlineQueryRequest<'_> {
        AnswerInlineQueryRequest {
            bot: self,
            inline_query_id: inline_query_id.into(),
            results,
            cache_time: None,
            is_personal: None,
        }
    }

    #[must_use]
    pub fn get_chat_member(
        &self,
        chat_id: impl Into<ChatId>,
        user_id: i64,
    ) -> GetChatMemberRequest<'_> {
        GetChatMemberRequest {
            bot: self,
            chat_id: chat_id.into(),
            user_id,
        }
    }

    #[must_use]
    pub fn set_my_commands(&self, commands: Vec<BotCommand>) -> SetMyCommandsRequest<'_> {
        SetMyCommandsRequest {
            bot: self,
            commands,
            language_code: None,
        }
    }
}

// --- SetMyCommands ---

pub struct SetMyCommandsRequest<'a> {
    bot: &'a TelegramBot,
    commands: Vec<BotCommand>,
    language_code: Option<String>,
}

impl SetMyCommandsRequest<'_> {
    /// Localize the command list for clients with this UI language
    /// (e.g. "zh" or "en"). Omit for the default (empty) list.
    #[must_use]
    pub fn language_code(mut self, code: impl Into<String>) -> Self {
        self.language_code = Some(code.into());
        self
    }

    pub async fn send(self) -> ResponseResult<bool> {
        #[derive(serde::Serialize)]
        struct Params {
            commands: Vec<BotCommand>,
            #[serde(skip_serializing_if = "Option::is_none")]
            language_code: Option<String>,
        }
        self.bot
            .call_api(
                "setMyCommands",
                &Params {
                    commands: self.commands,
                    language_code: self.language_code,
                },
            )
            .await
    }
}

impl<'a> IntoFuture for SetMyCommandsRequest<'a> {
    type Output = ResponseResult<bool>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- GetChatMember ---

pub struct GetChatMemberRequest<'a> {
    bot: &'a TelegramBot,
    chat_id: ChatId,
    user_id: i64,
}

impl GetChatMemberRequest<'_> {
    pub async fn send(self) -> ResponseResult<ChatMember> {
        #[derive(serde::Serialize)]
        struct Params {
            chat_id: i64,
            user_id: i64,
        }
        self.bot
            .call_api(
                "getChatMember",
                &Params {
                    chat_id: self.chat_id.0,
                    user_id: self.user_id,
                },
            )
            .await
    }
}

impl<'a> IntoFuture for GetChatMemberRequest<'a> {
    type Output = ResponseResult<ChatMember>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- API Response wrapper ---

#[derive(Deserialize)]
#[serde(untagged)]
enum ApiResponse<T> {
    Ok {
        result: T,
    },
    Err {
        error_code: i32,
        description: String,
    },
}

// --- GetMe ---

pub struct GetMeRequest<'a> {
    bot: &'a TelegramBot,
}

impl GetMeRequest<'_> {
    pub async fn send(self) -> ResponseResult<User> {
        #[derive(serde::Serialize)]
        struct Params {}
        self.bot.call_api("getMe", &Params {}).await
    }
}

impl<'a> IntoFuture for GetMeRequest<'a> {
    type Output = ResponseResult<User>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- GetUpdates ---

pub struct GetUpdatesRequest<'a> {
    bot: &'a TelegramBot,
    offset: i64,
    timeout: u32,
    allowed_updates: Vec<String>,
}

impl GetUpdatesRequest<'_> {
    pub async fn send(self) -> ResponseResult<Vec<Update>> {
        #[derive(serde::Serialize)]
        struct Params {
            offset: i64,
            timeout: u32,
            allowed_updates: Vec<String>,
        }
        self.bot
            .call_api(
                "getUpdates",
                &Params {
                    offset: self.offset,
                    timeout: self.timeout,
                    allowed_updates: self.allowed_updates,
                },
            )
            .await
    }
}

impl<'a> IntoFuture for GetUpdatesRequest<'a> {
    type Output = ResponseResult<Vec<Update>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- SendMessage ---

pub struct SendMessageRequest<'a> {
    bot: &'a TelegramBot,
    chat_id: ChatId,
    text: String,
    parse_mode: Option<ParseMode>,
    reply_parameters: Option<ReplyParameters>,
    disable_link_preview: bool,
    reply_markup: Option<InlineKeyboardMarkup>,
}

impl SendMessageRequest<'_> {
    pub fn parse_mode(mut self, mode: ParseMode) -> Self {
        self.parse_mode = Some(mode);
        self
    }

    pub fn reply_parameters(mut self, params: ReplyParameters) -> Self {
        self.reply_parameters = Some(params);
        self
    }

    pub fn disable_link_preview(mut self, disable: bool) -> Self {
        self.disable_link_preview = disable;
        self
    }

    pub fn reply_markup(mut self, markup: InlineKeyboardMarkup) -> Self {
        self.reply_markup = Some(markup);
        self
    }

    pub async fn send(self) -> ResponseResult<Message> {
        #[derive(serde::Serialize)]
        struct Params {
            chat_id: i64,
            text: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            parse_mode: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_parameters: Option<ReplyParameters>,
            #[serde(skip_serializing_if = "Option::is_none")]
            link_preview_options: Option<LinkPreviewOptions>,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_markup: Option<InlineKeyboardMarkup>,
        }

        #[derive(serde::Serialize)]
        struct LinkPreviewOptions {
            is_disabled: bool,
        }

        let params = Params {
            chat_id: self.chat_id.0,
            text: self.text,
            parse_mode: self.parse_mode.map(|m| m.as_str().to_string()),
            reply_parameters: self.reply_parameters,
            link_preview_options: if self.disable_link_preview {
                Some(LinkPreviewOptions { is_disabled: true })
            } else {
                None
            },
            reply_markup: self.reply_markup,
        };
        self.bot.call_api("sendMessage", &params).await
    }
}

impl<'a> IntoFuture for SendMessageRequest<'a> {
    type Output = ResponseResult<Message>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- SendAudio ---

pub struct SendAudioRequest<'a> {
    bot: &'a TelegramBot,
    chat_id: ChatId,
    audio: InputFile,
    caption: Option<String>,
    reply_parameters: Option<ReplyParameters>,
    reply_markup: Option<InlineKeyboardMarkup>,
    parse_mode: Option<ParseMode>,
}

impl SendAudioRequest<'_> {
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    pub fn reply_parameters(mut self, params: ReplyParameters) -> Self {
        self.reply_parameters = Some(params);
        self
    }

    pub fn reply_markup(mut self, markup: InlineKeyboardMarkup) -> Self {
        self.reply_markup = Some(markup);
        self
    }

    pub fn parse_mode(mut self, mode: ParseMode) -> Self {
        self.parse_mode = Some(mode);
        self
    }

    pub async fn send(self) -> ResponseResult<Message> {
        match &self.audio {
            InputFile::FileId(file_id) => {
                #[derive(serde::Serialize)]
                struct Params {
                    chat_id: i64,
                    audio: String,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    caption: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    parse_mode: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    reply_parameters: Option<ReplyParameters>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    reply_markup: Option<InlineKeyboardMarkup>,
                }
                let params = Params {
                    chat_id: self.chat_id.0,
                    audio: file_id.clone(),
                    caption: self.caption,
                    parse_mode: self.parse_mode.map(|m| m.as_str().to_string()),
                    reply_parameters: self.reply_parameters,
                    reply_markup: self.reply_markup,
                };
                self.bot.call_api("sendAudio", &params).await
            }
            _ => {
                // For file uploads, the raw upload path in telegram_api.rs handles this
                Err(TelegramError::Api {
                    error_code: 0,
                    description: "Use raw_send_file for file uploads".to_string(),
                })
            }
        }
    }
}

impl<'a> IntoFuture for SendAudioRequest<'a> {
    type Output = ResponseResult<Message>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- EditMessageText ---

pub struct EditMessageTextRequest<'a> {
    bot: &'a TelegramBot,
    chat_id: ChatId,
    message_id: MessageId,
    text: String,
    parse_mode: Option<ParseMode>,
    reply_markup: Option<InlineKeyboardMarkup>,
}

impl EditMessageTextRequest<'_> {
    pub fn parse_mode(mut self, mode: ParseMode) -> Self {
        self.parse_mode = Some(mode);
        self
    }

    pub fn reply_markup(mut self, markup: InlineKeyboardMarkup) -> Self {
        self.reply_markup = Some(markup);
        self
    }

    pub async fn send(self) -> ResponseResult<Message> {
        #[derive(serde::Serialize)]
        struct Params {
            chat_id: i64,
            message_id: i32,
            text: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            parse_mode: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_markup: Option<InlineKeyboardMarkup>,
        }
        let params = Params {
            chat_id: self.chat_id.0,
            message_id: self.message_id.0,
            text: self.text,
            parse_mode: self.parse_mode.map(|m| m.as_str().to_string()),
            reply_markup: self.reply_markup,
        };
        self.bot.call_api("editMessageText", &params).await
    }
}

impl<'a> IntoFuture for EditMessageTextRequest<'a> {
    type Output = ResponseResult<Message>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- DeleteMessage ---

pub struct DeleteMessageRequest<'a> {
    bot: &'a TelegramBot,
    chat_id: ChatId,
    message_id: MessageId,
}

impl DeleteMessageRequest<'_> {
    pub async fn send(self) -> ResponseResult<bool> {
        #[derive(serde::Serialize)]
        struct Params {
            chat_id: i64,
            message_id: i32,
        }
        let params = Params {
            chat_id: self.chat_id.0,
            message_id: self.message_id.0,
        };
        self.bot.call_api("deleteMessage", &params).await
    }
}

impl<'a> IntoFuture for DeleteMessageRequest<'a> {
    type Output = ResponseResult<bool>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- AnswerCallbackQuery ---

pub struct AnswerCallbackQueryRequest<'a> {
    bot: &'a TelegramBot,
    callback_query_id: String,
    text: Option<String>,
}

impl AnswerCallbackQueryRequest<'_> {
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub async fn send(self) -> ResponseResult<bool> {
        #[derive(serde::Serialize)]
        struct Params {
            callback_query_id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            text: Option<String>,
        }
        let params = Params {
            callback_query_id: self.callback_query_id,
            text: self.text,
        };
        self.bot.call_api("answerCallbackQuery", &params).await
    }
}

impl<'a> IntoFuture for AnswerCallbackQueryRequest<'a> {
    type Output = ResponseResult<bool>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- AnswerInlineQuery ---

pub struct AnswerInlineQueryRequest<'a> {
    bot: &'a TelegramBot,
    inline_query_id: String,
    results: Vec<InlineQueryResult>,
    cache_time: Option<u32>,
    is_personal: Option<bool>,
}

impl AnswerInlineQueryRequest<'_> {
    pub fn cache_time(mut self, seconds: u32) -> Self {
        self.cache_time = Some(seconds);
        self
    }

    pub fn personal(mut self, personal: bool) -> Self {
        self.is_personal = Some(personal);
        self
    }

    pub async fn send(self) -> ResponseResult<bool> {
        #[derive(serde::Serialize)]
        struct Params {
            inline_query_id: String,
            results: Vec<InlineQueryResult>,
            #[serde(skip_serializing_if = "Option::is_none")]
            cache_time: Option<u32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            is_personal: Option<bool>,
        }
        let params = Params {
            inline_query_id: self.inline_query_id,
            results: self.results,
            cache_time: self.cache_time,
            is_personal: self.is_personal,
        };
        self.bot.call_api("answerInlineQuery", &params).await
    }
}

impl<'a> IntoFuture for AnswerInlineQueryRequest<'a> {
    type Output = ResponseResult<bool>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- DeleteWebhook (used during startup) ---

impl TelegramBot {
    /// # Errors
    /// Returns an error if the API request fails.
    pub async fn delete_webhook(&self) -> ResponseResult<bool> {
        #[derive(serde::Serialize)]
        struct Params {
            drop_pending_updates: bool,
        }
        self.call_api(
            "deleteWebhook",
            &Params {
                drop_pending_updates: true,
            },
        )
        .await
    }
}
