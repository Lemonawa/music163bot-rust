fn percentile_95(samples: &VecDeque<f64>) -> f64 {
    let mut values: Vec<f64> = samples.iter().copied().collect();
    values.sort_by(f64::total_cmp);
    let len = values.len();
    let idx = ((len * 95).div_ceil(100)).saturating_sub(1);
    values[idx.min(len.saturating_sub(1))]
}

fn sample_resource_snapshot() -> ResourceSnapshot {
    let mut guard = lock_unpoisoned(&STATUS_RESOURCE_CACHE);
    let (system, last_refresh, snapshot) = &mut *guard;
    if last_refresh.elapsed() >= STATUS_RESOURCE_REFRESH_INTERVAL {
        system.refresh_cpu_usage();
        system.refresh_memory();
        let bot_memory_mb = sample_current_process_memory_mb(system);
        *snapshot = ResourceSnapshot {
            cpu_percent: system.global_cpu_usage(),
            system_used_memory_mb: system.used_memory() / (1024 * 1024),
            system_total_memory_mb: system.total_memory() / (1024 * 1024),
            bot_memory_mb,
        };
        *last_refresh = Instant::now();
    }
    *snapshot
}

fn sample_current_process_memory_mb(system: &mut System) -> Option<u64> {
    let current_pid = get_current_pid().ok()?;
    let pids = [current_pid];
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        false,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system
        .process(current_pid)
        .map(|process| process.memory() / (1024 * 1024))
}

fn format_bot_memory(bot_memory_mb: Option<u64>) -> String {
    bot_memory_mb.map_or_else(|| "n/a".to_string(), |mb| format!("{mb} MB"))
}

fn build_status_text(
    total_count: i64,
    user_count: i64,
    chat_count: i64,
    cache_snapshot: CacheSnapshot,
    resource_snapshot: ResourceSnapshot,
    uptime: &str,
    download_line: &str,
    upload_line: &str,
) -> String {
    format!(
        "📊 <b>系统状态</b>\n\
<b>实时运行指标</b>\n\n\
<b>💾 缓存</b>\n\
• 总缓存: <code>{total_count}</code>\n\
• 用户缓存: <code>{user_count}</code>\n\
• 群组缓存: <code>{chat_count}</code>\n\n\
<b>⚡ 运行缓存</b>\n\
• 命中: <code>{hits}</code>\n\
• 未命中: <code>{misses}</code>\n\
• 命中率: <code>{hit_rate:.2}%</code>\n\n\
<b>🖥️ 资源</b>\n\
• CPU: <code>{cpu:.1}%</code>\n\
• 系统内存: <code>{system_used}/{system_total} MB</code>\n\
• Bot 内存: <code>{bot_memory}</code>\n\
• 运行时长: <code>{uptime}</code>\n\n\
<b>🚀 传输</b>\n\
• {download_line}\n\
• {upload_line}",
        hits = cache_snapshot.hits,
        misses = cache_snapshot.misses,
        hit_rate = cache_snapshot.hit_rate_percent,
        cpu = resource_snapshot.cpu_percent,
        system_used = resource_snapshot.system_used_memory_mb,
        system_total = resource_snapshot.system_total_memory_mb,
        bot_memory = format_bot_memory(resource_snapshot.bot_memory_mb),
    )
}

