---
title: 插件数据库访问
sidebar_label: 插件数据库访问
sidebar_position: 6
---

# 插件数据库访问

拥有 `sql` 权限的插件通过 `host.db.*`（`packages/server/src/host_funcs/sql.rs`）执行 SQL。这些 SQL 能做什么由 Postgres 决定：每个插件以自己的角色登录，拥有一个模式，并可读取一组经过挑选的宿主列。宿主状态只通过宿主函数改变，宿主函数带有宿主所依赖的保护（评测纪元栅栏与已定稿保护、死信队列归属、`plugin_config` 变更计数器、账户与令牌）。

## 插件可以做什么

| 对象 | 权限 |
| --- | --- |
| 自己的模式 `plugin_<id>` | 所有者：完整的 DDL 与 DML、`TRUNCATE`、自有表之间的外键 |
| `public` 中的比赛数据（m0001 中的 `PLUGIN_READABLE`） | 除内容列外每一列上的 `SELECT` |
| 清单声明的内容列 | 该列上的 `SELECT` |
| `"user"` | `id`、`username`、`display_name`、`created_at`、`deleted_at` 列上的 `SELECT` |
| 其他所有宿主关系 | 无 |
| 宿主序列、模式 `public` | 无；`public` 上的 `CREATE` 也已从 `PUBLIC` 收回 |
| 其他插件的模式 | 无 |

可读关系是不含机密的比赛数据：比赛、题目、测试点、提交、评测、结果、代码运行、答疑和题目文件。不可读的有：密码哈希与凭据时间戳、刷新令牌、设备码、幂等键及其响应体、插件登录密钥、blob 存储、角色与权限、插件注册表、每个插件由宿主保管的状态（`plugin_config`、`plugin_storage`、`plugin_timer`、`plugin_replica`、`submission_feedback`）、死信队列，以及模式戳和迁移记录。

对宿主状态的写入经由宿主函数完成：评测与结果用 `host.submission.*` 和 `host.code_run.*`，此外还有 `host.storage.*`、`config:read`/`config:write`、`host.timer.*` 以及提交反馈函数。随仓库发布的插件都不直接写宿主表。

## 内容列与读取范围

`packages/broccoli-types/src/manifest.rs` 中的 `SQL_READ_SCOPES` 以 `table.column` 形式列出宿主的内容列：选手源代码（`submission.files`、`code_run.files`）、编译器与程序输出、测试数据（`test_case.input`、`expected_output` 及其预览和 blob 哈希）、检查器输出、自定义测试、题面和答疑正文。默认情况下没有插件能读取它们。评测器通过在宿主函数连接池上运行的 `host.submission.query_test_cases` 获取测试数据。

需要读取某一列的插件需要声明它：

```toml
[server]
permissions = ["sql"]

[server.sql]
read = ["submission.files"]
```

范围不在列表中、`[server.sql]` 下出现未知键，或声明了 `read` 却没有 `sql` 权限时，清单解析失败。上传对话框和插件详情视图在权限旁边显示所声明的范围，管理员随插件一并批准。开通时会从插件角色收回所有内容列，再恰好授予所声明的列；新版本声明了不同的列时会再次执行。打印插件声明了 `submission.files`，用于打印提交的源代码。

## 角色与模式

`packages/server/src/plugin_db/schema.rs` 从插件 id 和数据库推导出所有名称，因此宿主不持有任何针对单个插件的知识：

- 模式为 `plugin_<id>`；
- 数据库的组角色为 `broccoli_<db>_plugin`，每个插件的登录角色为 `broccoli_<db>_plugin_<id>`，其中 `<db>` 是数据库名 sha256 的前 8 个十六进制位，因为同一集群的所有数据库共享角色；
- 超出 Postgres 63 字节标识符上限的 id 保留开头部分，并以整个 id 的摘要结尾。

