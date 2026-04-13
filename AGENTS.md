# AGENTS

这是本仓库面向代理的规范入口。如果仓库里已经存在 `AGENTS.md`，应优先更新这份文件，而不是再创建一份平行文档。

## 这个文件是做什么的

这份文档的职责是：

- 告诉代理这个仓库是什么
- 告诉代理仓库里的主要文档入口
- 约束代理在本仓库里开发、改代码、验证、提交时的基本行为

它不是最终用户安装手册，也不应承载完整的产品部署流程。

如果任务是“替用户安装和部署 `repo-auto-puller`”，看：

- `docs/agent-install.md`

如果任务是“给最终用户展示安装方式”，看：

- `docs/manual-install.md`

如果任务是“给人解释项目本身”，看：

- `README.md`

## 仓库概览

`repo-auto-puller` 是一个本地优先的 Git 自动拉取工具。

主要结构：

- `crates/core`: Git 状态读取与同步决策
- `crates/cli`: 配置加载、调度、日志、信号处理
- `examples/config.toml`: 通用配置示例
- `deploy/systemd`: 服务模板
- `scripts/install.sh`: Linux/macOS 安装脚本
- `scripts/install.ps1`: Windows 安装脚本
- `docs/manual-install.md`: 给最终用户看的安装文档
- `docs/agent-install.md`: 给代理看的安装部署文档
- `docs/troubleshooting.md`: 常见故障排查
- `docs/product-plan.md`: 产品路线图
- `docs/rust-architecture.md`: Rust 技术设计

## 代理工作规则

- 优先保持文档职责清晰，不要把用户手册、代理安装手册、仓库开发规范混在同一份文件里。
- 如果任务涉及安装部署，优先更新 `docs/agent-install.md`，不要把完整部署流程塞回 `AGENTS.md`。
- 不要把当前机器上的实例化路径硬编码成所有用户的默认值，除非任务明确要求面向这台机器。
- 不要提交本地构建产物或运行时产物。
- 不要默认改用户仓库的 remote。
- 不要默认清理、stash、覆盖用户未提交改动。
- 不要把“服务在跑”直接说成“已经会自动拉取”，必须区分服务状态和实际拉取条件。
- 如果任务是连续产品开发，默认按“小步实现 -> 验证 -> 提交 -> 推送”的节奏推进，不把多个不相关功能揉进同一个提交。

## 开发与验证

本仓库的常用命令：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p repo-auto-puller
./scripts/regression-check.sh
```

如果改了 shell 安装脚本，也应至少检查：

```bash
bash -n scripts/install.sh
```

如果改了 PowerShell 安装脚本，也应至少做语法级检查或人工复核：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/install.ps1
```

## 文档导航

- 项目入口：`README.md`
- 用户安装：`docs/manual-install.md`
- 代理安装：`docs/agent-install.md`
- 故障排查：`docs/troubleshooting.md`
- 产品方向：`docs/product-plan.md`
- 技术架构：`docs/rust-architecture.md`
