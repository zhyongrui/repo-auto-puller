# Repo Auto Puller

`repo-auto-puller` 是一个本地优先的 Git 自动拉取工具。它会在后台检查你本机上的仓库；当远端分支有新提交、且本地可以安全 fast-forward 时，它会自动拉取。

正常用户不应该被要求先装 Rust。产品的默认交付方式应该是：

- 用户下载预编译二进制
- 用户运行安装脚本或按手册手动配置
- 工具以用户级后台服务的方式常驻运行

只有开发者或贡献者才需要 Rust。

## 两套使用文档

- 用户版安装手册：`docs/manual-install.md`
- 代理安装手册：`docs/agent-install.md`
- 仓库代理规范：`AGENTS.md`

如果你是最终用户，先看用户版。
如果你是帮用户安装和配置的 Agent，先看 Agent 版。

## 当前项目状态

- Rust workspace：`crates/core` + `crates/cli`
- 配置文件模型：支持多仓库
- `init`、`status`、`check-config`、`doctor`、`install-service`、`uninstall-service` 子命令
- 失败告警钩子
- 平台原生后台服务安装辅助
- GitHub Actions CI
- GitHub Releases 构建工作流
- Linux/macOS 安装脚本
- 预编译发布包覆盖 Linux `x86_64`/`aarch64` 与 macOS `x86_64`/`aarch64`

## 对新用户的默认路径

推荐安装方式：

1. 从 GitHub Releases 下载预编译包
2. 运行 `scripts/install.sh`
3. 运行 `repo-auto-puller init`
4. 运行 `repo-auto-puller install-service`
5. 用 `repo-auto-puller doctor`、`status` 和 `check-config` 验证

如果用户愿意手动安装，也支持完全手动配置。

## 本机当前应用状态

这个仓库当前已经在本机上实际守护 `openclawcode`：

- 配置文件：`/home/lyz/pros/repo-auto-puller/config.toml`
- 服务名：`openclawcode-auto-puller.service`
- 二进制路径：`/home/lyz/.local/bin/repo-auto-puller`

这属于当前机器上的实例化部署，不代表普通用户也必须按这个路径来。

## 仓库结构

- `crates/core`: Git 状态、决策逻辑
- `crates/cli`: 配置加载、调度、日志、信号处理
- `examples/config.toml`: 通用配置示例
- `deploy/systemd/repo-auto-puller.service.template`: 通用 systemd 模板
- `scripts/install.sh`: Linux/macOS 安装脚本
- `docs/manual-install.md`: 用户版手册
- `docs/agent-install.md`: 给 Codex、Claude Code、OpenClaw 等代理的安装使用说明
- `AGENTS.md`: 给代理看的仓库工作规范与文档导航
- `docs/product-plan.md`: 产品路线图
- `docs/rust-architecture.md`: Rust 技术设计

## 开发者

开发和贡献时才需要 Rust：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p repo-auto-puller
```

## 新增：初始化配置

现在可以直接用 `init` 生成或更新配置，而不用手写 TOML：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml init --repo-path /path/to/repo
```

也可以显式指定名字、轮询间隔和 dry-run：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml init \
  --repo-path /path/to/repo \
  --name my-repo \
  --interval 30 \
  --dry-run
```

如果只允许在指定分支上自动拉取，可以在配置里加：

```toml
[[repositories]]
name = "my-repo"
path = "/path/to/your/repo"
allowed_branches = ["main", "release"]
```

如果只是想临时停掉某个仓库的自动拉取，但保留配置和状态可见性，可以设：

```toml
[[repositories]]
name = "my-repo"
path = "/path/to/your/repo"
paused = true
```

如果你想在真实自动拉取前后执行命令，也可以在配置里加：

```toml
[defaults]
before_pull_command = "echo before pull"
after_pull_command = "echo after pull"
```

## 新增：状态和配置检查

查看仓库当前状态和阻塞原因：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml status --repo my-repo
```

如果要给脚本、Agent 或其他工具消费，可直接输出 JSON：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml status --repo my-repo --json
```

检查配置是否有效、仓库路径是否能初始化：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml check-config
```

做一遍安装后诊断，确认配置、后台服务和仓库探测都正常：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml doctor --repo my-repo
```

如果安装时用了自定义服务名，也要带上：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml doctor \
  --repo my-repo \
  --service-name my-repo-auto-puller
```

## 新增：服务安装辅助

现在可以直接生成并安装用户级后台服务。
Linux 上会生成 systemd user service，macOS 上会生成 launchd agent：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml install-service --enable --start
```

如果只想让服务盯某一个仓库：

```bash
repo-auto-puller --config ~/.config/repo-auto-puller/config.toml install-service \
  --service-name my-repo-auto-puller \
  --repo my-repo \
  --enable \
  --start
```

如果要移除这个后台服务：

```bash
repo-auto-puller uninstall-service --service-name my-repo-auto-puller
```