fn format_uptime(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_speed_line(label: &str, snapshot: Option<SpeedSnapshot>) -> String {
    if let Some(snapshot) = snapshot {
        format!(
            "{label}: 实时 <code>{last:.2}</code> MB/s | 平均 <code>{avg:.2}</code> MB/s | P95 <code>{p95:.2}</code> MB/s | 样本 <code>{total}</code> (窗口 <code>{window}</code>)",
            last = snapshot.last_mbps,
            avg = snapshot.avg_mbps,
            p95 = snapshot.p95_mbps,
            total = snapshot.samples,
            window = snapshot.recent_samples
        )
    } else {
        format!("{label}: 暂无非缓存测速样本")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CoverPolicy {
    download_original: bool,
    download_thumbnail: bool,
    embed_tags: bool,
    embed_cover: bool,
}

fn resolve_cover_policy(cover_mode: CoverMode) -> CoverPolicy {
    let download_original = matches!(cover_mode, CoverMode::Original | CoverMode::Both);
    let download_thumbnail = matches!(cover_mode, CoverMode::Thumbnail | CoverMode::Both);

    CoverPolicy {
        download_original,
        download_thumbnail,
        embed_tags: true,
        embed_cover: download_original || download_thumbnail,
    }
}

#[must_use]
fn should_download_cover(policy: CoverPolicy) -> bool {
    policy.embed_cover || policy.download_thumbnail
}

pub async fn run(config: Config) -> Result<()> {
    tracing::info!("Starting Telegram bot...");

    // Ensure cache directory exists
    ensure_dir(&config.cache_dir)?;

    // Initialize database
    let database = Database::new(&config.database).await?;
    tracing::info!("Database initialized");

    // Initialize music API
    let music_api = Arc::new(MusicApi::new_with_config(&config));
    tracing::info!("Music API initialized");

    let (maintenance_tx, maintenance_rx) = tokio::sync::mpsc::channel(MAINTENANCE_QUEUE_CAPACITY);
    let maintenance_database = database.clone();
    let maintenance_music_api = Arc::clone(&music_api);
    tokio::spawn(async move {
        maintenance_worker(maintenance_rx, maintenance_database, maintenance_music_api).await;
    });

    // Initialize bot with custom API URL support
    let bot = if !config.bot_api.is_empty() && config.bot_api != "https://api.telegram.org" {
        // 使用自定义API URL
        // API URL must be base URL without "/bot" suffix - teloxide appends "bot<TOKEN>/" automatically
        let api_url_str = format!("{}/", config.bot_api.trim_end_matches("/bot"));

        match reqwest::Url::parse(&api_url_str) {
            Ok(api_url) => {
                tracing::info!(
                    "Using custom Telegram API URL: {}",
                    sanitize_sensitive_text(api_url.as_str())
                );

                // Create a custom HTTP client tuned for Cloudflare compatibility (mimic Go http client)
                // pool_max_idle_per_host(2) keeps reasonable connection pool for API efficiency
                let client_builder = reqwest::Client::builder()
                    .use_rustls_tls()
                    .user_agent("Go-http-client/2.0")
                    .pool_max_idle_per_host(2)
                    .pool_idle_timeout(std::time::Duration::from_secs(60))
                    .timeout(std::time::Duration::from_secs(30))
                    .no_gzip();
                let client = build_http_client(client_builder)?;

                // Create bot with custom client and API URL
                let bot = Bot::with_client(&config.bot_token, client).set_api_url(api_url.clone());

                // Test the connection with timeout and better error handling
                tracing::info!("Testing custom API connection...");
                match tokio::time::timeout(std::time::Duration::from_secs(15), bot.get_me()).await {
                    Ok(Ok(_)) => {
                        tracing::info!(
                            "✅ Custom API connection successful: {}",
                            sanitize_sensitive_text(api_url.as_str())
                        );
                        bot
                    }
                    Ok(Err(e)) => {
                        let error_msg = format!("{e}");
                        // Check if it's a CloudFlare challenge or other blocking issue
                        if error_msg.contains("Just a moment")
                            || error_msg.contains("cloudflare")
                            || error_msg.contains("challenge")
                        {
                            tracing::warn!(
                                "❌ Custom API blocked by CloudFlare protection. Falling back to official API."
                            );
                        } else {
                            tracing::warn!(
                                "❌ Custom API connection failed: {}. Falling back to official API.",
                                sanitize_sensitive_text(&e.to_string())
                            );
                        }
                        tracing::info!("Using fallback Telegram API URL: https://api.telegram.org");
                        Bot::new(&config.bot_token)
                    }
                    Err(_) => {
                        tracing::warn!(
                            "❌ Custom API connection timeout (15s). Falling back to official API."
                        );
                        tracing::info!("Using fallback Telegram API URL: https://api.telegram.org");
                        Bot::new(&config.bot_token)
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Invalid custom API URL '{}': {}. Using official API.",
                    sanitize_sensitive_text(&config.bot_api),
                    sanitize_sensitive_text(&e.to_string())
                );
                tracing::info!("Using fallback Telegram API URL: https://api.telegram.org");
                Bot::new(&config.bot_token)
            }
        }
    } else {
        // 使用默认API URL，但配置连接池以提高效率
        tracing::info!("Using default Telegram API URL: https://api.telegram.org");
        let client_builder = reqwest::Client::builder()
            .use_rustls_tls()
            .pool_max_idle_per_host(2)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .timeout(std::time::Duration::from_secs(30));
        let client = build_http_client(client_builder)?;
        Bot::with_client(&config.bot_token, client)
    };

    // Log the API configuration
    tracing::info!(
        "Music API configured: {}",
        sanitize_sensitive_text(&config.music_api)
    );

    let me = bot.get_me().await?;
    let bot_username = me
        .username
        .clone()
        .unwrap_or_else(|| "Music163bot".to_string());
    tracing::info!("Bot @{} started successfully!", bot_username);

    // Create bot state (needs bot username)
    let max_concurrent_downloads = config.max_concurrent_downloads;
    let upload_max_concurrent = config.upload_max_concurrent;
    let is_official_api = is_official_telegram_api(&config.bot_api);

    let bot_state = Arc::new(BotState {
        config,
        database,
        music_api: Arc::clone(&music_api),
        inflight_downloads: Arc::new(InflightDownloads::default()),
        download_semaphore: Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_downloads as usize,
        )),
        upload_semaphore: Arc::new(tokio::sync::Semaphore::new(upload_task_limit(
            upload_max_concurrent,
        ))),
        message_task_semaphore: Arc::new(tokio::sync::Semaphore::new(message_task_limit(
            max_concurrent_downloads,
        ))),
        maintenance_tx,
        bot_username,
        upload_client_state: Arc::new(Mutex::new(UploadClientState {
            bot: None,
            raw_client: None,
            upload_api_url: String::new(),
            reuse_count: 0,
        })),
        maintenance_counters: MaintenanceCounters::new(),
        upload_counters: UploadCounters::default(),
        runtime_metrics: RuntimeMetrics::new(),
        is_official_api,
    });

    let prewarm_state = Arc::clone(&bot_state);
    tokio::spawn(async move {
        let _ = run_upload_prewarm(&prewarm_state.config, || {
            acquire_upload_client(&prewarm_state)
        })
        .await;
    });

    // Create dispatcher
    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback))
        .branch(Update::filter_inline_query().endpoint(handle_inline_query));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![bot_state])
        .default_handler(|upd| async move {
            tracing::debug!("Unhandled update: {:?}", upd);
        })
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

