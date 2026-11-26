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
设置 `additional_properties = true`。`step`、`precision`、`unit` 和 `span` 只影响表单的外观；`secret`
会隐藏取值（见下文）。

默认值不符合自身 schema 的清单，或者使用了未知类型的清单，无法加载。

## 宿主拒绝的写入

通过管理 API 或 `host.config.set*` 写入时，只有插件为该级别声明了这个命名空间、且值符合其
schema，写入才会被接受。命名空间声明了 `validate` 函数时，每次写入（包括插件自己的写入）还必须通过它（见下文）：

| 响应 | 错误码 | 情形 |
| --- | --- | --- |
| 404 | `CONFIG_NAMESPACE_NOT_FOUND` | 插件未安装、没有这个命名空间，或者没有为该级别声明它 |
| 400 | `VALIDATION_ERROR` | 值不符合 schema，或命名空间的 `validate` 函数拒绝了它；`details.fields` 列出每个 `{path, message, kind}`（`validate` 的拒绝为 `kind: "plugin"`，可带 `key` 与 `params`） |
| 503 | `PLUGIN_UNAVAILABLE` | 命名空间声明了 `validate` 函数，而插件此刻无法应答 |

存储的值可以省略字段，读取时这些字段取默认值。升级插件改变了 schema 时，检查或上传压缩包的
结果会在 `incompatible_configs` 中列出新 schema 拒绝的每一条已存储配置。

写入时省略记录的 `enabled` 开关或 `position`，会保留已存储记录中的值；`"enabled": null` 会清除
开关，交由上一级决定。

## 保管密钥

字段的取值是凭据时（例如打印站登录用的令牌），把这个字符串字段标记为 `secret = true`。数组的项含有
密钥时，数组要用 `key` 指定一个用来标识各项的键字段：它必须是每一项都必填的字符串，且本身不是密钥。
数组含有密钥却没有键字段的清单无法加载。

```toml
[config.print.properties.stations]
type = "array"
key = "name"

[config.print.properties.stations.items]
type = "object"
required = ["name", "token"]

[config.print.properties.stations.items.properties.token]
type = "string"
title = "Token"
secret = true
```

JSON schema 会把该字段标记为 `writeOnly`，并把数组标记为 `x-key`。任何读取路由都不会发送密钥的值。
管理配置路由把已设置的密钥返回为它的引用 `{"$secret": "<ref>"}`（即 API schema 中的 `KeptSecret`）：
这是服务器每次存储新值时生成的不透明 id，值不变时引用也不变。级联结果把这个字段返回为 `null`。

写入只有在把这个引用发回、并且发在读取时显示它的那一项上（键字段取值相同的已存储项，而不是相同位置）时，
才会保留已存储的密钥。按读取结果原样保存配置时，无论增加、删除还是移动了哪些打印站，每个打印站的令牌都会保留。
其他情况都以写入方自己发送的内容为准：

- 字符串会设置密钥。再次发送已存储的值会保留它的引用，其他值会得到新的引用。
- `{"$clear": true}`（即 API schema 中的 `ClearSecret`）会清除它，使该字段读到未设置。
- 已存储密钥的位置上不带密钥（`null`、空字符串或省略该字段）会以 `secret_not_stored` 字段错误被拒绝；
  发送该项已存储密钥并不持有的引用也一样：写入方从未读取过这个密钥。删除一个打印站后又添加同名打印站的客户端、
  改名的打印站，或者在密钥重新设置之前读取到的引用，都拿不回原来的密钥。没有已存储密钥时，不带密钥则该字段保持未设置。
- 项必填的密钥只能设置或保留，不能清除，也不能不带密钥：发送清除标记或不带密钥，都会以 `secret_required`
  字段错误被拒绝。

键数组中两项的键相同时，会在第二项上以 `duplicate_key` 字段错误被拒绝。

设置表单把已设置的密钥显示为空的密码框，除非你输入新值，否则把它的引用发回；旁边的“清除”操作会发送清除标记
（必填的密钥没有该操作）。表单新增的项，或你改了键的项，即使某个已保存的项曾用过这个键，也算作新项：需要
输入它的密钥，留空则发送清除标记，因此任何已保存项的密钥都不会转给它。

