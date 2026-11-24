---
title: 翻译插件
sidebar_label: 翻译
sidebar_position: 2
---

# 翻译插件

Web 界面使用用户所选的语言。插件为每种语言提供一个词条目录，宿主在其中查找插件自己的文本：前端显示的字符串、插件所注册内容的名称、插件拒绝请求的原因，以及清单和配置模式中的文本。宿主不知道这些键的含义，只知道去哪里查找。

## 为每种语言提供词条目录

在 `plugin.toml` 中列出词条目录。`en` 是源目录；其他目录必须恰好翻译其中的全部键，并使用相同的占位符。

```toml
[translations]
en = "i18n/en.toml"
zh-CN = "i18n/zh-CN.toml"
```

```toml
# plugins/submission-limit/i18n/en.toml
'submission-limit.usedCount' = '{used} / {total} used'
```

```toml
# plugins/submission-limit/i18n/zh-CN.toml
'submission-limit.usedCount' = '已用 {used} / {total}'
```

```toml
# plugins/cooldown/i18n/en.toml
'cooldown.apiError.COOLDOWN_ACTIVE' = '{remaining_seconds, plural, one {Wait # more second before submitting again.} other {Wait # more seconds before submitting again.}}'
```

每个键都以插件 ID 开头，因此插件永远无法改写宿主或其他插件的文本。出于同样的原因，插件 ID 不能是宿主消息的命名空间：英文语言包中任何键的首段（`apiError`、`toast`、`nav` 等），以及键族 `plugin`、`config`、`identifier` 和 `apiError`，比较时不区分大小写。使用这类 ID 的插件无法加载，其上传会被拒绝，`broccoli-dev plugin new` 也不会生成它。词条目录中只允许另外两类键：清单文本（`plugin.<插件 ID>.…` 和 `config.<插件 ID>.…`，见下文）以及所注册内容的名称（`identifier.contestType.…`、`identifier.problemType.…` 和 `identifier.checker.…`）。含有其他任何键的词条目录无法加载，含有它的上传也会被拒绝；错误信息会列出这些键。标识符的名称只取自该标识符所解析到的插件：为你未注册的竞赛类型命名不会显示任何内容。当没有已加载的插件提供某个标识符时，它保留最后提供它的插件所起的名称，因此已禁用的评测插件的题目类型仍显示为“传统题”，而不是 `batch`。已禁用或加载失败的插件只保留插件列表所显示的清单文本以及这些名称。

前端通过 `@broccoli/web-sdk/i18n` 中 `useTranslation()` 返回的 `t()` 读取词条。`{name}` 插入参数。`{count, plural, one {...} other {...}}` 按该语言的复数规则为 `count` 选择形式，分支中的 `#` 表示该数字。日期和相对时间由 `useFormat()` 按用户语言格式化；服务器时间戳请用 `useFormat().relative` 计算，它以服务器时钟为准。

## 添加语言

宿主自身的消息由语言包提供：清单中声明 `[language_pack]` 及其所提供语言的插件。`plugins/broccoli-en` 存放英文消息，其键即其他语言需要翻译的全部键；`plugins/broccoli-zh-cn` 存放简体中文。要添加一种语言，按相同结构提供一个语言包：

```toml
# plugin.toml
name = 'ja-language'
version = '0.1.0'

[language_pack]
locale = "ja"

[translations]
ja = "locales/ja.toml"
```

语言包为其所声明语言提供的词条目录存放宿主自己的键，因此不限于其 ID；它为其他语言提供的词条目录与任何插件一样受此限制。语言包只声明翻译：同时声明 `[server]`、`[web]` 或 `[config]` 的语言包无法加载。只提供翻译而未声明 `[language_pack]` 的插件是普通插件。像安装其他插件一样安装语言包，该语言即出现在语言菜单中，无需重新构建 Web 应用。

一种语言的宿主文本由一个已启用的语言包提供。若另一个已启用的语言包已提供该语言，则启用或上传第二个语言包会被拒绝（`LANGUAGE_PACK_TAKEN`）：请先禁用前一个，再启用后一个。检查上传时会给出已提供该语言的语言包（`language_pack_taken`），无需等到发布时被拒绝。若随附的语言包所提供的语言已由某个已启用的插件提供，则该语言包以禁用状态登记，服务器会记录日志；若两个新的随附语言包提供同一语言，则启用 ID 排序在前的那个。数据库强制执行这一规则：已启用插件的语言上有唯一索引。你自己的键和名称始终属于你：语言包为你插件 ID 下的键提供的文本，只在你的词条目录没有该键时显示。

Web 构建会内嵌仓库中的语言包，因此在服务器响应之前以及无法连接时界面也能使用这些语言；服务器提供的语言包会覆盖内嵌版本。语言包缺少的键以英文显示。

## 为注册的内容命名

宿主按名称显示比赛类型、题目类型和校验格式。在 `identifier.<kind>.<id>` 下给出名称；没有名称时直接显示 id。