async fn handle_message(bot: Bot, msg: Message, state: Arc<BotState>) -> ResponseResult<()> {
    if let MessageKind::Common(common) = &msg.kind
        && let teloxide::types::MediaKind::Text(text_content) = &common.media_kind
    {
        if !should_spawn_message_task(&text_content.text) {
            return Ok(());
        }
        let text = text_content.text.clone();

        let permit = match state.message_task_semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(e) => {
                tracing::error!("Message task semaphore closed: {}", sanitize_sensitive_text(&e.to_string()));
                return Ok(());
            }
        };

        let bot = bot.clone();
        let msg = msg.clone();
        let state = state.clone();

        // Spawn a new task to handle the message concurrently
        // This allows multiple messages to be processed in parallel
        tokio::spawn(async move {
            let _permit = permit;

            // Handle commands
            if is_command_text(&text) {
                if let Err(e) = handle_command(&bot, &msg, &state, &text).await {
                    tracing::error!("Error handling command: {}", sanitize_sensitive_text(&e.to_string()));
                }
            }
            // Handle music URLs
            else if contains_music_link_hint(&text)
                && let Err(e) = handle_music_url(&bot, &msg, &state, &text).await
            {
                tracing::error!("Error handling music URL: {}", sanitize_sensitive_text(&e.to_string()));
            }
        });
    }
    Ok(())
}

