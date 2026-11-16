---
title: 快速开始
sidebar_label: 快速开始
sidebar_position: 1
---

# 快速开始

Broccoli 插件是一个运行在 Broccoli 服务端内部的 WebAssembly 模块，并可附带一个运行在
网页前端中的 React 包。后端模块通过宿主函数访问平台的数据库、配置与日志能力，这些能力
受其所声明的权限约束。本页将完整构建一个插件，并以随仓库发布的 `cooldown` 插件作为示例。

## 运行约束

后端代码以沙箱化的 WASM 模块形式运行。它不会打开网络套接字，也不会访问文件系统。它通过
声明的宿主函数与宿主通信，其 HTTP 处理函数返回 JSON 值，而非文件或数据流。因此，插件负责
协调与存储数据，而任何二进制内容（例如已渲染的 PDF）都必须放在独立的原生客户端中。提前
了解这一点，可以避免逆着平台的设计进行开发。

插件可以添加两类行为：

- 后端路由与钩子，使用 Rust 编写并编译为 WASM。
- 前端组件，使用 React 编写并挂载到具名的 UI 插槽中。

## 前置条件

- 带有 `wasm32-wasip1` 目标的 Rust 工具链。单独创建的插件会得到一个
  `rust-toolchain.toml`，选用带该目标的 stable Rust。在已固定工具链的项目（例如
  Broccoli 仓库）中创建的插件则使用该项目固定的工具链。
- Broccoli CLI。其 crate 名为 `broccoli-dev-cli`，安装后的可执行文件名为 `broccoli-dev`。
- 若插件包含前端，则需要 `pnpm`。
- 一个用于上传的运行中的 Broccoli 服务端。

安装 CLI：

```bash
cargo install broccoli-dev-cli
```

## 生成插件骨架

```bash
broccoli-dev plugin new my-plugin --full
```

使用 `--backend`、`--frontend` 或 `--full` 选择要生成的内容。若不带这些标志，命令会进行
询问。骨架默认生成在 `./my-plugin`，也可通过 `-o <DIR>` 写入其他位置。

## 结构

后端插件由三个文件定义。

### plugin.toml

清单文件。它为插件命名，声明后端可执行的操作，并将事件与路由映射到导出的函数。以下是
`cooldown` 的清单，仅保留其后端部分：

```toml
name = "cooldown"
version = "0.1.0"
description = "Submission cooldown timer"

[server]
entry = "cooldown_plugin.wasm"
permissions = ["logger", "sql", "config:read"]

[[server.hooks]]
topic = "before_submission"
function = "check_cooldown"
scope = "resource"

[[server.routes]]
method = "GET"
path = "/api/plugins/cooldown/problems/{problem_id}/status"
handler = "get_cooldown_status_standalone"
```

- `entry` 是构建产物的 WASM 文件名。
- `permissions` 控制宿主访问权限。没有 `sql` 时，数据库宿主函数不可用，其余权限同理。
- 有 `sql` 时，插件的表落在它自己的模式中，宿主表对它只读。选手源代码、测试数据、程序输出和消息正文默认不可读，除非 `[server.sql] read` 列出这些列，例如 `read = ["submission.files"]`；管理员在安装插件时会看到它们。参见[插件数据库访问](../internals/plugin-database.md)。
- 钩子将一个函数订阅到平台事件，此处为 `before_submission`。
  `scope = "resource"`（默认值）表示钩子只在插件适用的范围内运行，即设置中启用了该插件的比赛或题目，以及该插件自身注册的比赛类型下的所有比赛。`scope = "global"` 表示对服务器上的所有提交运行。
- 路由将一个 HTTP 方法与路径映射到导出的函数。`{problem_id}` 这类路径参数在处理函数内
  读取。
- 可选的 `[bundle]` 段落指定发布包如何提供该插件。`mode = "default"`（默认值）表示随发布包提供并默认加载；`mode = "optional"` 表示放在 `plugins-available/` 中，由运维人员通过 `enable-plugin.sh` 启用，适用于仅部分赛事需要的插件；`mode = "none"` 表示不随发布包提供，用于测试插件。

### Cargo.toml

