---
title: 插件数据库访问
sidebar_label: 插件数据库访问
sidebar_position: 6
---

# 插件数据库访问

拥有 `sql` 权限的插件通过 `host.db.*`（`packages/server/src/host_funcs/sql.rs`）执行 SQL。这些 SQL 能做什么由 Postgres 决定：每个插件以自己的角色登录，拥有一个模式，并可读取一组经过挑选的宿主表。宿主状态只通过宿主函数改变，宿主函数带有宿主所依赖的保护（评测纪元栅栏与已定稿保护、死信队列归属、`plugin_config` 变更计数器、账户与令牌）。

## 插件可以做什么

| 对象 | 权限 |
| --- | --- |
| 自己的模式 `plugin_<id>` | 所有者：完整的 DDL 与 DML、`TRUNCATE`、自有表之间的外键 |
| `public` 中的比赛数据（m0001 中的 `PLUGIN_READABLE`） | `SELECT` |
| `"user"` | `id`、`username`、`display_name`、`created_at`、`deleted_at` 列上的 `SELECT` |
| 其他所有宿主关系 | 无 |
| 宿主序列、模式 `public` | 无；`public` 上的 `CREATE` 也已从 `PUBLIC` 收回 |
| 其他插件的模式 | 无 |

可读关系是不含机密的比赛数据：比赛、题目、测试点、提交、评测、结果、代码运行和答疑，以及视图 `plugin_user_public`。不可读的有：密码哈希与凭据时间戳、刷新令牌、设备码、幂等键及其响应体、插件登录密钥、blob 存储、角色与权限、插件注册表、每个插件由宿主保管的状态（`plugin_config`、`plugin_storage`、`plugin_timer`、`plugin_replica`、`submission_feedback`）、死信队列，以及模式戳和迁移记录。

对宿主状态的写入经由宿主函数完成：评测与结果用 `host.submission.*` 和 `host.code_run.*`，此外还有 `host.storage.*`、`config:read`/`config:write`、`host.timer.*` 以及提交反馈函数。随仓库发布的插件都不直接写宿主表。

## 角色与模式

`packages/server/src/plugin_db/schema.rs` 从插件 id 推导出所有名称，因此宿主不持有任何针对单个插件的知识：

- 模式为 `plugin_<id>`；
- 登录角色为 `broccoli_<db>_<id>`，其中 `<db>` 是数据库名的摘要，因为同一集群的所有数据库共享角色；
- 超出 Postgres 63 字节标识符上限的 id 保留开头部分，并以整个 id 的摘要结尾。

每个插件角色为 `LOGIN INHERIT`，不带其他属性，是 NOLOGIN 组角色 `broccoli_plugin` 的成员，并拥有自己的模式。组角色持有 `public` 上的 `USAGE` 和各项 `SELECT` 授权，除此之外别无他物。角色的 `search_path` 是其模式加上应用自身的路径，因此 `CREATE TABLE job (...)` 落在 `plugin_<id>` 中，而 `FROM submission` 找到的是宿主表。服务端 SDK 中的 `Db::ensure_schema` 因此无需插件做任何改动，就会在插件自己的模式中创建对象。

宿主关系上的授权由迁移 m0001 施加（`PLUGIN_READABLE`、`PLUGIN_READABLE_COLUMNS`、`plugin_role`）。没有设置默认权限，因此宿主日后新增的表在被列入之前对插件不可读；对于既未列入可读也未列为隐藏的宿主关系，`plugin_db::tests::grants::every_host_relation_is_classified` 会失败。

## 开通

`schema::provision` 在插件首次于某台服务器上执行 SQL 时运行，在宿主函数连接池上以应用角色在一个事务中完成：

1. 为该角色向 `plugin_login_secret` 插入随机密码，已有则保留，并锁定该行；
2. 角色不存在时创建它，然后设置其属性和已存储的密码；
3. 将 `broccoli_plugin` 授予该角色，并将该角色授予应用角色，以便应用把模式交给它；
4. 以该角色执行 `CREATE SCHEMA IF NOT EXISTS ... AUTHORIZATION`；
5. 设置该角色的 `search_path`。

