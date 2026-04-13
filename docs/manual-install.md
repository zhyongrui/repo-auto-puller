# Manual Install

这份文档是给最终用户看的。

## 你需不需要安装 Rust

一般不需要。

正常使用 `repo-auto-puller` 时，推荐直接使用预编译二进制。只有在以下情况才需要 Rust：

- 你想自己从源码构建
- 还没有适合你平台的发布包
- 你准备参与开发

## 推荐安装方式

### 方式 A：用预编译二进制

1. 打开 GitHub Releases 页面
2. 下载适合你系统和架构的压缩包
3. 解压出 `repo-auto-puller`
4. 放到 `~/.local/bin/repo-auto-puller`

Linux 用户也可以直接使用安装脚本：

```bash
curl -fsSL https://raw.githubusercontent.com/zhyongrui/repo-auto-puller/main/scripts/install.sh | bash
```

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

## 启动方式

### 前台运行

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml --repo my-repo
```

### 只检查一次

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml --repo my-repo --once
```

### 用户级后台服务

把服务文件放到：

```bash
~/.config/systemd/user/repo-auto-puller.service
```

服务文件示例：

```ini
[Unit]
Description=Repo Auto Puller
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=%h/.local/bin/repo-auto-puller --config %h/.config/repo-auto-puller/config.toml
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
```

启用：

```bash
systemctl --user daemon-reload
systemctl --user enable --now repo-auto-puller.service
```

查看状态：

```bash
systemctl --user status repo-auto-puller.service
```

看日志：

```bash
tail -f ~/.local/state/repo-auto-puller/repo-auto-puller.log
```

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