```toml
[package]
name = "cooldown-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
broccoli-server-sdk = { path = "../../packages/server-sdk", features = ["guest"] }
extism-pdk = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

`cdylib` crate 类型用于产出 WASM 模块。`broccoli-server-sdk` 上的 `guest` 特性会启用
代码所调用的宿主函数封装。

### src/lib.rs

导出函数使用 `#[plugin_fn]` 标注。路由处理函数以字符串形式接收请求，并返回 JSON 字符串。
`run_api_handler` 会解码请求，并向你提供类型化的 `Host` 与 `PluginHttpRequest`：

```rust
use broccoli_server_sdk::prelude::*;
use extism_pdk::{plugin_fn, FnResult};

#[plugin_fn]
pub fn get_cooldown_status_standalone(input: String) -> FnResult<String> {
    run_api_handler(&input, handle_status)
}

fn handle_status(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let user_id = req
        .require_user_id()
        .map_err(|_| PluginHttpResponse::error(401, "Authentication required"))?;
    let problem_id: i32 = req.param("problem_id")?;

    let eff = host.config.get_effective("cooldown", problem_id, None)?;

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({
            "enabled": eff.is_enabled,
            "user_id": user_id,
        })),
    })
}
```

响应的 `body` 是一个 `serde_json` 值。这就是 JSON 唯一约束在实践中的体现。

钩子返回一个简短的 JSON 决策，而非 HTTP 响应。`cooldown` 钩子读取自身配置，通过
`host.db` 检查上次提交时间，并返回 `{"action": "pass"}` 或一个拒绝结果：

```rust
#[plugin_fn]
pub fn check_cooldown(input: String) -> FnResult<String> {
    let host = Host::new();
    let event: BeforeSubmissionEvent = serde_json::from_str(&input)?;

    let eff = host
        .config
        .get_effective("cooldown", event.problem_id, event.contest_id)?;
    if !eff.is_enabled {
        return Ok(serde_json::to_string(&serde_json::json!({"action": "pass"}))?);
    }

    // ... query host.db for seconds since the last submission ...

    Ok(serde_json::to_string(&serde_json::json!({"action": "pass"}))?)
}
```

## 构建与安装

```bash
broccoli-dev plugin build my-plugin --install
```

由于清单包含 `[server]` 段，CLI 会运行 `cargo build --target wasm32-wasip1`，并将产物
复制到 `entry` 指定的路径。`--install` 会把构建结果放到本地服务端加载的位置。添加
`--release` 可得到优化后的模块。若清单包含 `[web]` 段，CLI 还会运行前端构建。

## 无网络时构建

发布包的 `sdk/` 目录附带了 SDK：`server-sdk/`、`broccoli-types/` 和已构建的 `web-sdk/`，布局与源码仓库的 `packages/` 相同；另有 `vendor/`，包含生成的后端所需的全部 crates.io crate。让脚手架指向它，再让 cargo 从 `vendor/` 读取 crate：

```bash
broccoli-dev plugin new my-plugin --sdk <bundle>/sdk
mkdir -p my-plugin/.cargo
sed 's|^directory = "vendor"|directory = "<bundle>/sdk/vendor"|' \
  <bundle>/sdk/.cargo/config.toml > my-plugin/.cargo/config.toml
CARGO_NET_OFFLINE=true broccoli-dev plugin build my-plugin --install
```

`<bundle>` 须写为绝对路径。工具链需已安装 `wasm32-wasip1` 目标。前端的构建工具（`tsdown`、`tailwindcss`、`typescript`）仍来自 npm，因此请在能访问 npm 源或局域网镜像的机器上构建前端。

## 对接运行中的服务端进行迭代

先登录一次，然后监视目录。CLI 会在每次变更时重新构建并上传新的包：

```bash
broccoli-dev login --server http://localhost:3000
broccoli-dev plugin watch my-plugin --server http://localhost:3000
```

`broccoli-dev login` 会将凭据存储在 `~/.config/broccoli/credentials.json`。你可以用
`BROCCOLI_URL` 与 `BROCCOLI_TOKEN`，或用 `--server` 与 `--token` 覆盖它们。

## 后续方向

- 宿主能力位于 `host` 之下。使用 `host.db` 配合 `Params` 构造器进行参数化 SQL，使用
  `host.config.get_effective` 获取分层配置，使用 `host.log` 记录日志。
- 在 `plugin.toml` 中声明 `[config.<plugin>]` 段，即可在管理界面获得一个设置表单，其作用
  域可以是题目、比赛或比赛题目。
- 使用 web SDK 将前端组件挂载到具名插槽中。参见完整插件清单中的 `[web]` 与
  `[[web.slots]]` 段。
