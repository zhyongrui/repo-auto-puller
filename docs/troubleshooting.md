# Troubleshooting

这份文档给已经安装好 `repo-auto-puller`、但发现“没有自动拉到最新代码”的用户或代理。

## 先看哪三样

先同时看：

- `repo-auto-puller --config <config> status --repo <name>`
- `repo-auto-puller --config <config> doctor --repo <name>`
- 日志文件和状态文件

如果你已经开了本地状态页，也可以先看：

- `repo-auto-puller --config <config> dashboard --listen 127.0.0.1:8787`

## 常见情况

### 1. 工作区是脏的

表现：

- `status` 里 `dirty: true`
- message 类似 “working tree has uncommitted changes”

含义：

- 工具不会替你处理未提交改动
- 自动拉取会被安全策略跳过

### 2. 本地和远端分叉

表现：

- `decision` 是 `diverged`
- message 里会明确说本地和远端都各自有提交

含义：

- 工具只做安全 fast-forward，不会自动 merge 或 rebase

### 3. 本地领先远端

表现：

- `decision` 是 `ahead-only`

含义：

- 这是保护行为，不是故障
- 先确认这些提交是不是你本地还没推上去

### 4. 远端 fetch 失败

表现：

- `status.json` 或 `history.jsonl` 里有 `ERROR`
- 日志里出现 TLS、认证、网络超时、权限失败等信息

含义：

- 问题在远端访问链路，不在自动拉取决策本身

### 5. 服务安装了，但没有持续运行

Linux：

- `systemctl --user status <service>.service`

macOS：

- `launchctl print gui/$(id -u)/<service>`

Windows：

- `schtasks /Query /TN <service>`

## 平台注意事项

### Linux

- 如果你是手动替换二进制，先停掉用户服务再覆盖，避免 `Text file busy`

### macOS

- `launchd` plist 已存在但未成功加载时，优先重新执行 `install-service`

### Windows

- 后台任务由 Task Scheduler 托管
- 启动脚本默认写到 `%APPDATA%\\repo-auto-puller\\<service>.cmd`
- 可以手动触发一次：

```powershell
schtasks /Run /TN repo-auto-puller
```

## 建议排障顺序

1. 先确认远端 `fetch` 能不能成功
2. 再确认当前仓库是不是 `dirty` / `diverged` / `ahead-only`
3. 再确认后台服务是否真的在跑
4. 最后再看配置文件里的 `paused`、`allowed_branches`、`quiet_hours`
