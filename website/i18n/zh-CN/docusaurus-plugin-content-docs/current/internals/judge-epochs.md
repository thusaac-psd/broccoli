---
title: 评测纪元
sidebar_label: 评测纪元
sidebar_position: 2
---

# 评测纪元

评测纪元（judge epoch）是某个评测记录一次分发的隔离令牌。评测机结果、分发失败（`consumers/mod.rs` 中的 `mark_submission_system_error_with_epoch`）以及 `after_judging:{id}:{epoch}` 钩子去重键都以它为键。下文涉及的逻辑全部位于 `packages/server/src/services/judgement_lifecycle.rs`。

## 不变量

- 同一提交的两个评测记录永远不会共用一个纪元，提交的 `judge_epoch` 永不回退。
- `submission.judge_epoch` 是当前评测记录分发的隔离令牌。`submission.max_judge_epoch` 是分配器：该提交及其所有评测记录曾获得的最大纪元。延迟重测在这里预留纪元而不改动 `judge_epoch`，因此当前评测记录继续在自己的令牌下运行。
- 评测记录的 `test_case_result` 行携带该评测记录自身的纪元。插件按纪元把结果关联到评测记录（IOI 的 `sj.judge_epoch = tcr.judge_epoch`）。
- 仍在进行中却不再是当前记录的评测记录会被定为 `SystemError`（错误码 `SUPERSEDED`，卡住任务重试时为 `STUCK_JOB`），卡住检测器和抢占扫描器都不会再接手它。

## 代码如何维持这些不变量

- 新纪元只来自 `AllocateJudgeEpoch`：它在更新提交行的 UPDATE 语句内设置 `GREATEST(max_judge_epoch, judge_epoch) + 1`，行锁使并发分配串行化。不要在 Rust 中以 `epoch + 1` 计算纪元。
- 每个新评测记录都由 `open_judgement` 创建：管理员重测、延迟重测、DLQ 重试、卡住任务重试与租约抢占。它在同一事务中分配纪元、取代先前的当前评测记录并插入新记录。重测或 DLQ 重试开启新的评测尝试，并把 `retry_count` 重置为 0；重新分发则消耗一次。
- 对提交当前评测记录的每次重新分发都通过 `redispatch_current_submission` 写入提交行。评测记录自身的重试次数以 `GREATEST` 继承，提交的重试预算因此不会下降。
- `apply_judgement` 为被应用的评测记录分配新纪元，并在同一事务中把其保留的结果迁移到该纪元。
- 插件写入路径（`host_funcs/submissions.rs`）对评测记录的每次写入都以 `JUDGEMENT_WRITE_FENCE` 隔离：指定的评测记录、属于指定的提交、处于分发时的纪元、尚未定稿。延迟重测在立即重测并行运行时也能完成自己的评测记录，只有当前评测记录会更新提交缓存的结果。