| 注册的内容 | 键 |
| --- | --- |
| `register_contest_type("icpc", ...)` | `identifier.contestType.icpc` |
| `register_staged_evaluator("batch", ...)` | `identifier.problemType.batch` |
| `register_checker_resolver("exact", ...)` | `identifier.checker.exact` |

```toml
'identifier.contestType.icpc' = 'ICPC'
```

## 翻译拒绝原因

拒绝提交的钩子返回一个机器可读的代码。主机以插件 ID 为前缀转发它，因此 `submission-limit` 给出的 `SUBMISSION_LIMIT_EXCEEDED` 到达界面时是 `submission-limit.SUBMISSION_LIMIT_EXCEEDED`。界面显示词条目录中 `<插件 ID>.apiError.<CODE>` 的翻译，并以拒绝时的 `details` 作为参数，从不显示英文的 `message`：

```rust
HookResponse::reject(
    Refusal::SubmissionLimitExceeded,
    format!("Submission limit reached ({count}/{max})"),
    429,
    Some(serde_json::json!({ "submissions_made": count, "max_submissions": max })),
)
```

```toml
'submission-limit.apiError.SUBMISSION_LIMIT_EXCEEDED' = 'You have used all {max_submissions} submissions for this problem.'
```

该代码是插件用 `refusal_codes!` 统一声明的拒绝码之一；只要其中任何一个在某个翻译包中缺少文本，插件的测试 `broccoli_server_sdk::testing::assert_refusal_codes_translated` 就会失败（见[路由](./routes.md#错误)）。没有翻译的代码会退回到其 HTTP 状态类别的通用提示，绝不会显示主机对同名代码的说明。插件自有 HTTP 路由返回的错误使用同样的查找方式，其参数是拒绝用 `.detail(name, value)` 给出的详情（见[路由](./routes.md#错误)）。拒绝只以 `400`、`403`、`404`、`409`、`413` 或 `429` 发出；其他状态码一律以 `400` 发出。

## 翻译评测错误

插件以系统错误结束的评测记录或代码运行可以带上 `error_code`。主机会把它记在插件的 ID 下，因此 `icpc` 给出的 `NO_TEST_CASES` 会存为 `icpc.NO_TEST_CASES`，既不会与其他插件的代码冲突，也不会被当作主机自己的代码。界面只显示词条目录中 `<插件 ID>.apiError.<CODE>` 的翻译：你的词条目录未翻译的代码显示通用的系统错误提示，绝不会显示主机对同名代码的说明。例如：

```rust
host.submission.update(&SubmissionUpdate::standing_system_error(
    req.submission_id,
    req.judgement_id,
    req.judge_epoch,
    "NO_TEST_CASES",
    "Problem has no test cases; cannot be judged",
))?;
```

```toml
'icpc.apiError.NO_TEST_CASES' = '该题目没有测试点，无法评测。请联系比赛工作人员添加测试点。'
```

在提交的系统错误预算用完之前，主机会重新评测系统错误，提交也只在预算用完后才显示它。每次尝试都会重复的失败（例如按当前配置无法评测的题目）应当立即成立：`standing_system_error` 会设置 `SubmissionUpdate::stands`，于是提交立即显示该错误，在题目修改、工作人员重新评测之前不会再被评测。对于之后的尝试可能不会遇到的失败，不要设置 `stands`。不要把这样的提交以 0 分的 `Accepted` 结束：那会被当作通过。

## 翻译清单和配置模式

插件列表和配置表单显示清单中的 `name` 和 `description`，以及配置模式中的 `title`、`description` 和 `unit` 字段。它们在词条目录中各有一个键；缺少该键时显示 `plugin.toml` 中的原文。

| 文本 | 键 |
| --- | --- |
| 插件名称 | `plugin.<plugin id>.name` |
| 插件描述 | `plugin.<plugin id>.description` |
| 配置模式描述 | `config.<plugin id>.<namespace>.description` |
| 字段标题 | `config.<plugin id>.<namespace>.<field path>.title` |
| 字段描述 | `config.<plugin id>.<namespace>.<field path>.description` |
| 字段单位 | `config.<plugin id>.<namespace>.<field path>.unit` |

字段路径以点连接嵌套的属性名，因此 `batch-evaluator` 中 `[config.sandbox.properties.compile_time_limit_s]` 字段的标题键为 `config.batch-evaluator.sandbox.compile_time_limit_s.title`。

在插件自己的位置渲染某个字段的组件（`config.field.<plugin id>.<namespace>.<field path>` 插槽）会收到按同样方式选出的字段 `label` 和 `description`。请显示这两项，而不要显示 `schema.title` 或 `schema.description`，后两者只包含 `plugin.toml` 中的原文。

```toml
'plugin.batch-evaluator.description' = 'Judges batch problems: compile, run against each test, check the output.'
'config.batch-evaluator.sandbox.description' = 'Sandbox resource limits for compilation and execution steps'
```