迁移 m0001 创建组角色，并把它的名字记录在 `plugin_group_role` 中，因此从模板克隆的数据库保留其授权所指向的组。每个插件角色为 `LOGIN INHERIT`，不带其他属性，是组的成员，并拥有自己的模式。组持有 `public` 上的 `USAGE` 和默认的 `SELECT` 授权，除此之外别无他物。角色的 `search_path` 是其模式加上应用自身的路径，因此 `CREATE TABLE job (...)` 落在 `plugin_<id>` 中，而 `FROM submission` 找到的是宿主表。服务端 SDK 中的 `Db::ensure_schema` 因此无需插件做任何改动，就会在插件自己的模式中创建对象。

没有设置默认权限，因此宿主日后新增的表在被列入之前对插件不可读；对于既未列入可读也未列为隐藏的宿主关系，`plugin_db::grants::every_host_relation_is_classified` 会失败。

## 开通

`schema::provision` 在插件首次于某台服务器上执行 SQL 时运行，在宿主函数连接池上以应用角色在一个事务中完成：

1. 为该角色向 `plugin_login_secret` 插入随机密码，已有则保留，并锁定该行；
2. 角色不存在时创建它，然后设置 `LOGIN INHERIT NOCREATEROLE` 和已存储的密码（角色只能指定自己持有的属性，因此应用角色不指定其他属性；`CREATE ROLE` 默认不带这些属性）；
3. 将组授予该角色，并将该角色授予应用角色，以便应用把模式交给它；
4. 以该角色执行 `CREATE SCHEMA IF NOT EXISTS ... AUTHORIZATION`；
5. 设置该角色的 `search_path`；
6. 收回所有内容列，再授予所声明的列。

行锁使开通同一插件的多个副本串行执行，且任何插件都无法获取它：`plugin_login_secret` 不向插件授予任何权限。每个副本读回首个写入者的密码，因此它们都以同一密码连接。这些 DDL 属于模式戳的输入（`packages/server/build.rs` 中的 `SCHEMA_SOURCES`）。

应用角色需要 `LOGIN` 和 `CREATEROLE`，拥有数据库，且不是超级用户；运维配置参考中写明了这一点，每个安装程序也都这样创建它。缺少 `CREATEROLE` 时，m0001 会跳过组角色并记录警告。开通随之失败：插件中执行 SQL 的调用以“暂不可用”失败，调用方以 503 应答；失败只以 error 级别记录一次，下一次尝试要等待 30 秒（`PROVISION_BACKOFF`）。

从集群移除的插件，其角色被设为 `NOLOGIN`，其会话被终止（`schema::retire`）。它的模式和数据保留：插件 id 就是插件的身份，因此日后以同一 id 安装的就是同一个插件，开通会让其角色再次登录。每个副本在卸载插件时丢弃该插件的连接池，因此下一次加载会再次开通。

## 连接池

`PluginSqlPools`（`packages/server/src/plugin_db/mod.rs`）在每个插件首次执行 SQL 时为其建立一个连接池。池中的连接以插件的角色认证，因此一次调用无需付出 `SET ROLE` 或 `SET search_path` 的代价，`RESET ROLE` 也会落回插件自己的角色。`SET ROLE` 到组只会失去插件自己的模式。

所有插件连接池共享一个预算 `PoolSizes::plugin`（每两个插件 CPU 令牌一个，2 到 8；承载评测写入的宿主函数连接池为每个令牌一个，4 到 16）：

- 每次租用占用 `budget` 个许可之一；同时执行 SQL 的插件调用至多 `budget` 个；
- 已经持有许可的调用（通过打开的事务，或其自身或其所嵌套调用的租用）在后续每条语句和事务中复用该许可，因此调用永远不会等待自己持有的许可（`plugin_core::executor::CallLocals` 在调用树中传递它）；
- 当各池已持有 `budget` 个连接而某插件没有空闲连接时，它先关闭另一个插件的一个空闲连接；
- 在各池持有超过 `budget` 个连接时被释放的连接会被关闭而不是保留；
- 返回连接池的连接会释放其会话级咨询锁、停止其 `LISTEN`，并重置其会话设置（`SESSION_RESET`），因此对宿主启动锁键的锁定或角色切换都随调用结束。

