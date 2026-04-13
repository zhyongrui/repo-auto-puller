# Manual Install

这份文档是给最终用户看的。

## 你需不需要安装 Rust

一般不需要。

正常使用 `repo-auto-puller` 时，推荐直接使用预编译二进制。只有在以下情况才需要 Rust：

- 你想自己从源码构建
- 还没有适合你平台的发布包
- 你准备参与开发

当前预编译发布包覆盖：

- Linux `x86_64`
- Linux `aarch64`
- macOS `x86_64`
- macOS `aarch64`

## 推荐安装方式

### 方式 A：用预编译二进制

1. 打开 GitHub Releases 页面
2. 下载适合你系统和架构的压缩包
3. 解压出 `repo-auto-puller`
4. 放到 `~/.local/bin/repo-auto-puller`

Linux 和 macOS 用户都可以直接使用安装脚本：

```bash
curl -fsSL https://raw.githubusercontent.com/zhyongrui/repo-auto-puller/main/scripts/install.sh | bash
```

安装脚本会：

- 下载预编译二进制
- 放到 `~/.local/bin/repo-auto-puller`
- 写一个默认配置文件
- 提示你用 `install-service` 安装对应平台的后台服务
- 如果 `curl` 因网络/TLS 抖动失败，且系统已安装 `gh`，会自动回退到 GitHub CLI 下载

## 配置

把配置文件写到：

```bash
~/.config/repo-auto-puller/config.toml
```

最小示例：

```toml
[defaults]
log_file = "~/.local/state/repo-auto-puller/repo-auto-puller.log"
verbose = false

[[repositories]]
name = "my-repo"
path = "/path/to/your/repo"
interval_seconds = 60
enabled = true
paused = false
dry_run = false
allowed_branches = ["main"]
```

如果你想先只做检测、不真的拉取，把 `dry_run` 改成 `true`。

如果你只想允许某些分支自动拉取，可以把 `allowed_branches` 改成你允许的分支列表；留空则表示不限制分支。

如果你想临时暂停某个仓库的自动拉取，但保留它在配置和状态输出里，把 `paused` 改成 `true`。

如果你想在真正执行自动拉取之前或之后触发命令，可以使用：

```toml
[defaults]
before_pull_command = "echo before pull"
after_pull_command = "echo after pull"
```

这两个 hook 只会在真实自动拉取时触发；`dry_run` 或被保护策略跳过时不会执行。

如果你想在固定时段内暂停自动拉取，可以加：

```toml
quiet_hours = { start = "23:00", end = "07:00" }
```

它按本机本地时间生效，并支持跨午夜窗口。

你也可以直接让工具帮你生成配置，而不是手写：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml init --repo-path /path/to/your/repo
```

例如：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml init \
  --repo-path /path/to/your/repo \
  --name my-repo \
  --interval 60
```

你也可以在配置里加失败告警钩子：

```toml
[defaults]
on_failure_command = "notify-send 'repo-auto-puller' \"$REPO_AUTO_PULLER_REPO_NAME: $REPO_AUTO_PULLER_ERROR\""
```

默认还会把每个仓库最近一次同步结果写到：

```bash
~/.local/state/repo-auto-puller/status.json
```

## 启动方式

### 前台运行

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml --repo my-repo
```

### 只检查一次

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml --repo my-repo --once
```

### 查看当前状态

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml status --repo my-repo
```

如果你想让脚本或其他工具读取状态，可以加上 `--json`：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml status --repo my-repo --json
```

### 运行诊断

安装好配置和服务后，推荐再跑一遍：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml doctor --repo my-repo
```

它会检查：

- 配置文件是否存在且可解析
- 后台服务定义是否存在、是否处于运行状态
- 仓库 fetch 和同步决策是否正常

如果你安装时用了自定义服务名，也要一起传给 `doctor`：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml doctor \
  --repo my-repo \
  --service-name my-repo-auto-puller
```

### 检查配置

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml check-config
```

### 用户级后台服务

`install-service` 会按当前系统生成对应的后台服务：

- Linux：systemd user service
- macOS：launchd agent

先执行：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml install-service --enable --start
```

#### Linux

服务文件会写到：

```bash
~/.config/systemd/user/repo-auto-puller.service
```

查看状态：

```bash
systemctl --user status repo-auto-puller.service
```

看日志：

```bash
tail -f ~/.local/state/repo-auto-puller/repo-auto-puller.log
```

#### macOS

plist 会写到：

```bash
~/Library/LaunchAgents/repo-auto-puller.plist
```

查看服务是否已加载：

```bash
launchctl print gui/$(id -u)/repo-auto-puller
```

重新加载：

```bash
launchctl bootout gui/$(id -u)/repo-auto-puller 2>/dev/null || true
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/repo-auto-puller.plist
launchctl kickstart -k gui/$(id -u)/repo-auto-puller
```

看日志：

```bash
tail -f ~/.local/state/repo-auto-puller/repo-auto-puller.log
```

### 卸载后台服务

如果你只想移除后台服务定义，而保留二进制和配置文件：

```bash
repo-auto-puller uninstall-service
```

如果你安装时用了自定义服务名：

```bash
repo-auto-puller uninstall-service --service-name my-repo-auto-puller
```

这个命令会尝试停止并卸载服务，然后删除对应平台上的服务定义文件：

- Linux：删除 `~/.config/systemd/user/<service>.service`
- macOS：删除 `~/Library/LaunchAgents/<service>.plist`

## 工具为什么没有自动拉取

以下情况属于正常保护行为：

- 你的仓库有未提交改动
- 你的本地分支领先远端
- 你的本地分支和远端分叉

这时工具会跳过自动拉取，避免把你的现场改坏。

## 从源码安装

只有在你确实需要源码安装时才这样做：

```bash
git clone https://github.com/zhyongrui/repo-auto-puller.git
cd repo-auto-puller
cargo build --release -p repo-auto-puller
mkdir -p ~/.local/bin
cp target/release/repo-auto-puller ~/.local/bin/repo-auto-puller
```
