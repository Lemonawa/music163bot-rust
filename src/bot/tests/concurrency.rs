
    async fn join_futures_with_parallel_assert<T, U, E, Left, Right>(
        left: Left,
        right: Right,
    ) -> (Result<T, E>, Result<U, E>)
    where
        Left: std::future::Future<Output = Result<T, E>>,
        Right: std::future::Future<Output = Result<U, E>>,
    {
        let start = std::time::Instant::now();
        let results = super::join_futures(left, right).await;
        assert!(
            start.elapsed() < Duration::from_millis(90),
            "Should run in parallel"
        );
        results
    }

    #[tokio::test]
    async fn inflight_entry_wait_returns_after_finish() {
        let entry = super::InflightEntry::new();
        entry.finish();

        tokio::time::timeout(Duration::from_secs(1), entry.wait())
            .await
            .expect("wait should return when already finished");
    }

    #[tokio::test]
    async fn inflight_entry_wait_wakes_on_finish() {
        let entry = Arc::new(super::InflightEntry::new());
        let entry_for_hook = Arc::clone(&entry);
        super::set_inflight_wait_hook(move || {
            entry_for_hook.finish();
        });

        let result = tokio::time::timeout(Duration::from_secs(1), entry.wait()).await;
        assert!(result.is_ok(), "wait should complete after finish");
    }

    #[tokio::test]
    async fn lyric_parallel_fetch() {
        let (res1, res2) = join_futures_with_parallel_assert(
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("lyric")
            },
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("detail")
            },
        )
        .await;

        assert_eq!(res1, Ok("lyric"));
        assert_eq!(res2, Ok("detail"));
    }

    #[tokio::test]
    async fn lyric_upload_resource_parallel() {
        let (res1, res2) = join_futures_with_parallel_assert(
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("client")
            },
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, ()>("permit")
            },
        )
        .await;

        assert_eq!(res1, Ok("client"));
        assert_eq!(res2, Ok("permit"));
    }