`host.db.*` 以外的宿主函数在宿主函数连接池（`packages/server/src/database.rs` 中的 `connect_host_fn_pool`）上以应用角色运行，执行宿主构建的语句。插件编写的 SQL 从不在那里运行。

## SQL 防护

`host_funcs/sql.rs` 中的 `reject_session_escape` 拒绝角色变更、`set_config`、`ALTER ROLE` 与 `ALTER USER`、`DISCARD`、`DO`、例程定义，以及对宿主强制设置的会话级修改。角色变更由引擎约束；该防护阻止插件修改自身角色的密码或默认设置，并防止插件让其连接脱离 `statement_timeout` 的限制。

## 边界不覆盖的部分

- 咨询锁、`LISTEN` 和 `NOTIFY` 对所有角色开放。在一次调用之内，插件可以占用宿主使用的锁键，或在 `broccoli_claim` 上发送认领唤醒；这会延迟工作，但不会改变宿主状态，且会话锁在连接返回连接池时结束。
- 大对象：任何角色都可以创建，因此插件可以用 `lo_from_bytea` 在其模式之外存储数据。收回 `lo_*` 函数需要拥有它们的超级用户。插件无法读取其他角色的大对象。
- 系统目录对所有角色可读，因此插件可以列出角色名和表名，但看不到其内容。

## 测试

`packages/server/tests/integration/plugin_db.rs` 中的每个测试都在 test-support 的共享容器上，于一个归非超级用户 `LOGIN CREATEROLE` 应用角色所有的空数据库中，通过真实的迁移和真实的连接池运行，因此每条规则都由 Postgres 强制执行。集成测试和端到端测试框架同样以该角色登录。

| 测试 | 规则 |
| --- | --- |
| `a_plugin_has_full_ddl_and_dml_in_its_own_schema` | 自有模式：创建、修改、索引、清空、删除、外键 |
| `a_plugin_cannot_touch_another_plugins_schema` | 不能读、写或对其他插件的模式执行 DDL，不能 `SET ROLE` 为其角色 |
| `a_role_escape_lands_on_the_plugins_own_role` | `RESET ROLE` 停留在插件角色；`SET ROLE postgres` 被拒绝；降级随调用结束 |
| `a_plugin_cannot_read_the_plugin_logins` | `plugin_login_secret` 不可读 |
| `grants::every_host_relation_is_classified` | 每个宿主关系都被列为可读或隐藏 |
| `grants::a_plugin_cannot_write_any_host_relation` | 在每个宿主关系上拒绝 `INSERT`、`UPDATE`、`DELETE`、`TRUNCATE`、`ALTER`、`DROP` 和 `CREATE TRIGGER`，且不持有任何写入、`REFERENCES` 或 `TRIGGER` 权限 |
| `grants::a_plugin_cannot_use_host_sequences_or_create_in_public` | 不能使用序列，不能在 `public` 中 `CREATE`，不能新建模式 |
| `grants::a_plugin_reads_contest_data_but_no_secret` | 可读列可以读取；内容列、机密列和隐藏关系被拒绝 |
| `grants::a_declared_scope_grants_exactly_its_columns` | 声明的范围只向该插件授予其列；撤销的范围会被收回 |
| `concurrent_first_uses_on_several_replicas_converge` | 四个副本并发开通，每个角色一个密码 |
| `the_plugin_pools_keep_to_one_budget` | 超出预算的租用会等待 |
| `a_call_reuses_its_permit_at_a_full_budget` | 调用的语句和嵌套调用复用其许可；其他调用等待 |
| `failed_provisioning_backs_off` | 开通失败后，在退避期结束前不会重试 |
| `a_returned_connection_drops_its_session_state` | 连接返回时，会话锁、监听和设置随之结束 |
| `a_removed_plugin_cannot_log_in_until_it_is_installed_again` | 退役后设为 `NOLOGIN`；重新安装后可以登录并保留数据 |

`plugin_core::manifest` 中的测试在解析时拒绝未知范围以及缺少 `sql` 权限的范围。
