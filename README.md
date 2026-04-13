# Repo Auto Puller

`repo-auto-puller` 是一个本地优先的 Git 自动拉取产品原型。它已经按你的当前环境接到了 `openclawcode` 仓库上，同时也具备继续扩展到多仓库的基础能力。

当前项目已经完成这几件事：

- Rust workspace 结构，拆成 `core + cli`
- `config.toml` 配置文件
- 多仓库配置模型
- `openclawcode` 的实际接入配置
- GitHub Actions CI
- MIT License

## 现在怎么应用到 openclawcode

项目根目录里的 `config.toml` 不是示例，而是当前这台机器上针对 `openclawcode` 的实际配置：

- 仓库名：`openclawcode`
- 仓库路径：`/home/lyz/pros/openclawcode`
- 拉取间隔：60 秒

如果只想对这个仓库执行一次检查：

```bash
cd /home/lyz/pros/repo-auto-puller
cargo run -p repo-auto-puller -- --config /home/lyz/pros/repo-auto-puller/config.toml --repo openclawcode --once
```

如果只做检测，不真的拉取：

```bash
cd /home/lyz/pros/repo-auto-puller
cargo run -p repo-auto-puller -- --config /home/lyz/pros/repo-auto-puller/config.toml --repo openclawcode --once --dry-run
```

常驻运行：

```bash
cd /home/lyz/pros/repo-auto-puller
cargo run -p repo-auto-puller -- --config /home/lyz/pros/repo-auto-puller/config.toml --repo openclawcode
```

## 给 openclawcode 的部署文件

项目里已经加了一个 `systemd --user` 服务文件模板：

- `deploy/systemd/openclawcode-auto-puller.service`

推荐部署步骤：

1. 安装 Rust
2. 在本项目里执行 `cargo build --release -p repo-auto-puller`
3. 把生成的二进制复制到 `~/.local/bin/repo-auto-puller`
4. 把服务文件复制到 `~/.config/systemd/user/openclawcode-auto-puller.service`
5. 执行 `systemctl --user daemon-reload`
6. 执行 `systemctl --user enable --now openclawcode-auto-puller.service`

查看日志：

```bash
journalctl --user -u openclawcode-auto-puller.service -f
```

## 项目结构

- `crates/core`: Git 仓库状态与自动拉取决策
- `crates/cli`: 配置加载、多仓库调度、日志与信号处理
- `config.toml`: 当前机器上对 `openclawcode` 的实际配置
- `deploy/systemd`: systemd 用户服务模板
- `docs/product-plan.md`: 产品路线图
- `docs/rust-architecture.md`: Rust 技术设计

## 当前行为

- 周期性 `git fetch`
- 用 `git rev-list --left-right --count HEAD...<upstream>` 判断 ahead / behind
- 仅在 fast-forward 安全时执行 `git pull --ff-only --no-rebase`
- 本地脏工作区、本地领先、分叉时跳过
- 支持 `--config`、`--repo`、`--once`、`--dry-run`、`--verbose`

## 说明

当前这台机器还没有安装 Rust 工具链，所以我已经把工程结构和代码改好了，但还没有在本机执行 `cargo build` / `cargo test`。CI 已经补上，等你推送后 GitHub Actions 会帮你验证。
