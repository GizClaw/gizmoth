# Display drawing and frames

Graphics, colors, shapes, retained drawing and frame submission. See input-example for basic drawing.

### Display AA 与 framebuffer fade

`display.fill_circle_aa(cx, cy, radius, color)` 使用有界 supersample coverage 混合 RGB565 framebuffer，`radius` 限制为 `0..64`。`display.fade_to_black(amount)` 对完整 framebuffer 衰减，`display.fade_rect_to_black(x, y, width, height, amount)` 只衰减完全位于 framebuffer 内的正尺寸矩形；`amount` 均为 `0..255`。三者只标记实际 clipping 后的 dirty region，不隐式 `present`。小于一个 RGB565 channel step 的 fade 使用固定、有界的 spatial phase，避免高 FPS 下暗色 trail 永远不消失。

`display.draw_circle(cx, cy, radius, color)` 绘制裁剪到 framebuffer 的一像素圆周。圆心允许处于 Display 宽高的一倍负边界到两倍正边界内，半径必须位于 `0..min(display.width, display.height)`；超出范围、Display 未打开或颜色无效时保持现有 Lua argument/error 合同并确定性失败。`clear`、矩形、圆和圆角矩形可以批量写 framebuffer；默认 `present` 提交所有待绘制图元的 dirty bounding union，可显式启用下述 retained 比较模式。

### Display 多边形、椭圆与保留命令

`display.fill_polygon(points, color, offset_x=0, top=0, bottom=height, scale=1)` 接收 3..128 个 `{x,y}` 点。坐标和横向偏移必须有限且位于 ±100000，缩放为 `0<scale<=16`。先缩放顶点，再在整数行使用 even-odd 扫描转换；边的纵向范围下闭上开，每对交点覆盖 `ceil(left)..floor(right)`，包含水平区间的两个端点。交点取整后才将横向偏移按 `floor(value+0.5)` 加入，不等于在顶点变换阶段平移。支持凹多边形、自交、重复点和退化边；后两者不产生除零。

`display.fill_ellipse(cx, cy, rx, ry, color, offset_x=0, top=0, bottom=height)` 使用相同坐标范围，要求 `rx>=0`、`0<ry<=2048`。局部 y 从 `-ry` 开始以 1 递增到 `+ry`；目标行是 `floor(cy+y+0.5)`，半宽是 `rx*sqrt(max(0,1-y*y/(ry*ry)))`。左端是 `floor(cx-half_width+offset_x+0.5)`，宽度是 `floor(2*half_width+1.5)`；小数半径保留这一像素采样规则，不隐式缩放 framebuffer。

`display.compile_commands(commands)` 把最多 16384 条六字段命令复制为 VM 所有的不可变 userdata。`{0,x,y,width,height,color}` 表示矩形，`{1,x,y,x2,y2,color}` 表示线段。kind 必须是整数；四个数值字段必须有限、位于 ±100000，向零截断为像素整数；矩形尺寸不能为负。空列表合法。编译后修改或释放原表不影响命令；保留 userdata 使它跨 GC 存活，释放最后一个引用后可回收。复制体计入该 job 的 VM 内存预算，不创建系统堆缓存，也不自动扩容。

`display.draw_commands(handle, top=0, bottom=height, offset_x=0, offset_y=0, scale_x=1, scale_y=scale_x, color_override=nil)` 按原顺序重放。偏移必须有限且位于 ±100000，两个缩放均为 `0<scale<=1000`；端点按 `floor(offset+coordinate*scale+0.5)` 计算。矩形使用半开区间，线在 Bresenham 迭代前裁剪；因此远在屏幕外的端点不造成按距离增长的光栅循环。颜色覆盖不修改原命令。颜色沿用 Display 字符串或 `{r,g,b}` 合同；`green` 为 RGB(0,128,0)，满亮度绿色须显式传入 RGB(0,255,0)。

