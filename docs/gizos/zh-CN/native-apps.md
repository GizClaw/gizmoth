# 原生 App 开发

原生 GizOS App 基于 Runtime 和可移植公共库实现应用行为。入口接收已初始化的 `h2_runtime_t *`，以及可选的 App 自有公共配置。最终产物入口负责创建和销毁 Runtime、选择平台并提供硬件映射。这是源码层面的开发流程，需要 GizOS 源码和所选目标的编译环境。

## 组织应用

独立 App 使用 `projects/<app>/app/{include,src}/`；分组 App 使用 `projects/<group>/apps/<app>/app/{include,src}/`。资源和打包数据应保留在所属项目中。项目专用的可复用库放在命名明确的 `projects/<project>/libs/<library>/` 包内。

在 App 公共契约中定义必需的逻辑 `component_id`。启动器必须在运行 App 前验证每个映射；缺失必需映射属于接线配置错误。仅在执行 PAL 操作时通过 Runtime 解析物理外设 ID，不要把它作为应用的可移植标识长期保存。

## 建立可预测的执行模型

业务状态由一个主循环统一写入。Runtime 事件提供有序事实，Runtime 状态提供当前快照。工作线程向主循环返回结果，不直接修改应用状态。阻塞式网络、存储或解码操作应放在事件处理器和 UI 观察者之外。

使用 `h2_runtime_wait_notify()` 等待 Runtime 唤醒或 App 截止时间；再次等待前排空 Runtime 事件，并处理库的分发队列。唤醒信号可以合并，不能用它统计操作完成次数。使用 LVGL 时，其调用、subject 修改和控件生命周期都应留在 App/LVGL 线程。

## 退出与验证

明确缓冲区所有权和回调生命周期。退出时先停止新工作，再请求取消、唤醒阻塞的使用者、等待工作线程结束、清理待处理的完成结果，最后在借用的 PAL 依赖仍有效时释放集成。发出取消请求并不代表工作已经结束。

通过生产路径测试应用状态转换，再构建最终目标以验证启动器接线。必需能力必须实际受支持，仅存在 API 对象不能证明这一点。

GizMoth 目前支持 Lua 项目。原生/Giz 项目创建与固件烧录属于后续功能；现阶段请在 GizOS 源码和工具链中执行本指南的流程。

## 来源

- [guides/development.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/development.md)
- [guides/structure.md](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/guides/structure.md)
- [libs/runtime/include/h2_runtime.h](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/runtime/include/h2_runtime.h)
