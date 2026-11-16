---
title: 配置
sidebar_label: 配置
sidebar_position: 2
---

# 配置

插件的设置只在 `plugin.toml` 中以 schema 的形式声明一次，其余一切由宿主负责：宿主根据
schema 渲染设置表单，按 schema 检查写入的每一个值，并在把存储的值交给你的代码之前填入
schema 声明的默认值。因此，你的 Rust 配置类型只包含字段名和类型，不包含默认值或取值范围。

## 声明命名空间

每个 `[config.<namespace>]` 表是一组设置。`scopes` 列出它可以在哪些级别设置：`plugin`
（全局）、`problem`、`contest` 和 `contest_problem`。

```toml
[config.contest]
description = "ICPC contest-level configuration"
scopes = ["contest"]

[config.contest.properties.penalty_minutes]
type = "integer"
title = "Penalty Minutes"
default = 20
min = 0
max = 1440

[config.contest.properties.freeze_minutes]
type = "integer"
title = "Scoreboard Freeze (minutes)"
default = 0
min = 0
```

属性的 `type` 是 `string`、`integer`、`number`、`boolean`、`array` 和 `object` 之一。宿主
检查数字的 `min` 和 `max`，字符串的 `min_length`、`max_length` 和 `pattern`，`enum`，数组的
`items`，以及对象的 `properties` 和 `required` 字段。声明了属性的对象会拒绝未声明的字段，除非
设置 `additional_properties = true`。`step`、`precision`、`unit` 和 `span` 只影响表单的外观。

默认值不符合自身 schema 的清单，或者使用了未知类型的清单，无法加载。

## 宿主拒绝的写入

通过管理 API 或 `host.config.set*` 写入时，只有插件为该级别声明了这个命名空间、且值符合其
schema，写入才会被接受。管理员的写入若命名空间声明了 `validate` 函数，还必须通过它（见下文）：

| 响应 | 错误码 | 情形 |
| --- | --- | --- |
| 404 | `CONFIG_NAMESPACE_NOT_FOUND` | 插件未安装、没有这个命名空间，或者没有为该级别声明它 |
| 400 | `VALIDATION_ERROR` | 值不符合 schema，或命名空间的 `validate` 函数拒绝了它；`details.fields` 列出每个 `{path, message, kind}`（`validate` 的拒绝为 `kind: "plugin"`，可带 `key` 与 `params`） |
| 503 | `PLUGIN_UNAVAILABLE` | 命名空间声明了 `validate` 函数，而插件此刻无法应答 |

存储的值可以省略字段，读取时这些字段取默认值。升级插件改变了 schema 时，检查或上传压缩包的
结果会在 `incompatible_configs` 中列出新 schema 拒绝的每一条已存储配置。

## 拒绝 schema 表达不了的值

schema 只检查类型和范围，不检查含义。一份子任务列表可以类型完全正确，却引用了题目中不存在的
测试点，于是每份提交都得 0 分。在 `validate` 下写一个函数名，宿主就会在管理员每次写入该命名空间时，
在 schema 检查之后、存储之前调用它：

```toml
[config.task]
scopes = ["contest_problem"]
validate = "validate_task_config"
```

函数收到一个 `ConfigValidateInput`：命名空间、级别、配置所属的 `contest_id` 和 `problem_id`，
以及填好默认值的待写入值。其他需要的数据用常规的宿主函数读取，然后返回一个
`ConfigValidateOutput`，每个问题一条 `ConfigViolation`。列表为空即接受该值：

```rust
#[plugin_fn]
pub fn validate_task_config(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: ConfigValidateInput = serde_json::from_str(&input)?;
    let task: TaskConfig = req.parse()?;
    let problem_id = req
        .problem_id
        .ok_or_else(|| SdkError::Other("a task config belongs to a problem".into()))?;
    let labels = host.submission.query_test_cases(problem_id)?;
    let errors = task
        .subtasks
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.test_cases.iter().enumerate().map(move |(j, l)| (i, j, l)))
        .filter(|(_, _, label)| !labels.iter().any(|tc| tc.label.as_deref() == Some(label)))
        .map(|(i, j, label)| {
            ConfigViolation::new(
                format!("subtasks[{i}].test_cases[{j}]"),
                format!("this problem has no test case '{label}'"),
            )
            .key("my-plugin.config.task.unknownTestCase")
            .param("testCase", label.as_str())
        })
        .collect();
    Ok(serde_json::to_string(&ConfigValidateOutput { errors })?)
}
```