服务器只在自己提供某个插件时才处理该插件的配置，即该插件已在这台服务器上加载，且版本是集群发布的版本。
对于它不提供的插件，每个配置路由（读取、写入、删除、插件自己的配置列表、配置问题）都以同一种可重试的 503
应答，并指明该插件：

- 另一台在线服务器提供它时，为 `PLUGIN_NOT_LOADED_HERE`；
- 没有在线服务器加载它时，为 `PLUGIN_NOT_LOADED`；
- 这台服务器运行的是其他版本时，为 `PLUGIN_OUTDATED_HERE`；
- 管理员已禁用它时，为 `PLUGIN_DISABLED`。

资源的配置列表仍会列出这类插件已存储的配置，但整体隐藏：`config` 为 `null`，`config_withheld` 为 `true`，
`unavailable` 为其路由应答的代码，因此设置对话框会在该插件的表单位置说明原因。插件已不再声明的命名空间也以同样
方式隐藏，但没有代码，因为这里无人能判断其中哪些字段是密钥；级联结果也不显示它的任何级别。

命名空间还可以声明一个简短的 `title`，作为它在设置表单中的标签页名称；没有时使用命名空间的 id。

插件自己读取的是真实取值：`host.config.get*` 和插件读取的级联结果都不会隐藏密钥。密钥仍可能读到未设置，
例如导出包恢复到另一台服务器上的配置，因此读取时请给出默认值，并把未设置的密钥视为不授予任何权限。

## 拒绝 schema 表达不了的值

schema 只检查类型和范围，不检查含义。一份子任务列表可以类型完全正确，却引用了题目中不存在的
测试点，于是每份提交都得 0 分。在 `validate` 下写一个函数名，宿主就会在每次写入该命名空间时，
在 schema 检查之后、存储之前调用它：

```toml
[config.task]
scopes = ["contest_problem"]
validate = "validate_task_config"
```

