# Build, packaging and release rules

- 发布事实与历史例外必须从已核实的代码、构建输出、公开 GitHub 元数据或真实运行结果确认；不得依据猜测填写版本、资产或贡献者信息。
- 所有发布相关命令使用 UTF-8。工作区可能包含用户的未跟踪探针、构建目录和截图，只能显式暂存本次文件，不得清理、全量暂存或重置它们。
- 正式发布不得使用 `-SkipBuild` 或 `-AllowStale` 绕过新鲜度检查，也不得把 workspace 默认构建产生的 legacy shell 当作 GPUI 产品包。
- 构建和输出目录必须位于用户明确允许的路径。
- GitHub 推送使用普通非强制推送。发布后二进制版本标签不可因说明文档修订而移动。
- 结束前核验 Release 标题、标签、正文、真实资产文件名、空资产标签、大小、SHA256、分支与版本标签指向；创建命令成功不等于发布核验完成。
- 构建图、资产集合、更新协议或发布事务的非平凡变化写入 `architecture/notes/packaging/`。
- Windows MSIX/Scoop 的更新归属及打包边界见 [`安装归属记录`](../architecture/notes/packaging/2026-09-28-windows-distribution-ownership.md)；SDK 打包成功不代表商店认证或渠道安装验收完成。
