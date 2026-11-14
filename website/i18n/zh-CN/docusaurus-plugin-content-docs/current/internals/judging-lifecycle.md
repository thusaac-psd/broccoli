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
| progress | 进行中 | `Compiling`、`Running` | 插件（`host.submission.update`、`host.code_run.update`） |
| finish | 进行中 | `Judged`、`CompilationError`（已定稿） | 插件 |
| fail | 进行中 | `SystemError`（已定稿） | 分发、已结束的评测会话（`fail_dispatch`） |
| requeue | 进行中 | `Queued`，新纪元，无属主 | `release_leases`、`reclaim_abandoned` |
| exhaust | 进行中 | `SystemError`（已定稿），写入死信队列 | `reclaim_abandoned` |
| system-error retry | 已定稿的 `SystemError`，当前评测记录 | `Queued`，新纪元 | `retry_system_errors` |

只有认领纤程会把工作推入进行中状态，因此进行中上限与分发器准入控制同样适用于首次分发与重试。每一种恢复都是一次重新入队：

- 服务器在启动时与优雅关闭时重新入队自己进行中的工作，不消耗重试次数（`release_leases`）；
- 租约窃取会重新入队租约已失效 `lease_ttl_secs` 的工作，卡死任务检测器会重新入队 `stuck_job_timeout_secs` 内没有心跳或分发时间早于 `max_inflight_secs` 的工作（`reclaim_abandoned`）；
- SystemError 重试回收器会重新入队以 `SystemError` 定稿的当前评测记录（`retry_system_errors`）。

一次租约窃取或卡死任务扫描会接管它选中的全部工作：它以每个事务 `steal_batch_size` 行（检测器为 256 行）的批次处理，并不断重复，直到某一批不满为止，因此无论宕机服务器有多少进行中的工作，都会在一个扫描间隔内全部重新入队。

所有租约时长都以数据库时钟比较：认领与刷新写入 `NOW()`，扫描也与 `NOW()` 比较。

## 认领

每台服务器上的认领纤程在得知有工作时认领。`submission_judgement` 与 `code_run` 上的触发器在修改该行的事务中向 `broccoli_claim` 频道发送 Postgres 通知，因此提交路径不多执行任何语句，任何把工作入队的路径也不会漏掉通知：

- 单元变为 `Queued` 时发送 `queued`：新提交或代码运行、重新评测、重新入队、系统错误重试或 DLQ 重试。同一事务中相同的通知由 Postgres 合并为一条。
- 某服务器拥有的单元离开进行中状态时发送 `freed:<server id>`。只有上一轮被进行中上限或分发准入限制的纤程才会响应它。

被唤醒的纤程认领一批，只要每一轮都填满批次且仍有余量就继续下一轮。认领期间到达的唤醒合并为再一轮，且距上一轮至少 10 ms，因此突发的提交会成批认领。所有副本都会被同一条通知唤醒，认领中的 `SKIP LOCKED` 在它们之间分配工作。一秒一次的后备轮询覆盖监听连接断开期间丢失的通知；每次重新连接也会唤醒纤程。

## 评测会话以判定结束

竞赛插件在一个分离会话中评测提交：宿主运行其测试点，并对每个结果回调插件（见 `services/windowed_session.rs`）。插件在某次回调中记录判定。会话循环在每一个出口都返回其结束方式 `SessionEnd`，评测驱动对其处理一次：

- `Decided`：插件结束了会话（完成、取消，或对最后一个结果的回答）。插件留下的未定稿评测记录以 `SystemError` 和 `PLUGIN_NO_VERDICT` 定稿。
- `Aborted`：某次回调失败，或插件已不存在（被禁用、被卸载）。评测记录以 `SystemError` 和 `PLUGIN_SESSION_ABORTED` 定稿。
- `Interrupted`：本服务器正在关闭。不记录任何内容；关闭流程会释放该评测记录，由其他服务器重新评测。

两种定稿都经由 `fail_dispatch`，以评测记录的纪元及其尚未定稿为栅栏，因此插件已写入的判定保持不变。两者之后都会触发评测后钩子。当提供某竞赛类型的插件被禁用时，提交到其竞赛的请求以 `503 CONTEST_TYPE_DISABLED` 拒绝，且不带 `Retry-After`；`CONTEST_TYPE_UNAVAILABLE`（带 `Retry-After`）只表示当前副本尚未加载该插件。

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

## 死信

耗尽分发重试的单元会以其主体写入死信队列：当前评测记录的主体是其提交，其余情况是评测记录或代码运行本身（`DlqSubject`）。每个主体至多有一行 `dead_letter_message`。再次写入同一主体会重新打开该行，并把这次发生追加到其重试历史。

提交的死信行会自动解决：当其当前评测记录以成立的判定结束（系统错误以外的任何终态）时，同一条语句会解决该行，并让 `resolved_by` 保持为 NULL。死信重试会重新评测判定为 `SystemError` 的提交，并把这次重试记入历史，该行保持打开，直到重新评测恢复。没有任何机制会重新评测卡住的延迟评测记录或代码运行，因此由运维人员解决这些行；丢弃该评测记录会解决其行。运维人员的解决会记录该运维人员。

评测机写入死信的操作任务（其执行重试或投递次数已耗尽）带有它所服务的评测记录，因此其行归入该提交（`submission_id`，评测记录 id 在负载中）。让提交得到成立判定的那条语句会连同提交自己的行一起解决这些行；在该判定之后才到达数据库的行（死信经由队列传递）会在写入时即被解决。没有提交的操作行不服务于任何评测记录。

## 锁

行按同一顺序加锁：先提交，再其评测记录，最后死信。涉及多行的语句（扫描、租约刷新）按 id 顺序并以 `SKIP LOCKED` 取行，因此从不等待繁忙的行，也不会与重新评测或插件写入死锁。
