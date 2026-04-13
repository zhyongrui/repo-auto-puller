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
dry_run = false
```

如果你想先只做检测、不真的拉取，把 `dry_run` 改成 `true`。

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
