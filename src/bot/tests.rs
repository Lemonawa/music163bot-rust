    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use super::UploadClientState;
    use super::acquire_download_permit;
    use super::append_search_result_line;
    use super::build_music_url;
    use super::build_program_url;
    use super::format_perf;
    use super::get_upload_bot;
    use super::parse_api_url;
    use super::resolve_cover_policy;
    use super::should_download_cover;
    use crate::config::Config;
    use crate::config::CoverMode;
    use crate::config::UploadLogLevel;
    use crate::utils::build_http_client;
    use teloxide::Bot;
    use uuid::Uuid;

    fn create_temp_file() -> PathBuf {
        let filename = format!("music163bot_local_uri_{}", Uuid::new_v4());
        let path = std::env::temp_dir().join(filename);
        fs::write(&path, b"ok").expect("write temp file");
        path
    }

    fn critical_path_stage_labels() -> [&'static str; 2] {
        [
            super::PERF_STAGE_SELECT_URL,
            super::PERF_STAGE_PRE_UPLOAD_PATH,
        ]
    }


include!("tests/concurrency.rs");
include!("tests/upload.rs");
include!("tests/scheduling.rs");
include!("tests/telegram.rs");
include!("tests/command_ui.rs");
