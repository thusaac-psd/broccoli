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
schema，写入才会被接受：

| 响应 | 错误码 | 情形 |
| --- | --- | --- |
| 404 | `CONFIG_NAMESPACE_NOT_FOUND` | 插件未安装、没有这个命名空间，或者没有为该级别声明它 |
| 400 | `VALIDATION_ERROR` | 值不符合 schema；`details.fields` 列出每个 `{path, message}` |

存储的值可以省略字段，读取时这些字段取默认值。升级插件改变了 schema 时，检查或上传压缩包的
结果会在 `incompatible_configs` 中列出新 schema 拒绝的每一条已存储配置。

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

## 测试

`Host::mock()` 会读取被测 crate 的 `plugin.toml`，因此原生测试看到的配置与服务器填入默认值后的
配置相同：

```rust
let host = Host::mock();
host.config.seed("contest", "2", "contest", json!({"freeze_minutes": 15}));
let config: ContestConfig = host.config.get_contest(2, "contest")?.parse()?;
assert_eq!(config.penalty_minutes, 20);
```
