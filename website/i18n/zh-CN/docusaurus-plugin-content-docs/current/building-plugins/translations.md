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
'limit.usedCount' = '{used} / {total} used'
```

```toml
# plugins/submission-limit/i18n/zh-CN.toml
'limit.usedCount' = '已用 {used} / {total}'
```

```toml
# plugins/cooldown/i18n/en.toml
'apiError.COOLDOWN_ACTIVE' = '{remaining_seconds, plural, one {Wait # more second before submitting again.} other {Wait # more seconds before submitting again.}}'
```

前端通过 `@broccoli/web-sdk/i18n` 中 `useTranslation()` 返回的 `t()` 读取词条。`{name}` 插入参数。`{count, plural, one {...} other {...}}` 按该语言的复数规则为 `count` 选择形式，分支中的 `#` 表示该数字。日期和相对时间由 `useFormat()` 按用户语言格式化；服务器时间戳请用 `useFormat().relative` 计算，它以服务器时钟为准。

## 添加语言

宿主自身的消息同样是语言包：清单中只声明 `[translations]` 的插件。`plugins/broccoli-en` 存放英文消息，其键即其他语言需要翻译的全部键；`plugins/broccoli-zh-cn` 存放简体中文。要添加一种语言，按相同结构提供一个语言包：

```toml
# plugin.toml
name = 'ja-language'
version = '0.1.0'

[translations]
ja = "locales/ja.toml"
```

像安装其他插件一样安装它，该语言即出现在语言菜单中，无需重新构建 Web 应用。Web 构建会内嵌仓库中的语言包，因此在服务器响应之前以及无法连接时界面也能使用这些语言；服务器提供的语言包会覆盖内嵌版本。语言包缺少的键以英文显示。

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

拒绝提交的钩子返回一个机器可读的代码。界面显示 `apiError.<CODE>` 的翻译，并以拒绝时的 `details` 作为参数，从不显示英文的 `message`：

```rust
HookResponse::reject(
    "SUBMISSION_LIMIT_EXCEEDED",
    format!("Submission limit reached ({count}/{max})"),
    429,
    Some(serde_json::json!({ "submissions_made": count, "max_submissions": max })),
)
```

```toml
'apiError.SUBMISSION_LIMIT_EXCEEDED' = 'You have used all {max_submissions} submissions for this problem.'
```

没有翻译的代码会退回到其 HTTP 状态类别的通用提示。插件自有 HTTP 路由返回的错误使用同样的查找方式。

## 翻译清单和配置模式

插件列表和配置表单显示清单中的 `name` 和 `description`，以及配置模式中的 `title` 和 `description` 字段。它们在词条目录中各有一个键；缺少该键时显示 `plugin.toml` 中的原文。

| 文本 | 键 |
| --- | --- |
| 插件名称 | `plugin.<plugin id>.name` |
| 插件描述 | `plugin.<plugin id>.description` |
| 配置模式描述 | `config.<plugin id>.<namespace>.description` |
| 字段标题 | `config.<plugin id>.<namespace>.<field path>.title` |
| 字段描述 | `config.<plugin id>.<namespace>.<field path>.description` |

字段路径以点连接嵌套的属性名，因此 `batch-evaluator` 中 `[config.sandbox.properties.compile_time_limit_s]` 字段的标题键为 `config.batch-evaluator.sandbox.compile_time_limit_s.title`。

```toml
'plugin.batch-evaluator.description' = 'Judges batch problems: compile, run against each test, check the output.'
'config.batch-evaluator.sandbox.description' = 'Sandbox resource limits for compilation and execution steps'
```
