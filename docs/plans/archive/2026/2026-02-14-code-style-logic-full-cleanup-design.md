# Code Style and Logic Full Cleanup Design

**Date:** 2026-02-14
**Scope:** `src/*.rs` (focus: `src/bot.rs`, `src/music_api.rs`, `src/audio_buffer.rs`)

## Goal
在不改变任何外部行为、配置语义、数据库语义和关键路径性能特征的前提下，完成全仓代码风格与逻辑清理，提升可读性、可维护性和后续迭代安全性。

## Hard Constraints
- 外部功能行为完全不变（命令、文案、回调、缓存语义、错误路径）。
- 外部性能特征完全不变（并发上限、关键路径顺序、网络/IO调用形态、锁使用策略）。
- 不新增配置项，不修改已有配置含义。
- 不修改数据库 schema 和 SQL 结果语义。

## Architecture Boundary
- 保持现有公开模块边界：`main/config/bot/music_api/audio_buffer/database/utils`。
- 仅做模块内重构：
  - 提取私有 helper
  - 统一重复判定
  - 收敛常量
  - 拆分超长函数中的局部逻辑块
- 不改变公开 API、类型和调用顺序语义。

## Component Strategy
### 1. `src/bot.rs`
- 目标：清理消息分发、上传编排、状态构建中的重复分支和长逻辑段。
- 方法：
  - 抽取纯函数（文本判断、参数拼装、日志门控、状态片段格式化）。
  - 将并发控制相关分支改写为等价表达（不改上限与时序）。
  - 合并重复 map/if 模式，保持返回值与日志一致。

### 2. `src/music_api.rs`
- 目标：清理 URL 回退、cookie 健康检测、缓存键和重写规则中的重复逻辑。
- 方法：
  - 保持候选顺序、阈值与重试策略，抽取内部 helper 以减少重复。
  - 统一条件表达形式，避免分支漂移。

### 3. `src/audio_buffer.rs`
- 目标：清理标签/封面路径中的重复流程代码。
- 方法：
  - 提取共享步骤 helper，保持编码参数和数据流不变。

### 4. 其他模块（`utils/config/database/main`）
- 目标：仅进行低风险风格统一与小型逻辑去重。
- 方法：避免触及行为敏感流程；以现有测试为等价护栏。

## Error Handling Policy
- 不改变 `BotError`/`Result` 对外语义。
- 仅重构内部重复 `map_err + log` 模式，日志级别和文案保持一致。
- 失败短路与返回时机保持一致。

## Testing and Verification
- 先增加特征化测试（当覆盖不足时），再进行对应重构。
- 每批次执行：
  - `cargo fmt -- --check`
  - `cargo check`
  - `cargo clippy -- -D warnings`
  - `cargo test`
- 关键输出使用现有测试断言保障（status 文本、bitrate 回退、cookie 探针、上传分流等）。

## Deliverables
- 清理后的代码改动（保持行为/性能特征不变）。
- 补充或增强的回归测试。
- 计划文档与验证命令输出记录。