以上绘制使用 `[top,bottom)` 行裁剪和 framebuffer 列裁剪，要求整数 `0<=top<=bottom<=height`，在转换为 native int 前检查；空裁剪和零面积矩形不修改像素。参数解码完成后才开始绘制，非法参数或 Display 已关闭时抛 Lua 错误；RGB 表的 getter 仍遵循 Lua 元方法语义，其自身的副作用不属于绘制的原子性保证。编译失败不会返回部分句柄，OOM 后释放临时数据即可再次尝试较小批次。绘制只标记 dirty union，不隐式 present；Display deinit 后保留命令不持有 framebuffer，也不允许继续绘制。重复调用缓存的 `require('display')` 不代表重新打开设备。

### Display indexed rectangles and palettes

`display.compile_rects(records)` 复制具名 `{x,y,width,height,color_index}` 记录为不可变 userdata，`display.compile_palette(colors)` 将既有颜色字符串或具名 `{r=...,g=...,b=...}` 转为固定长度 RGB565 userdata。两者最多 16384 项，空列表合法，数据计入 VM；不改变已有 commands/mesh 接口。Lua 颜色索引从 1 开始，绘制时验证实际 palette 长度。

`display.blend_palette(output,a,b,progress)` 要求三套 palette 长度一致，进度是 `0..256` 的整数，输出可与输入相同。`display.draw_rects(batch,palette[,left,top,right,bottom])` 使用已打开的 Display，四个半开 clip 整数要么全部提供，要么全部省略。成功返回零个值；调用不隐式 present，不增长容量、不分配内存。完整参数与像素合同见 [Lua API](../../references/lua.md) 和 [Raster2D](./raster2d.md)。

C core 完整校验后绘制，adapter 再逐矩形标记已有 dirty/background damage。没有新缓存或损伤对象。构造和 palette blend 不自行打开 Display；`require('display')` 仍沿用既有 acquisition，关闭后的 proxy 不可绘制。参数 getter 自身可以有副作用；构造过程不写 framebuffer，后续 draw 必须重新检查 Display 状态。GC、取消、job release 与 Host teardown 继续走已有回收流程。

### Display 四边形条带批次

`display.compile_quad_batch` 保存调用方定义的归一化条带、可选横向 patch 和 painter order；`display.draw_quad_batch` 每次接收四角、颜色及可选行裁剪，复用既有 polygon 光栅、dirty/background damage 和 retained present。接口与精确插值顺序见 [Display API](../../references/lua.md)。颜色可使用既有 `compile_palette`，或复用普通 Lua 颜色数组以逐帧更新 RGB888 配方；批次本身不生成渐变或光晕颜色，不选择视角、拓扑或可见性。

绘制参数末尾可选 `clip_top/clip_bottom` 指定 framebuffer 的半开整数行区间，省略或 `nil` 分别使用 `0/height`，要求 `0<=clip_top<=clip_bottom<=height`。它们只限制光栅扫描行，不移动四角、不重新插值，与批次记录内的归一化 patch `top/bottom` 无关。空裁剪仍完整验证颜色、角点、区间及 Display acquisition；多个不相交区间按每行相同的 painter order 绘制，可合成全幅结果。每次调用仍解码颜色并展开几何，应用自行选择脏行区间；此接口不新增扫描跨度缓存。

拓扑上限为 256 项，初始化复制到 VM 计费 userdata，GC/VM teardown 回收；逐帧不创建 mesh、中间顶点 buffer 或光栅缓存。显式横向 patch 先插值，再沿其两边展开条带，保持与逐条 Lua `fill_polygon` 相同的浮点运算顺序和绘制顺序。成功 draw 自身不分配存储，所有颜色与角点在首次写像素前验证。空批次、退化条带、重叠及屏幕裁剪沿用 polygon 行为；颜色 getter 的 Lua 副作用不属于失败原子性保证。

### Display 双轨材质实验批次

`display.material_strip` 接收调用方提供的两条屏幕 XY 轨道，复用 f64 buffer、已编译 material 和 palette；公共合同见 [Display API](../../references/lua.md)。同一站点数的几何更新保留绑定，站点数变化释放旧绑定。逐面保持 material → iso-line 顺序，沿用原 Q24、完整 fallback 与整数线条相位。此候选不决定投影、拓扑、可见性、颜色配方或提交时机；接入前应对照 scalar 和 Lua 共享端点基线测量真实几何上传、绑定、调色板更新及冷/热绘制成本，并由消费端实机决定是否保留。

