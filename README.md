# Repo Auto Puller

`repo-auto-puller` 是一个独立的、本地优先的 Git 自动拉取助手。它不嵌进业务仓库，而是作为单独程序守护本机上的代码仓库。

当前方向已经从“单个 Python 脚本”升级为“可产品化的 Rust CLI 基础版”：

- 本地优先，直接操作用户机器上的 Git 仓库。
- 默认守护 `/home/lyz/pros/openclawcode`，也支持指定任意仓库。
- 当远端领先且本地可以安全 fast-forward 时，自动执行拉取。
- 本地脏工作区、本地领先、分叉时只告警，不强拉。
- 这是 CLI/MVP 基础，后续可以在此之上加桌面 UI、多仓库管理、通知和开机自启。

## 项目结构

- `src/main.rs`: CLI 入口、轮询循环、日志输出
- `docs/product-plan.md`: 产品定位、商业化方向、路线图
- `docs/rust-architecture.md`: Rust 版本的技术设计

## 快速开始

先安装 Rust：

```bash
curl https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
```

然后运行：

```bash
cd /home/lyz/pros/repo-auto-puller
cargo run -- --repo /home/lyz/pros/openclawcode --interval 60
```

只检查一次：

```bash
cargo run -- --repo /home/lyz/pros/openclawcode --once
```

只做检测，不真的拉：

```bash
cargo run -- --repo /home/lyz/pros/openclawcode --once --dry-run
```

写日志到文件：

```bash
cargo run -- \
  --repo /home/lyz/pros/openclawcode \
  --interval 60 \
  --log-file ~/.local/state/repo-auto-puller/openclawcode.log
```

## 当前 MVP 行为

- 自动执行 `git fetch`
- 用 `git rev-list --left-right --count HEAD...<upstream>` 判断 ahead/behind
- 仅在 fast-forward 安全时执行 `git pull --ff-only --no-rebase`
- 支持 `--once`、`--dry-run`、`--verbose`、`--log-file`
- 收到 `SIGINT` / `SIGTERM` 时优雅退出

## 产品化方向

建议先做“本地桌面产品”，不是纯 SaaS。

理由：

- 这个产品需要读写本机 Git 仓库
- 要兼容 SSH/HTTPS 凭据
- 必须尊重未提交改动和本地分支状态
- 更适合本地守护进程 + UI 外壳

详细设计见：

- [docs/product-plan.md](docs/product-plan.md)
- [docs/rust-architecture.md](docs/rust-architecture.md)

## 备注

当前这台机器上还没有安装 Rust 工具链，所以这个项目结构和代码已经切到 Rust，但还没有在本机完成 `cargo build` 验证。
