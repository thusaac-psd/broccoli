---
title: 数据库迁移
sidebar_label: 数据库迁移
sidebar_position: 9
---

# 数据库迁移

服务器以 `packages/server/src/migration` 中一组有序的前向迁移来管理数据库结构。worker 和存储层读写它们的表（`task_cache`、`blob_data`），但从不创建或修改这些表。

## 启动时做什么

每次服务器启动都会打开一个会话，获取启动锁（`packages/server/src/database.rs` 中的咨询锁 `BOOT_LOCK_KEY`），然后调用 `BootSession::migrate`：

1. `check_applied_versions` 读取 `seaql_migrations`。空数据库直接通过。含有表却没有 `seaql_migrations` 的数据库会被拒绝；记录了本构建未定义的迁移的数据库同样会被拒绝，错误信息会写明该迁移和本构建最新的迁移。两种拒绝发生前都不会修改任何内容。
2. `Migrator::up` 按从旧到新的顺序应用待执行的迁移，每个迁移在各自的事务中执行，并逐一记录到 `seaql_migrations`。

同时启动的服务器在锁上排队。第一个应用待执行的迁移，其余的随后发现已无待执行项。初始数据的写入随后在同一把锁下进行。

## 基线

`m0001_baseline` 创建完整的数据库结构。`m0001_baseline.sql` 包含全部表、索引、约束、序列、触发器函数和触发器。Rust 部分补充依赖于集群的内容：可选的 `pg_stat_statements` 扩展，以及数据库的插件组角色及其授权（见[插件数据库访问](plugin-database.md)）。

## 修改数据库结构

数据库结构的修改是一个新的迁移，绝不改动已发布的迁移：

1. 新增 `packages/server/src/migration/mNNNN_<what>.rs`，以前向 DDL 实现 `MigrationTrait`，并将其追加到 `Migrator::migrations`。
2. 修改实体（以及代码中维护的触发器定义）使之一致。
3. 运行结构测试。

迁移不实现 `down`：回滚即恢复升级前所做的备份。

## 结构测试

`migration::tests::the_migrations_create_the_schema_the_code_expects` 在空数据库上执行迁移，并将结果与代码的预期比较：

- 它先为系统目录拍下快照（列及其类型、可空性和默认值、约束、索引、触发器、函数、序列），然后执行实体结构同步，创建 `blob_data` 和 `task_cache` 的实体表，并执行代码中维护的触发器定义（`judging::notify::expected` 与 `plugin_config_cache::expected`）。系统目录的任何变化都会使测试失败。
- 同步从不修改已存在的列，因此它还会仅凭实体建立第二个数据库，逐列比较实体列的类型和可空性。

`the_claim_indexes_match_the_predicates_the_queue_scans_use` 检查每个用于认领的部分索引的谓词与队列扫描中内联的谓词一致，使查询规划器始终能使用它。

```bash
cargo test -p server --lib migration::
```

## 滚动升级

只要每个新迁移都是增量的（新增表、新增可空或带默认值的列、新增索引），较旧的服务器就能继续在已被较新构建迁移的数据库上工作。其他修改（重命名、删除、改变类型、不带默认值的新 `NOT NULL`、更严格的约束）都要分两个版本按“扩展/收缩”进行：第一个版本加入新结构并继续写入旧结构；第二个版本在不再有服务器运行旧版本后删除旧结构。数据库已领先后重启的旧服务器会拒绝启动，因此滚动升级一旦开始就应完成。运维步骤见 `release/docs/upgrade.md`。
