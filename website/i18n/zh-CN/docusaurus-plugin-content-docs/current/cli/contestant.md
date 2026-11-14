---
title: 参赛者命令行工具
sidebar_label: 参赛者命令行工具
sidebar_position: 1
---

# 参赛者命令行工具

`broccoli` 是面向参赛者的命令行工具。你可以在终端中登录、阅读题目、测试并提交解答、提出疑问，并实时观看比赛的进行。它是单个文件，无需在其周围安装任何其他东西，并且在 Linux、macOS 和 Windows 上的行为完全一致。

## 安装

从[下载页面](../downloads.md)下载适合你系统的构建，然后让它可执行。

在 macOS 和 Linux 上，将文件标记为可执行，并将其移动到你的可执行路径中，命名为 `broccoli`。

```bash
chmod +x broccoli-cli-linux-x86_64
mv broccoli-cli-linux-x86_64 /usr/local/bin/broccoli
```

在 Windows 上，将文件重命名为 `broccoli.exe`，并放在便于查找的位置。你需要从终端运行它，双击不会有任何用处。确认它能运行。

```bash
broccoli --version
```

### 从源码构建

如果没有适合你系统的构建，或者你想要最新的代码，可以使用 Rust 来构建。

```bash
git clone https://github.com/THUSAAC-PSD/broccoli
cargo install --path broccoli/packages/contestant-cli
```

这会把同一个 `broccoli` 命令安装到你的 Cargo bin 目录中。

### 启用 Tab 补全

为你的 shell 生成补全脚本并加载它。Broccoli 支持 bash、zsh、fish、PowerShell 和 elvish。

```bash
broccoli completions zsh > ~/.broccoli-completions.zsh
echo 'source ~/.broccoli-completions.zsh' >> ~/.zshrc
```

## 登录

将命令行工具指向你的比赛服务器。服务器地址由比赛的组织者提供。

```bash
broccoli login --server https://judge.example.com
```

这会打开浏览器进行授权，随后让你在后续命令中保持登录状态。你的登录信息会保存在本机，因此每个服务器只需登录一次。如果不带 `--server`，Broccoli 会连接位于 `http://localhost:3000` 的本地服务器，所以请传入你比赛的地址。你可以随时确认自己的身份。

```bash
broccoli whoami
```

在没有浏览器的机器上，例如你通过 SSH 连接的机器，Broccoli 会改为打印一个链接。在任意设备上打开它，完成授权，然后把令牌粘贴回来。你也可以直接要求使用这种方式。

```bash
broccoli login --server https://judge.example.com --no-browser
```

如果你的比赛使用用户名和密码登录，而不是浏览器流程，传入它们，Broccoli 就会跳过浏览器。

```bash
broccoli login --server https://judge.example.com -u alice -p secret
```

## 找到你的比赛

列出你能看到的比赛。

```bash
broccoli contest list
```

每场比赛都有一个编号和一个名称，在任何需要指定比赛的地方，两者都可以使用。打开其中一场，查看它的题目、时间安排，以及你是否已报名。

```bash
broccoli contest info "Spring Round"
```

如果某场比赛需要报名，请在提交之前先报名。退出比赛的方式相同，使用 `broccoli contest unregister`。

```bash
broccoli contest register "Spring Round"
```

## 阅读题目

列出一场比赛中的题目。

```bash
broccoli contest problems "Spring Round"
```

每道题都有一个标号（例如 `A`）、一个编号和一个标题，其中任意一个都能指代该题。下载某道题，会把它的题面和样例保存为当前目录中的文件，方便离线阅读并据此测试。

```bash
broccoli contest problems "Spring Round" -p A
```

## 提交前先测试

先用样例测试你的解答。指向你的源文件和题目。

```bash
broccoli test sol.cpp -c "Spring Round" -p A
```

Broccoli 会根据文件扩展名识别语言，获取样例，逐个运行，并告诉你哪些通过了，对未通过的给出差异对比。加上 `--local` 可以在你自己的机器上运行而不是服务器上，这样更快，并且在你下载题目之后可以离线进行。

```bash
broccoli test sol.cpp -c "Spring Round" -p A --local
```

使用 `-i` 尝试你自己的输入，它会运行一次该文件并打印其产生的输出。

```bash
broccoli test sol.cpp --local -i input.txt
```

## 提交

把你的解答发送到某道题。

```bash
broccoli submit sol.cpp -c "Spring Round" -p A
```

Broccoli 会识别语言，完成提交，并打印一个提交编号。需要时用 `-l` 覆盖语言。加上 `-w` 可以当场等待评测结果，而不必之后再查看。

```bash
broccoli submit sol.cpp -c "Spring Round" -p A -w
```

在某个目录中首次提交之后，Broccoli 会把比赛、题目和语言记录在一个小小的 `.broccoli` 文件里，于是你只用文件名就能重复提交。

```bash
broccoli submit sol.cpp
```

对于不属于任何比赛的独立题目，使用 `--no-contest` 和该题自身的 id 来提交。