同一批次中不同面的动态颜色必须使用独立 mutable palette；复用一个调色板后再批量绘制会让所有引用读取同一个最终颜色。成功 load/bind/draw 在预留 Lua stack 后无分配，不创建 mesh 或光栅缓存，存储和绑定引用均由 VM 计费和回收。构造、OOM、finalizer 重入与首个像素写入前的完整验证遵循 public header 合同。

### Display 笔画与有界缓存

`display.stroke_path(points,widths,color,offset_x=0,top=0,bottom=height,cache=false,fast=false,smooth=false,scale=1,tolerance=0)` 接受 `2..256` 个有限 ±100000 的点对、恰好 `n-1` 个 `0..1000` 宽度，以及单色或 `n-1` 个颜色。cache/fast/smooth 必须为 boolean；scale 为 `0<scale<=16`，先作用于坐标和宽度；offset 有限且位于 ±100000。top/bottom 沿用屏内整数半开行裁剪。所有参数、颜色 getter 和分配在绘制前完成并重新检查 Display。返回 `(cache_hit,fast_segment_count)`，不是帧率。

首参数也可为 `{buffer=xy,count=n}`，其中 xy 是容量至少 `2*n` 的 f64 packed xy buffer，n 为 `2..256` 整数，坐标沿用有限 ±100000 限制；descriptor 不得混入数字键点数组。字段使用 raw 读取，每次复制并验证当前坐标后再绘制。稳定 descriptor 表持有原 normals cache，widths 表持有原 span cache；坐标值、数量和样式变化会失效，不能只比较 buffer 身份。其他参数、返回值、像素算法与显式 present 行为不变。buffer 在本次调用期间保活，颜色 getter/GC 后仍检查 Display 生命周期，不跨调用保存裸指针。

默认 hard 模式为每段宽度居中的四边形与中心线，长度小于 `.01` 时绘制取整方块；非退化零宽度段保留中心线。fast 使用带整数边界误差检查的 float/FMA 四边形，不满足条件时回退双精度几何。width-independent 双精度法线缓存随点表存活，并比较全部坐标。cache 随宽度表保留最多 2048 条有序扫描段/中心线记录；键包含缩放后坐标、宽度、颜色、偏移、裁剪、viewport 和模式。容量不足时继续绘制完整结果但使缓存失效，不能截断画面。两类缓存都计入 VM，释放点/宽度表后可以 GC 回收，不持有 framebuffer。

smooth 显式启用圆端点连续覆盖，每像素只混合最大 alpha 一次，相同 alpha 保留先前段颜色，零宽度跳过。像素中心为 `(x+.5,y+.5)`；覆盖为 `clamp(radius+.5-distance,0,1)`，取整到 `0..255` 后使用现有 RGB565 blend。端点范围不超过 4096 时保留 screen-local float 快速覆盖，极端坐标使用双精度回退；它不承诺与 hard 模式相同像素。颜色/覆盖 scratch 按裁剪区域分配、在同一 job 内复用，并在 Display/job/Host 关闭时释放引用。tolerance 是默认关闭的 `0..0.25` 屏幕像素弦误差，仅用于 smooth；保留样式边界、拒绝回折并检查所有省略点，不改变世界物理节点或时间步。

smooth 的 `cache=true` 对单色且 `tolerance=0` 的笔画启用覆盖率缓存。稳定 widths 表持有一项最多 16 KiB payload 的 VM userdata，键包含点数、缩放后坐标/宽度、scale、精确 offset、行裁剪和 viewport；颜色与背景不在键中，命中时使用当前颜色在当前 framebuffer 上混合，保持逐笔画 glow/fill 顺序。只保存量化 alpha，不保存 RGB565 背景。多色、简化、空范围或超限输入保留原路径与已有缓存；热命中零分配并返回 `(true,0)`，冷绘制/回退返回 `(false,0)`。释放 widths 后可 GC 回收；冷建的 staging 与 job scratch 均计入 VM 配额，分配失败不发布不完整缓存或本次像素。详细生命周期、失效与内存合同见 [Lua Display API](../../references/lua.md)。

