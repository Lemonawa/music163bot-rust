# Code Style and Logic Full Cleanup Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在不改变任何外部行为和性能特征的前提下，完成全仓代码风格与逻辑清理，显著提升可读性与可维护性。

**Architecture:** 保持现有模块边界，仅做模块内等价重构。通过“先特征化测试，再最小重构”方式分批推进，确保分支判定、调用顺序、并发限流和错误返回语义保持一致。所有改动以现有单元测试和新增回归测试作为等价护栏。

**Tech Stack:** Rust 2024, tokio, teloxide, reqwest, sqlx/sqlite, tracing, cargo fmt/clippy/test

---

### Task 1: Characterization Guardrail for Behavior/Perf-Critical Paths

**Files:**
- Modify: `src/bot.rs`
- Modify: `src/music_api.rs`
- Modify: `src/audio_buffer.rs`

**Step 1: Write the failing test**
- 增加 1-3 个“行为边界”测试（命令触发、bitrate fallback 顺序、标签处理输出一致性）用于锁定重构范围。

**Step 2: Run test to verify it fails**
- Run: `cargo test <exact_test_name> -- --exact`
- Expected: FAIL（说明测试覆盖了当前未显式约束的边界）

**Step 3: Write minimal implementation**
- 只做让测试转绿的最小等价调整，不做跨模块改动。

**Step 4: Run test to verify it passes**
- Run: `cargo test <exact_test_name> -- --exact`
- Expected: PASS

**Step 5: Commit**
- Run: `git add src/bot.rs src/music_api.rs src/audio_buffer.rs && git commit -m "test: add characterization guards for cleanup"`

### Task 2: `bot.rs` Orchestration and Helper Cleanup (Equivalent Refactor)

**Files:**
- Modify: `src/bot.rs`

**Step 1: Write the failing test**
- 为将要提取的逻辑段补一个边界测试（例如 dispatch 判定、status 文本段落格式）。

**Step 2: Run test to verify it fails**
- Run: `cargo test bot::tests::<test_name> -- --exact`
- Expected: FAIL

**Step 3: Write minimal implementation**
- 提取私有 helper、消除重复分支、收敛常量；保持调用顺序和返回值完全一致。

**Step 4: Run test to verify it passes**
- Run: `cargo test bot::tests::<test_name> -- --exact`
- Expected: PASS

**Step 5: Commit**
- Run: `git add src/bot.rs && git commit -m "refactor(bot): simplify orchestration helpers without behavior change"`

### Task 3: `music_api.rs` Fallback/Health/Rewrite Cleanup (Equivalent Refactor)

**Files:**
- Modify: `src/music_api.rs`

**Step 1: Write the failing test**
- 为 fallback 顺序或 cookie 健康判定增加边界测试。

**Step 2: Run test to verify it fails**
- Run: `cargo test music_api::tests::<test_name> -- --exact`
- Expected: FAIL

**Step 3: Write minimal implementation**
- 合并重复条件和 helper，保持候选顺序、阈值和重试时机不变。

**Step 4: Run test to verify it passes**
- Run: `cargo test music_api::tests::<test_name> -- --exact`
- Expected: PASS

**Step 5: Commit**
- Run: `git add src/music_api.rs && git commit -m "refactor(music_api): dedupe fallback and policy logic"`

### Task 4: `audio_buffer.rs` Shared Flow Cleanup (Equivalent Refactor)

**Files:**
- Modify: `src/audio_buffer.rs`

**Step 1: Write the failing test**
- 增加标签流程边界测试，锁定字节级等价输出。

**Step 2: Run test to verify it fails**
- Run: `cargo test audio_buffer::tests::<test_name> -- --exact`
- Expected: FAIL

**Step 3: Write minimal implementation**
- 抽取公共步骤 helper，避免重复逻辑；不改编码参数和数据顺序。

**Step 4: Run test to verify it passes**
- Run: `cargo test audio_buffer::tests::<test_name> -- --exact`
- Expected: PASS

**Step 5: Commit**
- Run: `git add src/audio_buffer.rs && git commit -m "refactor(audio_buffer): extract shared tagging steps"`

### Task 5: Cross-Module Style Polish and Full Verification

**Files:**
- Modify: `src/utils.rs`
- Modify: `src/config.rs`
- Modify: `src/database.rs`
- Modify: `src/main.rs`

**Step 1: Write the failing test**
- 仅当发现覆盖盲区时补测试；否则直接依赖现有回归测试集。

**Step 2: Run test to verify it fails**
- Run: `cargo test <exact_test_name> -- --exact`（若新增）
- Expected: FAIL

**Step 3: Write minimal implementation**
- 执行低风险风格统一与重复逻辑消除，不触及行为敏感语义。

**Step 4: Run test to verify it passes**
- Run: `cargo fmt -- --check && cargo check && cargo clippy -- -D warnings && cargo test`
- Expected: 全部 PASS

**Step 5: Commit**
- Run: `git add src/*.rs && git commit -m "style: full code style and logic cleanup with invariant behavior"`