```bash
broccoli submit sol.cpp --no-contest -p 42
```

## 查看提交

查看某次提交的结果。不带编号时，Broccoli 让你从最近的提交中选择。

```bash
broccoli status
```

传入编号可以直接跳到它。

```bash
broccoli status 12345
```

使用 `--recent` 把你最近的提交以纯文本表格打印出来。从最近提交中选择需要知道是哪场比赛，Broccoli 会从你的 `.broccoli` 文件或你保存的默认值中读取。

```bash
broccoli status --recent
```

## 实时观看比赛

为一场比赛打开实时面板。

```bash
broccoli watch "Spring Round"
```

它会自动刷新，包含三个标签页，分别是你的提交、题目和疑问，并带有结束倒计时。打开任意一行查看详情，在分页器中阅读题目，并且无需离开界面即可提出疑问。

| 按键               | 作用                       |
| ------------------ | -------------------------- |
| `Tab`、`←`、`→`    | 在标签页之间切换           |
| `1` `2` `3`        | 跳转到提交、题目、疑问      |
| `↑` `↓`、`j` `k`   | 移动选择                   |
| `Enter`            | 打开选中的一行             |
| `o`                | 在分页器中打开题目         |
| `a`                | 提出疑问                   |
| `r`                | 立即刷新                   |
| `q`、`Esc`         | 关闭面板，或退出           |

## 提出疑问

就某场比赛向组织者提问。省略问题内容则进入交互式输入。

```bash
broccoli clarifications ask "Spring Round" "Is the input always sorted?"
```

用列表查看回答以及任何公告。

```bash
broccoli clarifications list "Spring Round"
```

## 设置你的默认值

大多数命令通过 `-c` 指定比赛，通过 `-p` 指定题目。你可以用两种方式避免重复输入。

工作目录中的 `.broccoli` 文件会为你在该目录下运行的所有命令设定比赛、题目和语言。Broccoli 会在你首次提交时为你写入一个，你也可以手动编辑它。

```toml
contest  = "Spring Round"
problem  = "A"
language = "cpp"
```

保存的默认值则在任何地方生效。设置一次比赛，之后就可以省略 `-c`。可设置的键是 `contest`、`language` 和 `server`，用 `broccoli config unset` 移除其中一个。

```bash
broccoli config set contest "Spring Round"
```

用不带参数的命令查看 Broccoli 保存的全部内容，包括文件所在的位置。

```bash
broccoli config
```

两个环境变量可以为单次运行覆盖保存的值，这在脚本中很有用。`BROCCOLI_URL` 设置服务器，`BROCCOLI_TOKEN` 设置登录令牌。你的登录信息和设置保存在你账户的配置目录中，旁边还有你下载的题目缓存。运行 `broccoli config` 查看确切路径。

## 语言

Broccoli 根据你的文件扩展名，在服务器提供的语言中识别语言。想用其他语言时，设置 `-l` 或你的 `language` 默认值。装有内置 standard-languages 插件的服务器提供下表中的语言，一场比赛接受其中哪些，由组织者决定。

| 扩展名                     | 语言         |
| -------------------------- | ------------ |
| `.c`                       | `c`          |
| `.cpp` `.cc` `.cxx` `.c++` | `cpp`        |
| `.py`                      | `python3`    |
| `.java`                    | `java`       |
| `.kt`                      | `kotlin`     |
| `.rs`                      | `rust`       |
| `.go`                      | `go`         |
| `.pas` `.pp`               | `pascal`     |
| `.js`                      | `javascript` |

## 全部命令

运行 `broccoli --help`，或在任意命令后加上 `--help`，即可查看每个选项。每个命令还有一个简短的别名。

| 命令                          | 别名      | 作用                       |
| ----------------------------- | --------- | -------------------------- |
| `broccoli login`              | `li`      | 登录到比赛服务器           |
| `broccoli whoami`             | `me`      | 显示你当前的登录身份       |
| `broccoli contest list`       | `c ls`    | 列出比赛                   |
| `broccoli contest info`       | `c i`     | 比赛详情与你的状态         |
| `broccoli contest register`   | `c reg`   | 报名一场比赛               |
| `broccoli contest unregister` | `c unreg` | 退出一场比赛               |
| `broccoli contest problems`   | `c p`     | 列出或下载题目             |
| `broccoli test`               | `t`       | 运行样例                   |
| `broccoli submit`             | `s`       | 提交解答                   |
| `broccoli status`             | `st`      | 查看某次提交               |
| `broccoli watch`              | `w`       | 实时比赛面板               |
| `broccoli clarifications list`| `clar ls` | 阅读疑问                   |
| `broccoli clarifications ask` | `clar a`  | 提出疑问                   |
| `broccoli config`             | `cfg`     | 显示或修改你的默认值       |
| `broccoli completions`        |           | 打印 shell 补全脚本        |
| `broccoli prewarm`            |           | 预热缓存以加快首次命令     |