函数收到一个 `ConfigValidateInput`：命名空间、级别、配置所属的 `contest_id` 和 `problem_id`，
以及填好默认值的待写入值。其他需要的数据用常规的宿主函数读取，然后返回一个
`ConfigValidateOutput`，每个问题一条 `ConfigViolation`。两个列表都为空即接受该值：

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
    let issues = task
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
    Ok(serde_json::to_string(&ConfigValidateOutput {
        issues,
        ..Default::default()
    })?)
}
```

返回值有两个列表，违规放在哪个列表决定宿主是否存储该值：

- `errors` 是配置本身的错误，与服务器上的其他数据无关，例如两个子任务同名。宿主拒绝这次写入。
- `issues` 是一切依赖其他数据的条件：子任务引用了题目中不存在的测试点、题目还没有测试点、可直接晋级的人数
  超过比赛题目所能容纳的名额。宿主照常存储该值，并在配置或数据变化之前把每条问题列在目标的配置问题中。

只要规则读取了待写入值以外的任何内容（测试数据、比赛的题目、其他作用域的配置），就应放进 `issues`。
若拒绝保存，一旦数据越过这条规则，之后的每次保存（包括与之无关的字段）都会失败；而放进 `issues`，
管理人员可以按任意顺序修正数据和配置。

宿主把每条违规作为 `VALIDATION_ERROR` 的一个字段返回，设置表单会把它显示在 `path` 指向的字段旁，
并在保存失败的提示中一并列出。带 `key` 的违规以用户的语言显示：表单在插件的翻译包里查找这个键并
填入 `params`，翻译包里没有时显示 `message`。键必须以插件 id 开头（`my-plugin.`）；其他键会被宿主
丢弃，因此插件无法冒用宿主或其他插件的文字。函数出错或返回无法解析的内容时，写入同样被拒绝：宿主绝不存储
未经校验函数接受的值。

插件自己用 `host.config.set*` 写入的值也经过同样的检查：宿主以嵌套在写入调用之下的调用执行校验函数，
被它拒绝的写入会让发起写入的调用失败，与其他被拒绝的写入一样。校验函数自己写入配置会再次嵌套，所以不要在校验函数中写入配置。

配置存储之后，它引用的测试数据也可能变化。因此，对一道题目测试点的每次修改（创建、编辑、删除、
重新排序、批量删除和 ZIP 上传）在提交之前，都会对该题存储的每条配置（题目级别的，以及使用该题的每场
比赛中的）重新做同样的检查，此时 `host.submission.query_test_cases` 返回的是修改之后的测试点。
若修改会使某条已保存的配置出现原本没有的错误或问题，请求会以 409 `CONFIG_WOULD_BREAK` 被拒绝，`details.issues` 以
`incompatible_configs` 的格式列出这些配置，除非请求带上 `force=true`。在未保存配置的地方，命名空间默认值检查出的内容
（例如题目还没有测试点）从不拒绝修改。检查期间，插件自己的 SQL
（`host.db.*`）读到的是已提交的数据，所以校验函数应通过 `host.submission.query_test_cases` 读取测试点。

`GET /api/v1/problems/{id}/config/issues` 列出插件当前对该题每条配置（题目级别的，以及使用该题的每场比赛中的）
发现的内容，错误和问题一并列出。`GET /api/v1/contests/{id}/config/issues` 对比赛本身的配置及其题目在该比赛中的配置
列出同样的内容。题目的测试点对话框和比赛的题目对话框会显示它们；在题目存在问题期间分发的评测，会向管理人员标明这一点
（提交评测记录上的 `config_issues`，以及提交页面上的警告）。

校验函数不仅检查插件存储的每条配置，也检查没有存储配置的地方：对每个声明了 `validate` 的命名空间，宿主会在插件所提供类型
（它注册的比赛类型、题目类型和检查器格式）的每个未存储该配置的目标上，用其默认值进行检查。比赛级或比赛题目级的命名空间
在该比赛类型的每场比赛中检查；题目级的命名空间在题目类型、检查器格式或默认比赛类型由该插件提供的每道题目上检查。
因此，即使没有人为某道题目保存过配置，比赛类型无法评测的题目也会在有人提交之前显示出来。

每当结果可能改变时，系统都会重新检查题目是否存在配置问题：题目的测试数据变更时；该题的任何一条配置（题目级别的，或某场比赛中的）被写入或删除时，无论由谁发起（管理员、插件的 `host.config.set*`、上传检查器源代码）；题目被创建、类型改变、加入或离开比赛，或其所在比赛更改比赛类型或被删除时；以及你的插件被启用、停用、重新加载或升级时，插件级配置被写入或删除时，比赛本身的配置被写入或删除时，导入导出包之后，服务器启动时，对插件检查所涉及的每道题目重新检查。比赛的配置问题在每次读取时检查。插件停用期间，其配置不会被检查，因此在重新启用之前视为没有问题。

只有加载了你的插件的服务器才能运行它的检查。如果你的插件已启用，但在进行检查的服务器上没有加载（在那里加载失败，
或其文件尚未到达），结果就是未知的，而未知视为存在问题：题目保留其标记，并被标记为等待一台加载了该插件的服务器处理，
后者在下一次插件协调时作出判断。这台服务器的配置问题接口会返回带 `Retry-After` 的 503 `PLUGIN_UNAVAILABLE`，而不是空列表。

写入题目配置时，从检查到提交一直持有与测试数据变更相同的锁，因此配置不会在并发变更刚刚替换掉的测试数据上被存储。

## 随导出包迁移

题目或比赛导出包（`broccoli-server bundle export`）携带其导出内容的配置：题目自身的配置，以及比赛导出包中
比赛的配置和其题目在该比赛中的配置。导出包从不携带插件级配置：那是每台服务器自己的设置，导入时目标服务器的
设置保持不变。导入会连同目标服务器的题目一起替换其全部题目级与比赛题目级配置，因此被替换题目的配置不会留下来
去配置占用其 id 的导入题目。

导出包从不携带密钥字段（见“保管密钥”）。导出时按命名空间的 schema 隐藏每个密钥字段，schema 读取自插件已发布
的上传包或服务器的插件目录，并在导出包中记下隐藏的位置，数组项按其键字段标识。两处都没有清单的插件的配置，
以及其清单已不再声明所在命名空间的配置，都不进入导出包，导出结果会列出它们。导出的行只携带每个密钥的引用，
从不携带它的值。只有当目标服务器在导入替换任何内容之前、为同一配置的同一项存储的引用与导出包中的引用相同，
即是同一个已存储的密钥时，导入才会保留目标服务器的这个密钥。把服务器自己的导出包重新导入，会保留它原有的每个
密钥，除非其中某个此后被重新设置过。其他每个被隐藏的密钥都保持未设置，导入结果会说明有多少个。从另一个数据库的
转储恢复出来的数据库，只在双方此后都未重新设置的密钥上共享对方的引用；在另一道题目曾占用的 id 上替换进来的题目，
不会保留那道题目的任何密钥。

导入不运行任何插件，因此不检查它恢复的配置；题目的 `config_issues` 标记也不随导出包迁移，因为它描述的是源
服务器上的插件。导入的题目在有服务器作出判断之前，其配置问题是未知的，因此它以已标记存在问题的状态导入，
在此之前领取的评测会注明它是在配置存在问题时运行的。运行中的服务器在下一次插件协调时（几秒之内）判断被标记的题目，
无需重启。

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

`host.config.get_effective(namespace, problem_id, contest_id)` 读取某个命名空间在一道题目上的配置：
保存了该配置的每个作用域设定它所保存的字段，其余字段交给上一级作用域，依次为竞赛题目、竞赛、题目、
插件全局的值，最后是 schema 的默认值：对象逐字段合并，其他值（包括数组）整体替换。行中说明了是否开启的
最具体作用域决定配置是否开启，插件全局的行最后考虑。被关闭的覆盖项不提供任何字段；起决定作用的较窄作用域被关闭时，
配置是插件全局的配置。结果的 `source` 指明配置从中取字段的最具体作用域，`fields` 指明每个顶层字段来自哪个作用域。

这条规则就是 `broccoli-types` 中的 `resolve_cascade`，所有读取方都使用它：插件自己的读取；管理表单，它通过
`GET …/config/cascade`（`/problems/{id}`、`/contests/{id}` 和 `/contests/{id}/problems/{problem_id}`）显示每个作用域
继承的内容；以及宿主决定提交运行哪些资源级钩子（插件的任一命名空间在该处开启时，该插件即运行）。

## 标记评测读取的配置

对插件评测时读取的每个命名空间（沙箱限制、检查器选项、题目的子任务）声明
`judging_input = true`：

```toml
[config.sandbox]
judging_input = true
scopes = ["plugin"]
```

此后该命名空间某条已存配置的变更，会计为它所涉及的每道题目评测输入的变更：题目或竞赛题目的配置涉及
该题目，竞赛的配置涉及该竞赛的每道题目，插件作用域的配置涉及该插件评测的所有工作。变更之前完成的评测只有加上
`force` 才会被应用，并显示为基于较早的输入评测。其他命名空间（榜单、显示设置）的变更不影响已有评测。
`checker_source` 命名空间无论是否声明，都是评测输入。

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

若检查器要编译这些源代码，请在同一命名空间上用 `compiles` 列出会编译它的检查器格式，并用 `checker_languages` 声明每种主文件扩展名所用的语言：

```toml
[config.checker_source]
scopes = ["problem"]
checker_source = true
compiles = ["testlib"]
checker_languages = { cpp = ["cpp", "cc", "cxx"], c = ["c"] }
```

题目的检查器语言是第一个扩展名（不区分大小写）被列出的已存文件的语言。宿主在每次需要时推导它，且仅对检查器格式列在 `compiles` 中、其集群范围提供者为你的插件的题目推导，因此它始终跟随已存的源代码。此后该题目的工作像需要其自身语言的插件一样需要该语言的插件：该插件被禁用时，工作排队等待而不是失败。请按同一规则 `broccoli_server_sdk::checker_language` 编译，这样你编译所用的语言就是宿主等待的语言。

## 测试

`Host::mock()` 会读取被测 crate 的 `plugin.toml`，因此原生测试看到的配置与服务器填入默认值后的
配置相同：

```rust
let host = Host::mock();
host.config.seed("contest", "2", "contest", json!({"freeze_minutes": 15}));
let config: ContestConfig = host.config.get_contest(2, "contest")?.parse()?;
assert_eq!(config.penalty_minutes, 20);
```
