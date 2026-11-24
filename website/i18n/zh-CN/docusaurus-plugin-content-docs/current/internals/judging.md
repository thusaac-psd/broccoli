---
title: 评测
sidebar_label: 评测
sidebar_position: 1
---

# 评测

一次提交会依据题目的 `problem_type`，通过评测器注册表解析出对应的评测器
（`dispatch.rs:15-20`），运行准备好的操作，并为每个测试点产出一个判定
（verdict）和一个分数（score）。赛制插件（ICPC、IOI）随后把这些逐测试点的结
果汇聚成一个提交判定和一个比赛得分。

## 判定与得分是分开的

`TestCaseVerdict` 同时带有 `verdict` 字段和 `score` 字段
（`evaluate.rs:516-527`），二者互不替代。在批处理评测器（batch evaluator）
下，得分由解析出的检查器（checker）通过 `interpret_fused_result` 产出：
`interpret` 调用返回一个分数，这个分数会原样落在判定上
（`interpret.rs:22-143`）。没有检查器的路径（`checker_format == "none"`，
即自定义"运行代码"场景）给出固定的 `1.0` 通过分，不做比对。因此在批处理
下，检查器给出的分数是部分得分到达聚合层的唯一途径。

交互题使用的通信评测器（communication evaluator）则完全不同：它直接从交
互器（interactor）的标准输出里算出自己的小数分数，不经过
`interpret_fused_result`，也不涉及检查器这套抽象。它把管理程序
（manager）标准输出的第一行解析成一个 `f64`，把它限制在 `[0.0, 1.0]` 范
围内，然后原样写进 `TestCaseVerdict.score`。限制后只要分数不满 `1.0`，判
定就是 `Verdict::WrongAnswer`，而不是"部分通过"
（`communication-evaluator/src/interpret.rs:263-299`）。

管理程序自身出错（以非零码退出、被其自身导致的信号终止，或未输出有效分数）
是本题的失败，直接成立（`PROBLEM_FAILED`），无论选手程序多早退出：选手程序
通常在管理程序评分之前就已退出，时间先后无法区分提前离开的选手程序和正常结
束的选手程序。只有协议上的证据才归咎于选手：管理程序在向已退出的选手程序写
入时被 SIGPIPE 终止（判为答案错误），或管理程序自己为过短的交互给出分数，例
如 `0`。

## 评分与题目类型是解耦的

评测器由 `problem_type` 通过注册表选定（`dispatch.rs:15-20`），这个选择
与赛制无关。赛制插件只负责把它收到的逐测试点判定和得分，折算成比赛得分：
ICPC 把它们压成全部通过或不通过，IOI 把它们汇总为子任务总分。交互题走
自己专属的评测器（`communication`）；纯输出题则复用批处理评测器
（`batch`），并没有专属于自己的评测器。无论哪种情况，产出的都是
同样形态的 `TestCaseVerdict`，ICPC 或 IOI 都以相同方式聚合。

## 赛制的真实程度

| 方面 | ICPC | IOI |
| --- | --- | --- |
| 部分得分 | 没有，每个测试点非全对即全错 | 有，通过子任务和检查器给出的小数分数 |
| 子任务 | 没有 | `group_min`（在二元结果下对应 CMS 的 GroupMin）、`sum`（对应 CMS 的 Sum）、`group_mul`（对应 CMS 的 GroupMul） |
| 缺失的 CMS 功能 | 不适用 | 没有 GroupThreshold |
| 跨提交评分 | 最早通过的提交胜出，而不是最后一次 | `max_submission`、`sum_best_subtask`（对应 CMS IOI 2017 plus）、`best_tokened_or_last` |
| 平局判定 | 依据队伍名称顺序，而不是最后一次通过提交的时间 | 默认 `max_score_time`，依据用时，不是经典的并列名次 |
| 内置检查器的得分能力 | 二元 | 二元，只有通过 testlib 的分数检查器才能拿到部分得分 |
