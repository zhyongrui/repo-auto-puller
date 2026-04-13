# Agent Install Guide

这份文档是给各类代理式安装助手看的，不是给最终用户看的。

适用对象包括但不限于：

- Codex
- Claude Code
- OpenClaw
- 其他能在用户机器上执行安装与配置操作的代理

目标：把 `repo-auto-puller` 安装并配置到用户指定的 Git 仓库上，让它在本机后台运行。

## 原则

- 默认优先使用预编译二进制，不要默认要求用户安装 Rust。
- 只有在没有可用发布产物、且用户明确同意时，才走源码构建。
- 配置和服务文件应写到用户主目录下，而不是硬编码仓库目录内。
- 安装后必须验证服务状态和日志。
- 如果仓库工作区是脏的，告诉用户“工具已工作，但会按安全策略跳过自动拉取”。

## 推荐安装流程

### 1. 收集环境信息

检查：

- 操作系统
- CPU 架构
- 用户主目录
- 当前平台可用的用户级服务管理器
- 目标仓库路径
- 目标仓库当前 remote 与分支状态

### 2. 优先使用预编译二进制

优先方案：

- 从 GitHub Releases 下载对应平台的压缩包
- 解压得到 `repo-auto-puller`
- 安装到 `~/.local/bin/repo-auto-puller`
- 如果 `curl` 下载偶发失败，而目标机器上有 `gh`，优先回退到 `gh release download`

当前发布包覆盖：

- Linux `x86_64`
- Linux `aarch64`
- macOS `x86_64`
- macOS `aarch64`

只有以下情况才考虑源码构建：

- 还没有发布产物
- 用户就是开发者
- 用户明确要求源码安装

### 3. 生成用户配置

把配置写到：

- `~/.config/repo-auto-puller/config.toml`

配置应至少包含：

- 仓库名称
- 仓库路径
- 轮询间隔
- 是否启用
- 日志文件路径
- 可选失败告警命令

如果用户只希望某些分支能自动拉取，可在仓库配置里加入 `allowed_branches`。

如果用户只是想临时停用某个仓库，但仍保留配置和状态可见性，优先设置 `paused = true`，不要直接删配置。

除非用户明确要求，不要把用户机器上的临时绝对路径提交回仓库。

优先使用 `init` 子命令生成或更新配置，而不是让用户或代理手写 TOML：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml init --repo-path /path/to/repo
```

如果要显式指定仓库名或 interval：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml init \
  --repo-path /path/to/repo \
  --name my-repo \
  --interval 60
```

如果需要失败告警，可在配置里加入：

```toml
[defaults]
on_failure_command = "notify-send 'repo-auto-puller' \"$REPO_AUTO_PULLER_REPO_NAME: $REPO_AUTO_PULLER_ERROR\""
```

### 4. 安装用户服务

优先使用工具内建的服务安装辅助，而不是让用户或代理手写服务定义：

- Linux：生成 `~/.config/systemd/user/<service>.service`
- macOS：生成 `~/Library/LaunchAgents/<service>.plist`

如果用户只想临时运行，可直接前台运行二进制。

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml install-service --enable --start
```

如果只想安装针对某个仓库的服务：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml install-service \
  --service-name openclawcode-auto-puller \
  --repo openclawcode \
  --enable \
  --start
```

如果要回滚用户级后台服务，优先用：

```bash
repo-auto-puller uninstall-service --service-name openclawcode-auto-puller
```

### 5. 验证

安装完成后必须验证：

- Linux：`systemctl --user status repo-auto-puller.service`
- macOS：`launchctl print gui/$(id -u)/repo-auto-puller`
- 日志文件是否持续写入
- 至少执行一次真实检查
- `repo-auto-puller --config ~/.config/repo-auto-puller/config.toml doctor --repo <name>`
- `repo-auto-puller --config ~/.config/repo-auto-puller/config.toml check-config`
- `repo-auto-puller --config ~/.config/repo-auto-puller/config.toml status --repo <name>`

