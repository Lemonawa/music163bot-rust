use super::{Bytes, RawDocumentParams, Result, send_raw_upload_form};

/// Upload an in-memory document via raw reqwest multipart.
pub(super) async fn raw_send_document_bytes(
    client: &reqwest::Client,
    api_base_url: &str,
    filename: &str,
    content: Bytes,
    params: &RawDocumentParams<'_>,
) -> Result<serde_json::Value> {
    let len = content.len() as u64;
    let mut form = reqwest::multipart::Form::new().text("chat_id", params.chat_id.to_string());

    if let Some(caption) = params.caption {
        form = form.text("caption", caption.to_owned());
    }

    let file_part = reqwest::multipart::Part::stream_with_length(content, len)
        .file_name(filename.to_owned())
        .mime_str("text/plain; charset=utf-8")?;
    form = form.part("document", file_part);

    let reply_params = format!(r#"{{"message_id":{}}}"#, params.reply_to_message_id);
    form = form.text("reply_parameters", reply_params);

    if let Some(ref markup_json) = params.reply_markup_json {
        form = form.text("reply_markup", markup_json.clone());
    }

    let url = format!("{api_base_url}sendDocument");
    send_raw_upload_form(client, &url, form, "sendDocument").await
}