async fn handle_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    text: &str,
) -> ResponseResult<()> {
    let (command, args) = parse_command_and_args(text);

    // Only log music/search commands and admin commands
    if should_log_command(command) {
        tracing::info!("Command: /{} from chat {}", command, msg.chat.id);
    }

    match command {
        "start" => handle_start_command(bot, msg, state, args).await,
        "help" => handle_help_command(bot, msg, state).await,
        "music" | "netease" => handle_music_command(bot, msg, state, args).await,
        "search" => handle_search_command(bot, msg, state, args).await,
        "about" => handle_about_command(bot, msg, state).await,
        "lyric" => handle_lyric_command(bot, msg, state, args).await,
        "status" => handle_status_command(bot, msg, state).await,
        "rmcache" => handle_rmcache_command(bot, msg, state, args).await,
        "clearallcache" => {
            if is_clearallcache_confirm(args.as_deref()) {
                handle_clearallcache_confirm_command(bot, msg, state).await
            } else {
                handle_clearallcache_command(bot, msg, state).await
            }
        }
        _ => {
            // Unknown commands: don't respond (as requested)
            Ok(())
        }
    }
}

fn parse_command_and_args(text: &str) -> (&str, Option<String>) {
    let (command_part, args) = if let Some((cmd, rest)) = text.split_once(char::is_whitespace) {
        (cmd, Some(rest.trim_start().to_string()))
    } else {
        (text, None)
    };
    let args = args.filter(|arg| !arg.is_empty());
    let command = command_part.trim_start_matches('/');
    let command = command
        .split_once('@')
        .map_or(command, |(without_username, _)| without_username);
    (command, args)
}

fn parse_start_music_id(args: Option<&str>) -> Option<u64> {
    args.and_then(|arg| arg.trim().parse::<u64>().ok())
}

fn parse_inline_query_keyword(text: &str) -> (&str, bool) {
    let trimmed = text.trim();

    if let Some(prefix) = trimmed.get(..7)
        && prefix.eq_ignore_ascii_case("search ")
    {
        let keyword = trimmed.get(7..).unwrap_or("").trim();
        (keyword, true)
    } else if let Some(prefix) = trimmed.get(..6)
        && prefix.eq_ignore_ascii_case("search")
    {
        ("", true)
    } else {
        (trimmed, false)
    }
}

async fn handle_start_command(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    args: Option<String>,
) -> ResponseResult<()> {
    if let Some(music_id) = parse_start_music_id(args.as_deref()) {
        return process_music(bot, msg, state, music_id).await;
    }

    let welcome_text = format!(
        "👋 欢迎使用网易云音乐机器人 <b>@{}</b>\n\n\
        我可以帮你解析网易云音乐链接、搜索音乐、获取歌词。\n\n\
        <b>主要功能：</b>\n\
        • 直接发送网易云音乐链接进行解析\n\
        • 使用 <code>/search &lt;关键词&gt;</code> 搜索音乐\n\
        • 在任何聊天中使用 <code>@{} &lt;关键词&gt;</code> 进行 Inline 搜索\n\
        • 使用 <code>/lyric &lt;关键词或ID&gt;</code> 获取歌词\n\n\
        <b>开源地址：</b> <a href=\"https://github.com/Lemonawa/music163bot-rust\">Lemonawa/music163bot-rust</a>",
        state.bot_username, state.bot_username
    );

    bot.send_message(msg.chat.id, welcome_text)
        .parse_mode(ParseMode::Html)
        .disable_link_preview(true)
        .reply_parameters(ReplyParameters::new(msg.id))
        .await?;

    Ok(())
}
