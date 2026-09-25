---
title: Codelink 淘汰赛
sidebar_label: Codelink 淘汰赛
sidebar_position: 3
---

# Codelink 淘汰赛

下午场淘汰赛使用 `codelink-bracket` 比赛类型。16 名选手进行 4 轮单败淘汰。每场比赛共 3 个小局，比分相同时进入附加赛；每个小局由先提交通过的一方获胜。

## 构建并启用插件

安装开发依赖后，在 Broccoli 仓库中运行以下命令。

```bash
pnpm --filter @broccoli/web-sdk build
just build-plugin plugins/codelink-bracket --install
```

构建产物包括 `plugins/codelink-bracket/codelink_bracket.wasm` 和 `plugins/codelink-bracket/frontend/dist` 下的前端包。启动服务器，或在管理界面使用**重新加载所有插件**，即可发现该插件。

在比赛编辑页面选择 `codelink-bracket` 比赛类型，并添加对阵表用到的全部题目。将 16 名选手加入比赛。然后打开该比赛的**配置**对话框，启用 `codelink-bracket` 插件的 `before_submission` 检查。未启用时，选手还可以向自己能看到、但当前并未进行的题目提交。

## 设置对阵表

每一轮需要两组各 3 道题，以及至少 1 道附加赛题目。同一道题在整个对阵表中只能出现一次。请在第一场比赛开始前，由拥有 `contest:manage` 权限的用户提交一次设置：

```bash
curl -X POST \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  "$BROCCOLI/api/v1/p/codelink-bracket/api/plugins/codelink-bracket/contests/$CONTEST/setup" \
  -d '{
    "seeds": [11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26],
    "rounds": [
      { "group_a": [101, 102, 103], "group_b": [104, 105, 106], "tiebreak": [107, 108] },
      { "group_a": [111, 112, 113], "group_b": [114, 115, 116], "tiebreak": [117] },
      { "group_a": [121, 122, 123], "group_b": [124, 125, 126], "tiebreak": [127] },
      { "group_a": [131, 132, 133], "group_b": [134, 135, 136], "tiebreak": [137] }
    ],
    "xiaoju_seconds": 1800,
    "round_intermission_seconds": 600,
    "escalation_grace_seconds": 120
  }'
```

| 字段 | 作用 |
| --- | --- |
| `seeds` | 16 个不同的用户 ID。第 1 轮中相邻的种子对阵：第 1 与第 2、第 3 与第 4，依此类推。 |
| `rounds` | 必须恰好 4 轮。每轮的每场比赛中，前一名选手使用 `group_a`，后一名选手使用 `group_b`。 |
| `xiaoju_seconds` | 每个小局的时限，必须大于 0。 |
| `round_intermission_seconds` | 一轮的最后一场比赛结束后，下一轮比赛最早可以开始前的间隔。 |
| `escalation_grace_seconds` | 小局等待卡住的评测的最长时间，超时后需由工作人员裁定。默认为 120。 |

## 进行比赛

比赛的**排名**页面显示对阵表，选择一场比赛可查看详情。**概览**页面显示比赛规则。

1. 每名选手为对手的 3 道题排定顺序，对手必须按此顺序做题。
2. 双方都提交排序后，由工作人员点击**开始比赛**。
3. 3 个小局中，双方同时做各自的下一道题，先提交通过者赢得该小局。胜负按提交时间判定，时间完全相同时按提交 ID 排序，与 [Codelink 晋级赛](./codelink-qualifier.md) 规则相同。截止时双方都未通过，则该小局无人得分。
4. 3 个小局后胜局多者晋级。比分相同（包括 0:0）时进入本轮第一道附加赛题目，双方做同一道题。附加赛无人得分时，进入下一道附加赛题目。

选手只能看到自己已进行到的题目。胜者会自动进入下一轮。

## 处理评测问题

只要更早的提交仍在评测，小局就不会判定胜负，即使已过截止时间。此时比赛显示**等待提交评测完成**，并列出正在等待的提交。评测结果出来后，小局按正常规则判定。

如果评测在 `escalation_grace_seconds` 内没有完成，比赛将变为**需工作人员裁定**。请先确认该提交的评测结果，如卡住请重新评测，再使用**判……获胜**裁定比赛。对于仍在进行中的比赛，**强制截止**会立即重新检查当前小局，例如在重新评测之后；它不会让小局在截止时间前提前结束。
