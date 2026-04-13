# Rust Architecture

## 设计目标

Rust 版本的第一目标不是“功能多”，而是：

- 常驻运行稳定
- Git 状态判断可靠
- 日志清楚
- 默认行为保守
- 方便以后扩展成多仓库 daemon 和桌面产品后端

## 当前架构

当前 Rust CLI 采用单二进制结构：

- `main.rs` 负责参数解析、日志、轮询循环
- `RepoAutoPuller` 封装 Git 操作和同步决策
- `Snapshot` 表示一次仓库状态快照

当前 CLI 已经不仅是 `run` 模式，还承担了几类产品级入口：

- `init`：配置生成/更新
- `status`：状态探测与原因解释
- `check-config`：配置校验
- `install-service`：用户级服务安装辅助

核心流程：

1. 解析参数
2. 校验仓库路径
3. 周期性执行 `git fetch`
4. 读取当前分支与 upstream 状态
5. 判断 ahead / behind / dirty
6. 满足条件时执行 `git pull --ff-only --no-rebase`
7. 记录结果并进入下一轮

## 为什么先用 CLI

先做 CLI 有三个好处：

- Git 行为和规则可以先独立验证
- 未来桌面 UI 可以直接复用 daemon / core 逻辑
- 跨平台打包前，先把同步语义定稳

## 核心领域模型

### `Snapshot`

一个仓库在某次检查时的状态：

- `branch`
- `upstream`
- `remote`
- `remote_branch`
- `ahead`
- `behind`
- `dirty`

### `RepoAutoPuller`

负责：

- 执行 Git 子进程
- 读取状态
- 记录日志
- 决定是否拉取
- 持续轮询

## 决策规则

MVP 保守策略如下：

- `behind == 0 && ahead == 0`：无操作
- `behind > 0 && ahead > 0`：判定分叉，跳过
- `dirty && behind > 0`：跳过
- `ahead > 0`：跳过
- `behind > 0 && !dirty && ahead == 0`：执行 fast-forward pull

## 未来拆分建议

当进入多仓库和桌面版阶段，建议把代码拆成：

- `repo-auto-puller-core`
  负责仓库状态、Git 访问、同步决策
- `repo-auto-puller-cli`
  负责命令行接口
- `repo-auto-puller-config`
  负责配置读写、校验、迁移
- `repo-auto-puller-daemon`
  负责后台常驻、多仓库调度
- `repo-auto-puller-desktop`
  Tauri UI

## 未来能力扩展

下面这些功能不应该硬塞进当前 `main.rs`，以后应独立模块化：

- 配置文件加载
- 多仓库调度器
- 桌面通知
- 指标与审计日志
- 自动安装开机自启
- 更新检查
- 告警钩子执行器

## 依赖选择

当前建议依赖：

- `clap`：参数解析
- `anyhow`：错误包装
- `chrono`：日志时间戳
- `ctrlc`：优雅退出

如果后续加入配置文件和序列化，可再引入：

- `serde`
- `toml`

## 非目标

当前阶段不做：

- 自动 stash / 自动 merge
- 强制覆盖本地改动
- 云端执行 Git 拉取
- 浏览器内运行