如果 Agent 需要做程序化判断，优先使用：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml status --repo <name> --json
```

如果实际安装的是自定义服务名，Agent 在执行 `doctor` 时必须把同一个 `--service-name` 传回去，否则会把“服务名不匹配”误报成“服务未安装”。

建议额外确认：

- 当前仓库是否工作区脏
- 当前仓库是否 behind / ahead / diverged
- 当前远端是否能正常 `fetch`
- 如果服务已启动但没有拉取，具体是保护策略导致，还是网络 / 凭据导致

### 6. 向用户解释状态

对用户的说明必须清楚区分以下几种情况：

- 已成功启动并正常检查
- 因工作区脏而跳过自动拉取
- 因分叉而跳过自动拉取
- 因本地领先而跳过自动拉取
- 因远端网络或凭据问题而无法 fetch

## 已验证的部署发现

下面这些不是假设，而是在这台机器上真实遇到过的情况：

- 普通用户不应被要求先安装 Rust。
- 当前发布链路的目标平台是 Linux `x86_64`/`aarch64` 和 macOS `x86_64`/`aarch64`。
- `repo-auto-puller` 已在这台机器上成功编译通过，`cargo test`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo build --release -p repo-auto-puller` 都已通过。
- 这台机器上已经把 `repo-auto-puller` 实际部署到了 `openclawcode` 仓库上。
- 当前机器上的实例化部署路径是：
  - 二进制：`/home/lyz/.local/bin/repo-auto-puller`
  - 配置：`/home/lyz/pros/repo-auto-puller/config.toml`
  - 日志：`/home/lyz/.local/state/repo-auto-puller/repo-auto-puller.log`
  - 服务：`openclawcode-auto-puller.service`
- 上面这些路径属于当前机器实例，不应被当成所有用户的默认路径写死到通用安装逻辑里。
- `openclawcode` 当前工作区是脏的，而且落后远端若干提交；工具会按设计跳过自动拉取，这是正确行为，不是故障。
- 这台机器曾经出现过对 `https://github.com/zhyongrui/openclawcode.git` 的 `gnutls_handshake()` TLS 错误；这是远端访问问题，不是同步决策逻辑的问题。
- 这台机器当前没有可用的 GitHub SSH key，`git@github.com` 连接会 `Permission denied (publickey)`；不要默认把用户仓库 remote 切到 SSH 来“修复” fetch。
- 替换正在运行的二进制时，直接 `cp` 到 `~/.local/bin/repo-auto-puller` 可能报 `Text file busy`；应先停服务，并确保没有残留进程占用该文件。

## 针对这个仓库的额外注意事项

- 本仓库里已经存在用户版和 Agent 版两类文档；更新时应保持角色边界清晰：
  - `docs/manual-install.md` 给最终用户
  - `docs/agent-install.md` 给代理
- 本仓库支持通用安装，也支持像 `openclawcode` 这样为某个具体仓库做实例化部署；不要把实例化部署细节误写成通用默认值。
- 通用服务模板应使用 `repo-auto-puller.service` 这类中性命名；只有在为具体仓库落地时，才用 `openclawcode-auto-puller.service` 这样的名字。
- 当前日志显示 `openclawcode` 持续因为工作区脏而跳过拉取。如果代理要向用户汇报状态，应明确说“工具已生效，但当前不会自动拉取”，而不是简单说“工具坏了”。

## 二进制更新注意事项

如果需要在用户机器上升级一个已经运行中的实例，推荐顺序是：

1. `systemctl --user stop <service>`
2. 确认没有残留进程占用二进制
3. 再覆盖 `~/.local/bin/repo-auto-puller`
4. `systemctl --user start <service>`
5. 重新看 `status` 和日志

不要在服务运行时直接覆盖二进制文件。

如果用户要彻底移除后台服务，但暂时保留二进制和配置，不要手删 unit/plist，优先使用 `uninstall-service`。

## 给 Agent 的推荐交付说法

安装完成后，建议明确告诉用户：

- 工具已经部署到哪个仓库
- 配置文件在哪
- 服务名是什么
- 二进制路径在哪
- 现在是否真的能拉取
- 如果不能，具体是因为脏工作区、凭据还是网络问题

## 不要做的事

- 不要默认改用户仓库的 remote
- 不要默认清理或 stash 用户未提交改动
- 不要把“服务已运行”说成“已经会自动拉取”，除非 fetch 和决策链路都正常
- 不要要求普通用户先安装 Rust，除非没有发布包可用