通用多边形使用 float edge-slope/integer-boundary 检查，不能确定相同 floor/ceil 时回退原双精度交点表达式。参考像素测试覆盖边界与确定性随机输入，不把有限样本当作数学证明。笔画通过 Utils 公共 `h2_f32_math.h` 消费单份数值辅助；编译器和浮点环境约束见 [Utils](./utils.md)，不要对绘制或物理库启用 fast-math。

### Display 快照、背景恢复与 retained 提交

`display.region_from_string(width,height,data,encoding="rgb565be")` 直接从 Lua 字符串创建不透明 region，宽高必须为 `1..4096` 的整数。构造函数本身不获取 Display，在已取得的 proxy 上调用 `deinit` 后仍可创建资源，不绘制也不隐式 present。首次 `require('display')` 仍遵循既有 acquisition 契约，获取失败抛错；缓存的 require 不重新打开设备，关闭后的绘制仍然失败。有效绘制会话中的结果可传给 `draw_region`，同屏尺寸的结果还可传给 `restore_background`。像素数据和调用时机由应用拥有，不读取文件或提前加载资源。

默认 `rgb565be` 接受恰好 `width*height*2` 个按行排列的二进制字节，每个 RGB565 像素高字节在前。`rgb565be-lz4-b85` 接受 8 位 ASCII 十六进制压缩长度与 Python `base64.b85encode(block,pad=True)` 文本，block 为标准 raw LZ4，不含 frame 或额外输出尺寸；输出必须恰好匹配宽高。Base85 字符、文本长度、32-bit group 溢出、零 padding、LZ4 截断、offset、输出边界及末尾序列条件均校验，不接受额外尾部数据。完整编码契约与示例见 [Lua Display API](../../references/lua.md#regions-from-strings)。

同步解码在所属 VM worker 中直接写入 region userdata，不构造像素 Lua 表或完整中间解压缓冲区；Base85 分组由 [`libs/encoding`](./encoding.md) 的 `h2_encoding_decode_base85_group()` 按 `h2_encoding_base85_rfc1924` 的只读解码表逐组解码，长度头、完整分组、零 padding 与 LZ4 规则仍由 Display 校验。像素、行元数据与 damage tiles 计入 VM 配额，输入字符串存活时也占自身配额，但 region 不保留输入引用。非法输入抛 Lua error，配额耗尽使用正常 Lua memory error；失败不发布半成品 region，不改变 framebuffer，临时 userdata 可由 GC 回收，释放其他数据后可以重试。普通 region 在最后引用释放后回收，背景持有的 region 沿用 `release_background` 和 teardown 的释放规则。

`display.capture_region(x,y,width,height,key=nil,reuse=nil)` 捕获 framebuffer 中的正尺寸区域，宽高各不超过 4096，位置和尺寸必须为整数且完整位于屏内。它只保存已绘制的 RGB565 像素，不加载贴图或文件。省略 key 保存不透明区域；指定 key 时压缩每行两侧透明边距，并预编译非透明连续段。透明捕获先计数，再直接分配最终 VM userdata；分配可能运行 finalizer，因此随后重新验证 Display 与区域边界，使用当前 framebuffer 和 stride，在无分配的扫描中检查容量并复制像素。每行首尾之间的 key 像素仍保留，普通及不同 key 回放语义不变。若 finalizer 改变了所需像素数或连续段数，丢弃未发布结果，回退一次到完整 VM 快照再压缩；不保留跨分配的 framebuffer 指针，不无限重试。finalizer 自身绘制和重开 Display 的副作用保留，关闭或缩小到无法容纳区域时抛错；捕获本身不绘制，OOM 不发布半成品，所有临时与最终存储均计入 VM 配额。reuse 要求尺寸和透明模式一致；透明快照还要求 RGB565 色键一致，否则抛错。不透明复用，或已有像素和连续段容量足够的透明复用，会在无分配、无 finalizer 回调的扫描中原地更新并返回同一 userdata；所有别名都观察到新内容，容量不会因内容缩小而减少。透明容量不足时走上述普通分配路径并返回新快照，旧快照保持不变，OOM 也不会破坏旧内容（用户 finalizer 显式修改旧对象的副作用除外）。调用者必须接住返回值；公共层不自动创建缓存池或预留额外容量。`display.masked_region_reuse == true` 表示支持此合同，旧模块不提供该字段；它与 width/height 一样是信息字段，修改 Lua 表字段不会开关原生能力。重新捕获当前不透明背景会使恢复基线失效，下次完整恢复。

`display.draw_region(region,x=0,y=0,top=0,bottom=height,key=nil,left=0,right=width)` 按原生像素尺寸重放，不缩放。整数 x/y 范围为 ±100000；整数裁剪边界构成屏内半开矩形。省略 key 表示不透明重放，包括还原压缩时省略的边距颜色；不同的重放 key 同样正确还原捕获内容。重放 key 等于捕获 key 时直接复制预编译连续段。空裁剪不写像素。颜色 getter 完成后检查设备状态，错误参数不造成绘制写入，但 getter 自身副作用仍属于 Lua 行为。

`display.restore_background(region)` 仅接受完整屏幕、不透明快照，并保留 VM 引用。首次绑定或恢复基线失效时完整复制；之后把所有绘制操作标记的 16×16 脏 tile 合并成相邻行段，仅恢复这些区域，然后清空背景损伤标记。背景恢复与上一帧提交是独立状态，不能用“已提交”代替“已恢复”。`display.release_background()` 幂等解除引用；快照本身仍可重放，最后一个引用释放后由 GC 回收。

`display.present(options=nil)` 和 `end_frame(options=nil)` 返回实际提交的 `(pixel_count, rectangle_count)`。options 是普通表，字段用 raw lookup 读取：`retained` 为 boolean，显式启用或禁用上一成功帧比较，省略则沿用当前模式；`bounds` 为 boolean，当前调用合并为一个包围矩形；`merge_gap` 为 `0..8` 整数，限制 span/tile 局部合并之间可跨过的未变化 tile 数（仍按 16 像素网格衡量）；这是局部合并上限，不保证一定合并，自动包围矩形/整屏候选不受该上限限制。retained 首帧或失效后完整提交，之后先完成 dirty 范围内的像素比较和区域规划，再提交变化区域，完全静止时返回 `(0,0)`。`tiles=true` 显式跳过精细 span 规划，直接精确比较 16×16 tile，再使用原有 `merge_gap` 与 guard 合并；省略或 false 保持默认。该选项仅影响当前调用的 retained 规划，用可能增加的提交像素换取更少规划 CPU，不保证总完成时间更短。`bounds=true` 保留原来的 tile 对齐包围矩形；默认规划可返回更紧的像素边界，调用方不能依赖固定 tile 大小或固定矩形数量。即使没有像素变化，也调用 PAL present 并传播其错误。任何 draw/present 失败都使提交基线失效，并按 worker 合同停止新提交、保留故障资源；部分成功的矩形不能作为完整成功帧。切换 retained 模式完整提交一次，再使用相应的比较或 dirty union 规划；发送缓冲 B 保留到 Display 成功释放。这些统计是软件提交量，不是实机 FPS。

快照、背景损伤标记、retained 比较图、矩形规划暂存区和基线像素都计入 VM 内存，原工作 framebuffer 保留 PAL ownership。Lua deinit、job release 和 Host teardown 在释放 framebuffer 或执行 VM finalizer 前断开全部显示缓存引用。teardown 期间不能重新打开 Display；正常 deinit 后旧 proxy 的绘制调用失败。OOM 不返回部分快照，释放其他 VM 数据后可重试；已有快照不因另一次捕获失败而失效。

默认 retained 规划先提取每行连续变化 span，按至多 16 个额外像素的增长限制连接相邻行，每 16 行对已有矩形做一次有界合并。span 生成沿用 `面积 + 256 × 矩形数 + 32 × sum(ceil(矩形高度 / 16))` 的内部启发式：最多 128 个矩形、4096 个原始 span、65536 次矩形合并检查；纵向连接逐 span 检查至多 128 个已有矩形。达到上限或 8×8 采样至少 56 点变化时，完整回退原有 tile 比较与合并。采样不能省略需要提交的变化像素。

完成原计划后，仅遍历其矩形一次累计提交面积、估算分块数和包围框。自动 guard 使用每分块 `222..286` 像素等价的经验成本范围：令 `ΔP` 为包围框面积减原计划累计面积，`ΔB` 为包围框的 `ceil(height/16)` 减原计划累计分块数，仅当 `ΔP + k × ΔB < 0` 对整个范围成立时，才替换为包围框；同成本保留原计划。实现只检查最坏端点，不用浮点，也不把减少 PAL 调用数计作收益。该范围由约 `160..200 µs/块` 与 `0.70..0.72 µs/像素` 的经验区间比值向外取整，属于候选敏感性范围，不能当作统计置信区间或 PAL 保证。不同后端的像素转换、窗口设置、分块高度和等待策略可能超出此范围；保守比较不保证所有后端更快，也不保证实际总 present 时间下降。

保留原 span 或完整 tile 计划作为基准，不因为另一个候选耗尽工作预算而丢弃它。guard 不再次比较 framebuffer、不改变原计划的像素覆盖或失败语义。tile fallback 没有 128 个矩形的提交上限：若超出暂存容量且 guard 拒绝包围框，则从已有 tile 字节图重放完整原计划，不提交部分前缀、不强制扩大面积。只有原 span 规划失败才进行原有 tile 像素比较；容量溢出后的重放只遍历标记，额外成本仍应在目标设备测量。`bounds=true` 直接沿用原 tile 对齐包围框。

矩形存储和计数占 1028 字节，随 VM 计费的 worker mailbox 一次分配；发送缓冲 B 的尾部保存每 tile 一个比较/遍历字节。原有每 tile 一个比较字节保留 damage 位与遍历位；guard 的面积、分块和包围框汇总只使用固定大小栈变量，不增加常驻缓存或每帧分配。热 present 不分配，不在 native heap 保存额外缓存；Display 成功释放后沿用 VM GC 生命周期。规划完成后由 Display worker 复制并调用 PAL；发送缓冲同时作为 tentative baseline，只有整次 PAL present 成功才有效，失败后遵循提交任务的故障隔离合同。

#### 提交规划的验证

从仓库根目录运行 `bazel test --config=macos_arm64 //libs/lua:all`（Linux 使用对应 config）。`display_plan_test` 对照原始 tile 合并规则，生成空心菱形移动、平移矩形、稀疏噪点、稠密帧、无变化和交替棋盘格，并从旧 framebuffer 按提交矩形回放，逐像素检查最终结果；另覆盖随机 dirty box、奇数尺寸、4096 轴边界、容量耗尽、256 矩形完整 fallback、显式 bounds、guard 拒绝/同成本原样保留及 64 位面积累计，并检查经验成本范围的两个端点均不劣于同帧完整原计划。`lua_test` 通过真实 Lua present/PAL fake 检查首帧、清除旧位置、guard 成功、完整 fallback、热路径无分配和单份发送缓冲的 VM 计费/释放；独立 fault suite 检查部分 draw、present、静止帧及超过容量的 fallback 中途失败后的资源隔离。源包测试验证该规划器随 portable runtime 导出。

可直接运行 `bazel-bin/libs/lua/display_plan_test` 输出通用合成对比；可选 `--frames <path> [first last]` 读取本地 240×240、无 header、RGB565 big-endian 连续全帧，帧号从 1 开始，前序帧仍用于建立 baseline。该入口只用于离线验证，不是 Lua/PAL API；私有轨迹不得提交到公共仓库。时间以同一帧规划 10 次的均值采样，再统计 p50/p95/max，仅包括差分和规划（含回退），不包括绘制、baseline copy、PAL 提交或真实总线等待。对比统一采用全屏 dirty 候选和 `merge_gap=1`，与消费端每次实际传入的 dirty/options 可能不同。静止合成场景仍比较全屏；实际没有 dirty 的 present 会直接跳过规划。

合成输出对比原始 `tiles`、默认 `selected` 和显式 `direct-tiles` 三种计划，`fallback` 统计原 span 规划失败后走完整 tile 路径的帧数。像素回放和成本约束则对照同帧、同 dirty/options 下 guard 前的完整 span/tile 基准。`direct-tiles` 逐像素回放验证最终帧，并以同帧完整原始 tile 计划检查 guard 成本，不要求优于默认精细 span 计划。后处理 guard 可以跨过局部 `merge_gap`，但必须通过整个经验范围的严格获益判断；保留的原计划仍可能不是模型中的全局最优解。

像素减少可能伴随更多窗口和较高规划成本，不能把 host 时间或提交面积直接换算成 MCU FPS。消费端应测量目标设备的 planner、每窗口/分块固定成本、baseline copy 和实际传输时间，再判断是否值得保留当前成本权重；未经设备验证不承诺帧率。算法参考 [fbcp-ili9341 的 exact scanline diff 与 span merge](https://github.com/juj/fbcp-ili9341/blob/master/diff.cpp) 及 [LVGL 的 lv_refr_join_area](https://github.com/lvgl/lvgl/blob/v9.2.2/src/core/lv_refr.c)，两者均声明 MIT 许可（[fbcp](https://github.com/juj/fbcp-ili9341/blob/master/LICENSE.txt)、[LVGL](https://github.com/lvgl/lvgl/blob/v9.2.2/LICENCE.txt)）；此处独立实现有界规划，没有引入它们的平台代码或依赖。

### Display 保留几何与 native 更新

`display.compile_mesh(vertices, primitives, vertex_capacity=#vertices, primitive_capacity=#primitives)` 创建 VM 所有的保留几何；`display.update_mesh(handle, vertices, primitives)` 在固定容量内替换所有活动数据。顶点是 `{x,y}`，primitive 是 `{kind,first,count,color}`；kind 0 为 3..128 顶点多边形，kind 1 为两顶点线段。Lua 的 first 从 1 开始，允许重叠的连续顶点范围。最多 65536 顶点、4096 primitives；坐标必须有限且在 ±1000000 内。空数据和零容量合法。Lua 更新先在 VM 临时存储中完整解码，任何后续参数错误都不会让本次更新只写入一部分；成功后替换活动长度、拓扑、颜色并使派生坐标失效。调用方颜色 getter 自身的副作用仍按 Lua 语义执行。

`display.draw_mesh(handle, options=nil)` 按原顺序绘制。options 的字段使用 raw lookup，缺省值不从元表获取：

| 字段 | 合同 |
| --- | --- |
| `matrix` | `{a,b,c,d,tx,ty}`，默认单位矩阵；计算 `x'=(a*x+c*y)+tx`、`y'=(b*x+d*y)+ty`，字段有限且在 ±1000000 内 |
| `transform` | 可选 `{x=...,y=...,scale=...,angle=...}`，四项均必填且 raw 读取；与显式 matrix 互斥。x/y/angle 有限且在 ±100000 内，`0<scale<=100`，必须显式提供 1..16 的 grid |
| `grid` | matrix 路径为整数 0..16，默认 0，不吸附；非零时用 `floor(value/grid+0.5)*grid` 吸附变换后的顶点。transform 路径为必填整数 1..16；选择策略由应用提供 |
| `offset_x` | 有限且在 ±100000 内，默认 0；多边形在交点取整后横移，线段在连续裁剪前横移，不与 matrix 平移合并 |
| `left,top,right,bottom` | 默认整个 framebuffer 的整数半开裁剪矩形，范围必须完全位于 framebuffer 内；空矩形合法 |
| `color` | 可选 Display 颜色覆盖，不修改保留颜色 |
| `cache` | boolean，默认 false；按需保留有序扫描段/线记录，最多 8192 条；初始 16..512 条，按需增长，重放后收缩到实际条数 |

所有派生顶点在光栅化前验证为有限且在 ±16000000 内。多边形沿用上述 even-odd 扫描和交点取整规则；线先连续裁剪再按 `floor(endpoint+0.5)` 取整并执行 Bresenham。绘制不隐式 present，关闭 Display 后拒绝绘制。派生顶点缓存以内容更新、matrix／transform 和 grid 为失效条件；裁剪、颜色、offset 每次绘制应用，不能因坐标缓存命中而跳过。可选 span 缓存还比较裁剪、viewport、offset 和 recolor，命中时按原顺序重放并标记 dirty/background damage；容量溢出仍完整绘制，但不发布部分缓存。成功的 native/Lua 更新要求重新验证派生坐标，但保留上一次成功绘制的 span 候选。完整验证后，只有活动顶点／primitive 数量、primitive 类型／范围／颜色、最终坐标和上述绘制参数全部相同时才能复用；不能只比较地址或包围盒。连续更新、失败调用和不保留缓存的绘制不会覆盖候选快照，相同输入的热调用复用已验证的比较结果。首次启用缓存且使用恒等变换时，按每个多边形裁剪后的整数行数乘以 `floor(边数/2)`、每条线一条记录求保守上界，初始容量限制在 16..512 条；最多检查 128 个多边形顶点，达到预算或上界时保留 512 条，其他变换也保持 512 条。同时按声明容量分配一份顶点／primitive 快照及对齐／固定元数据。估算只影响分配提示；分配期间 finalizer 改变 mesh 或 viewport 后仍执行原验证与溢出处理。一次光栅化超出容量时本帧仍完整绘制但不发布缓存，下一次绘制在入口把缓存换成 4 倍容量（最多 8192 条）。缓存至少被重放一次后，若剩余空位不少于 256 条，下一次绘制在入口把它换成恰好容纳现有记录的缓存，复制记录与快照；收缩后的缓存若再次溢出，直接恢复为 8192 条并不再收缩，动画网格不会反复分配。增长、收缩或恢复都只在入口分配；分配引起的 finalizer 重入若改变了缓存，则放弃替换并沿用重入留下的缓存。此后热绘制不分配。数据和缓存计入 VM；引用释放后由 GC 或 VM teardown 回收。

显式 transform 保留 `(x+(vx*cos(angle)-vy*sin(angle))*scale)/grid` 及 y 对应式的运算顺序，不预乘为 affine matrix。三角函数在参数不变时复用。每轴使用原版 float 表达式和 `64*FLT_EPSILON` 误差界：远离半格点且误差小于 0.25 时走 float 取整，否则使用原版 double 表达式；源顶点超出 ±100000 时也回退。应用决定 grid、pose、布局和复用时机；库内没有尺寸阈值或额外输出缩放。

精确单位矩阵且 `grid=0` 时，绘制直接读取 mesh 自有、已在创建／更新阶段验证的顶点，不再复制到派生坐标缓存或逐点重复计算单位变换，也不借用调用方缓冲区。既有派生坐标存储仍为一般变换保留，不增加容量或分配。此快路径不使用近似比较；非单位矩阵或非零 grid 仍在修改坐标缓存和像素前完整验证变换结果，错误、像素和缓存失效合同不变。

非 identity 变换在活动顶点不超过 1024 时，可使用独立的 Display 共享暂存区一次计算、完整验证后提交。暂存区按 `min(vertex_capacity,1024)*16` 字节 payload 加 userdata 开销计入 VM，多个 mesh 复用容量；首次使用或增长容量可能分配，增长期间旧、新分配可能同时存活。Display release 或 VM teardown 释放共享引用。活动顶点超过 1024 时仍使用原来的验证／变换两遍路径，公开 65536 顶点上限不变。暂存区不借用源顶点、已提交位置或完整 span 候选；所有可能重入的分配之后重新读取 mesh 与 Display 状态，完整验证到发布之间不分配、不回调。失败不撤销用户 finalizer 自身的合法修改，也不得用外层旧状态覆盖它们。

私有 native 计算模块通过生产公共头 `h2_lua_display.h` 创建／更新同一种 userdata，再交给 `display.draw_mesh`。C API 的完整参数、错误和 ownership 合同见从该头 Doxygen 生成的 [Lua Display API Reference](/references/lua)；native indices 从 0 开始，不同于 Lua 表。native 更新不分配、不增长 Lua stack，调用方预留两个空栈槽；创建通过受保护的 Lua 调用处理 OOM，失败恢复栈。它们不打开 Display、不绘制、不暴露内部存储地址，模块不得获取 job/framebuffer 或 include runtime 私有头。鱼身变形、场景投影、分色、网格选择等策略由应用先计算。

## 来源

Source: [guides/zh/developing/lua.md](https://github.com/GizClaw/gizos/blob/81e8cbfc64760b5bcc89e8be771393662e876ea5/guides/zh/developing/lua.md#L114), revision `81e8cbfc64760b5bcc89e8be771393662e876ea5`.
