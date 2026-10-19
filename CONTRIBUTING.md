# Pebrel 开发入口

当前以个人开发为主，任务范围和操作授权以用户指示为准。

- 开始任务读 [AGENTS.md](AGENTS.md) 和本地 `CONTEXT.md`，再读目标模块的指南。
- 架构职责见 [architecture](docs/architecture.md)，检查器合同见 [project constraints](docs/project-constraints.md)；仅在相关改动时查阅。
- 新 UI 文案遵循 [internationalization](docs/internationalization.md)。
- 产品命令用 `pebrel`，内部 crate/目录沿用 `nebula*`，保留现有兼容标识。
- 只改本次需求涉及的源码；实际运行、编译和自动化测试分别报告，不把未验证写成通过。
- 使用 Windows Git，保留 CRLF；禁止 `cargo fmt --all`。

常用产品命令（Windows Rust 工具链）：

```text
cargo check --locked -p nebula --bin pebrel --features gpui-shell --tests
cargo test --locked -p nebula --bin pebrel --features gpui-test-support <具体测试过滤词>
cargo build --locked -p nebula --bin pebrel -p nebula_hook --bin pebrel-hook --features nebula/gpui-shell
```

构建缓存、正式工作树和当前待验收项以本地 `CONTEXT.md` 为准。提交、部署和重启按用户授权执行。