行锁使开通同一插件的多个副本串行执行，且任何插件都无法获取它：`plugin_login_secret` 不向插件授予任何权限。每个副本读回首个写入者的密码，因此它们都以同一密码连接。这些 DDL 属于模式戳的输入（`packages/server/build.rs` 中的 `SCHEMA_SOURCES`）。

创建角色需要 CREATEROLE。缺少它时，m0001 会跳过组角色并记录警告，开通随之失败，于是 `host.db.*` 返回错误，没有任何插件 SQL 以超出自身的权限运行。

## 连接池

`PluginSqlPools`（`packages/server/src/plugin_db/mod.rs`）在每个插件首次执行 SQL 时为其建立一个连接池。池中的连接以插件的角色认证，因此一次调用无需付出 `SET ROLE` 或 `SET search_path` 的代价，`RESET ROLE` 也会落回插件自己的角色。`SET ROLE broccoli_plugin` 只会失去插件自己的模式。

所有插件连接池共享一个连接预算 `PoolSizes::plugin`：

- 每次租用为一条语句（或为一个事务直至其结束）占用 `budget` 个许可之一，因此同时使用中的连接至多 `budget` 个；
- 当各池已持有 `budget` 个连接而某插件没有空闲连接时，它先关闭另一个插件的一个空闲连接；
- 在各池持有超过 `budget` 个连接时被释放的连接会被关闭而不是保留，这涵盖了越过上一项检查的并发首次使用，以及仍在返回连接池途中的连接。

`host.db.*` 以外的宿主函数在宿主函数连接池（`packages/server/src/database.rs` 中的 `connect_host_fn_pool`）上以应用角色运行，执行宿主构建的语句。插件编写的 SQL 从不在那里运行，也没有插件被授予超出上述的权限：不存在需要在清单中声明的特权插件层级。

## SQL 防护

`host_funcs/sql.rs` 中的 `reject_session_escape` 仍会拒绝角色变更、`set_config`、`DISCARD`、`DO`、例程定义，以及对宿主强制设置的会话级修改。角色变更已由引擎约束；该防护用于防止插件让其池化连接在下一次调用时脱离 `statement_timeout` 的限制。

## 边界不覆盖的部分

- 咨询锁、`LISTEN` 和 `NOTIFY` 对所有角色开放。插件可以占用宿主使用的锁键，或在 `broccoli_claim` 上发送认领唤醒；这会延迟工作，但不会改变宿主状态。
- 系统目录对所有角色可读，因此插件可以列出角色名和表名，但看不到其内容。

## 测试

`packages/server/src/plugin_db/tests.rs` 为每个测试启动一个全新数据库并驱动真实的连接池，因此每条规则都由 Postgres 强制执行：

| 测试 | 规则 |
| --- | --- |
| `a_plugin_has_full_ddl_and_dml_in_its_own_schema` | 自有模式：创建、修改、索引、清空、删除、外键 |
| `a_plugin_cannot_touch_another_plugins_schema` | 不能读、写或对其他插件的模式执行 DDL，不能 `SET ROLE` 为其角色 |
| `a_role_escape_lands_on_the_plugins_own_role` | `RESET ROLE` 停留在插件角色；`SET ROLE postgres` 被拒绝 |
| `a_plugin_cannot_read_the_plugin_logins` | `plugin_login_secret` 不可读 |
| `grants::every_host_relation_is_classified` | 每个宿主关系都被列为可读或隐藏 |
| `grants::a_plugin_cannot_write_any_host_relation` | 在每个宿主关系上拒绝 `INSERT`、`UPDATE`、`DELETE`、`TRUNCATE`、`ALTER`、`DROP` 和 `CREATE TRIGGER`，且不持有任何写入、`REFERENCES` 或 `TRIGGER` 权限 |
| `grants::a_plugin_cannot_use_host_sequences_or_create_in_public` | 不能使用序列，不能在 `public` 中 `CREATE`，不能新建模式 |
| `grants::a_plugin_reads_contest_data_but_no_secret` | 可读关系可以读取，机密列和隐藏关系被拒绝 |
| `concurrent_first_uses_on_several_replicas_converge` | 四个副本并发开通，每个角色一个密码 |
| `the_plugin_pools_keep_to_one_budget` | 连接池保持在预算之内；超出预算的租用会等待 |
