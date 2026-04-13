# Repo Auto Puller

`repo-auto-puller` 是一个本地优先的 Git 自动拉取工具。它会在后台检查你本机上的仓库；当远端分支有新提交、且本地可以安全 fast-forward 时，它会自动拉取。

正常用户不应该被要求先装 Rust。产品的默认交付方式应该是：

- 用户下载预编译二进制
- 用户运行安装脚本或按手册手动配置
- 工具以用户级后台服务的方式常驻运行

只有开发者或贡献者才需要 Rust。

## 两套使用文档

- 用户版安装手册：`docs/manual-install.md`
- Agent 通用操作手册：`AGENTS.md`

如果你是最终用户，先看用户版。
如果你是帮用户安装和配置的 Agent，先看 Agent 版。

## 当前项目状态

- Rust workspace：`crates/core` + `crates/cli`
- 配置文件模型：支持多仓库
- systemd 用户服务模板
- GitHub Actions CI
- GitHub Releases 构建工作流
- Linux 安装脚本

## 对新用户的默认路径

推荐安装方式：

1. 从 GitHub Releases 下载预编译包
2. 运行 `scripts/install.sh`
3. 编辑配置文件
4. 启动用户服务

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
- `scripts/install.sh`: Linux 安装脚本
- `docs/manual-install.md`: 用户版手册
- `AGENTS.md`: 给 Codex、Claude Code、OpenClaw 等代理的安装使用说明
- `docs/product-plan.md`: 产品路线图
- `docs/rust-architecture.md`: Rust 技术设计

## 开发者

开发和贡献时才需要 Rust：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p repo-auto-puller
```
