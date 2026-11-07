# Vector math and geometry

Built-in vmath and geometry modules.

## 内置 vmath 与 geometry

所有 Host（Desktop、设备、Wasm/browser）默认提供 `require('vmath')` 和 `require('geometry')`，不需要 App 注册 native module。`vmath` 不覆盖标准 `math`；两者都不依赖 Display 设备。API 的完整参数和边界契约见生产头文件 `libs/lua/include/h2_lua_numeric.h` 和 [Lua 数值 API](../../references/lua-numeric.md)。

`vmath.buffer(count[,kind])` 创建固定容量 userdata，最多 65536 个数值。kind 为 `"f64"`（默认，nil 也使用默认值）或 `"f32"`，分别存储 binary64 或 binary32。存储及等长事务 scratch 都通过 VM allocator 计费，分别为 `16 * count` 或 `8 * count` 字节，加固定 metadata/userdata 开销。索引从 1 开始；点布局为连续 `x,y` 或 `x,y,z`，没有嵌套表。在初始化时分配缓冲区、mesh writer 和 mesh，帧内复用。成功的批量调用不分配；错误消息可以分配。所有数值及结果必须有限且绝对值不超过 1e6，越界、错误类型、无效拓扑或容量不足都会抛出 Lua error，已发布的缓冲区和 mesh 不变。

除下文明确规定的 prepared 位移/系数入口外，同一次调用的所有缓冲区必须使用相同 kind，包括系数、权重、相机、mask、索引、tag 和 mesh topology；混用会在发布结果前抛出 `mixed numeric buffer kinds (f32/f64)`，空前缀也不例外，不做隐式转换。全 f32 调用使用 float 运算和单精度数学函数；Lua 标量先验证有限性及 ±1e6 范围，再在调用入口转换为 float（load 对每个导入元素转换一次）。参数区间按所选精度检查；存储和运算按该精度舍入，极小值可能下溢为零。get/dot 返回普通 Lua number，纯标量 clamp/lerp/smoothstep/spring 及标准 math 保持 double 语义。

数学模块提供标量插值/夹取/弹簧步进，缓冲区线性组合、逐元素乘除、多项式、点积、三维长度/归一化和通道 gather/scatter，以及批量 Verlet、XPBD 距离约束和位移阻尼。物理输入显式传入加速度、逆质量、约束边与 compliance；零逆质量固定节点，时间步范围是 `[1e-6,.1]` 秒。`relax` 每次将 lambda 清零，可选择双向距离或仅张力约束，最多 256 点、512 边和 32 次交替迭代；它不包含碰撞、材质或游戏规则。

`relax_sweep` 用每边的两个权重执行一次正向或反向约束扫描，保留 lambda，允许调用方在扫描间组合额外约束；`damp_edges` 按边顺序更新上一帧位置以衰减分离方向的轴向位移。`map` 提供 abs/sqrt/sin/cos/floor，`select_le` 做逐分量条件选择，`take` 按一基行索引重排固定宽度数据。参数、容量、别名和失败原子性遵守生产公共头。

几何模块提供二维/三维仿射、按权重位移和旋转、位移前缀和、折线展开、轴平面切分与近裁面裁剪投影。相机是 `{fx,fy,cx,cy,near}`，在相机空间沿 +Z 看，投影为 `(cx+fx*x/z, cy+fy*y/z)`，`near >= .001`；可用负 `fy` 翻转屏幕 Y。切分输出 `{side,source_index}`，投影输出源 segment 索引，Lua 可据此决定颜色。游戏公式、镜头参数、材质、颜色和时间步策略仍由 Lua 组合。

`geometry.mesh(vc,pc)` 返回 writer 和现有公共 Display mesh。 `geometry.update_mesh(writer,xy,topology,nv,np)` 将结果直接复制到 mesh，返回同一 mesh；topology 每行是 `{kind,first,count,rgb565}`，kind 0 为 3..128 点多边形， kind 1 为两点线段。xy 和 topology 可以同时使用 f32；writer 和公共 Display mesh 不绑定数值精度。该操作不绘制、不 present；使用现有 Display 批次绘制接口。f64 保留更小的位移，f32 将数据及 scratch 空间减半并使用单精度运算，批量调用消除逐点 Lua/C 边界开销；尚不承诺 S3 帧率或不同平台结果逐位一致，设备侧应按实际点数和迭代数测量。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L577), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