宿主把每条违规作为 `VALIDATION_ERROR` 的一个字段返回，设置表单会把它显示在 `path` 指向的字段旁，
并在保存失败的提示中一并列出。带 `key` 的违规以用户的语言显示：表单在插件的翻译包里查找这个键并
填入 `params`，翻译包里没有时显示 `message`。键必须以插件 id 开头（`my-plugin.`）；其他键会被宿主
丢弃，因此插件无法冒用宿主或其他插件的文字。函数出错或返回无法解析的内容时，写入同样被拒绝：宿主绝不存储
未经校验函数接受的值。

插件自己用 `host.config.set*` 写入的值只按 schema 检查。这些值归插件所有，宿主不会再交给它的
校验函数。

配置存储之后，它引用的测试数据也可能变化。因此，对一道题目测试点的每次修改（创建、编辑、删除、
重新排序、批量删除和 ZIP 上传）在提交之前，都会对该题存储的每条配置（题目级别的，以及使用该题的每场
比赛中的）重新做同样的检查，此时 `host.submission.query_test_cases` 返回的是修改之后的测试点。
若修改会使某条配置出现问题，请求会以 409 `CONFIG_WOULD_BREAK` 被拒绝，`details.issues` 以
`incompatible_configs` 的格式列出这些配置，除非请求带上 `force=true`。检查期间，插件自己的 SQL
（`host.db.*`）读到的是已提交的数据，所以校验函数应通过 `host.submission.query_test_cases` 读取测试点。

`GET /api/v1/problems/{id}/config/issues` 列出当前被拒绝的配置。题目的测试点对话框会显示它们；
在存在问题期间分发的评测，会向管理人员标明这一点（提交评测记录上的 `config_issues`）。

## 读取配置

宿主会填入 schema 声明的所有默认值，包括嵌套对象和数组元素中的默认值。把配置反序列化为不带
`Default`、也不带 `#[serde(default)]` 的类型，解析失败时传播错误，而不是退回某个默认值：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ContestConfig {
    pub penalty_minutes: i32,
    pub freeze_minutes: i32,
}

let config: ContestConfig = host.config.get_contest(contest_id, "contest")?.parse()?;
```

`EffectiveConfig::parse_config` 对级联读取 `host.config.get_effective` 的结果做同样的事。这样，
与 schema 不再一致的类型会在读取处明确报错。

## 保存题目的检查器源代码

若检查器插件的格式要编译题目自带的检查器（例如 testlib），插件应声明一个题目作用域、带
`checker_source = true` 的命名空间。对于检查器格式由该插件提供的每道题目，宿主的检查器源代码接口
会把管理员上传的文件以 `{ "files": [{ "filename", "content" }] }` 的形式存入该命名空间，题目编辑器的
检查器源代码页面也从这里显示。该命名空间不会在其他地方作为设置项出现。

```toml
[config.checker_source]
scopes = ["problem"]
checker_source = true

[config.checker_source.properties.files]
type = "array"
default = []
```

插件像读取其他题目配置一样读取它：`host.config.get_problem(problem_id, "checker_source")`。一个
清单最多只能标记一个命名空间，且其 `scopes` 只能是 `["problem"]`。

## 测试

`Host::mock()` 会读取被测 crate 的 `plugin.toml`，因此原生测试看到的配置与服务器填入默认值后的
配置相同：

```rust
let host = Host::mock();
host.config.seed("contest", "2", "contest", json!({"freeze_minutes": 15}));
let config: ContestConfig = host.config.get_contest(2, "contest")?.parse()?;
assert_eq!(config.penalty_minutes, 20);
```
