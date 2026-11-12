---
title: 评测生命周期
sidebar_label: 评测生命周期
sidebar_position: 2
---

# 评测生命周期

提交、其评测记录与代码运行的每一次状态变化都经由同一个模块 `packages/server/src/services/judging/`。该模块之外的代码可以读取这些行，但从不写入它们的状态、纪元、属主、租约或重试计数。

## 分发单元

分发器评测两类行：`submission_judgement` 与 `code_run`。提交本身从不被分发。`create_submission` 会把提交与其第一条评测记录一同插入，因此每个提交从存在之时起就有一条当前评测记录。

| 转换 | 起始状态 | 目标状态 | 写入方 |
| --- | --- | --- | --- |
| claim | `Queued` | `Pending`，有属主，租约已盖戳 | 认领纤程 |
| progress | 进行中 | `Compiling`、`Running` | 插件（`host.submission.update`） |
| finish | 进行中 | `Judged`、`CompilationError`（已定稿） | 插件 |
| fail | 进行中 | `SystemError`（已定稿） | 分发（`fail_dispatch`） |
| requeue | 进行中 | `Queued`，新纪元，无属主 | `release_leases`、`reclaim_abandoned` |
| exhaust | 进行中 | `SystemError`（已定稿），写入死信队列 | `reclaim_abandoned` |
| system-error retry | 已定稿的 `SystemError`，当前评测记录 | `Queued`，新纪元 | `retry_system_errors` |

只有认领纤程会把工作推入进行中状态，因此进行中上限与分发器准入控制同样适用于首次分发与重试。每一种恢复都是一次重新入队：

- 服务器在启动时与优雅关闭时重新入队自己进行中的工作，不消耗重试次数（`release_leases`）；
- 租约窃取会重新入队租约已失效 `lease_ttl_secs` 的工作，卡死任务检测器会重新入队 `stuck_job_timeout_secs` 内没有心跳或分发时间早于 `max_inflight_secs` 的工作（`reclaim_abandoned`）；
- SystemError 重试回收器会重新入队以 `SystemError` 定稿的当前评测记录（`retry_system_errors`）。

所有租约时长都以数据库时钟比较：认领与刷新写入 `NOW()`，扫描也与 `NOW()` 比较。

## 提交行是缓存

`submission.status`、`judge_epoch`、`target_worker_id` 以及评测输出列是当前评测记录的投影。当前评测记录的每一次转换都会在同一条语句或同一个事务中重写它们。插件写入结果是一条同时写入评测记录与提交的语句。

## 评测纪元

评测纪元是某个单元一次分发的隔离令牌。插件写入、分发失败以及 `after_judging:{id}:{epoch}` 钩子去重键都以它为键。

- 同一提交的两条评测记录从不共享纪元，提交的 `judge_epoch` 从不倒退。
- `submission.max_judge_epoch` 是分配器。每个评测记录纪元都在更新提交行的 UPDATE 语句内递增它来分配，行锁使并发分配串行化。代码运行每次运行一行，递增自己的 `judge_epoch`。不要在 Rust 中计算纪元。
- 重新入队总会取得新纪元，因此被替换的那次分发迟到的写入会被拒绝。
- 评测记录的 `test_case_result` 行携带该评测记录自己的纪元。插件以纪元把结果关联到评测记录（IOI 的 `sj.judge_epoch = tcr.judge_epoch`）。`apply_judgement` 为被应用的评测记录分配新纪元，并把其保留的结果迁移到该纪元。
- 仍在进行中却不再是当前评测记录的评测记录，会以 `SystemError`（`SUPERSEDED`）定稿。
- 插件写入隔离（`write_fence`）只允许写入指定提交的指定评测记录，且纪元为分发时的纪元、尚未定稿。

## 重试预算

`RetryBudget` 是唯一的规则：已消耗的重试少于 `max` 时允许再重试一次。

- `submission_judgement.dispatch_retries` 与 `code_run.dispatch_retries` 统计分发被放弃后的重新入队次数，预算为 `server.max_dispatch_retries`。租约窃取与卡死任务检测器共享该预算。
- `submission.system_error_retries` 统计自上次重新评测以来的系统错误重试次数，预算为 `server.max_system_error_retries`。每次这类重试都让评测记录获得全新的分发预算。重新评测会将其清零。

## 锁

语句先锁提交行，再锁其评测记录。扫描用 `FOR UPDATE OF ... SKIP LOCKED` 同时锁定单元及其提交，因此扫描从不等待，也不会与重新评测或插件写入死锁。
