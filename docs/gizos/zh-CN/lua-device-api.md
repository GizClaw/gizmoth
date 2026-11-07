# Lua 设备接口

本指南说明图形化 GizOS 应用会用到的 Lua 接口。只要固件提供相应的 Runtime 能力，同一组 API 名称就能用于桌面模拟器和设备。

## 模块与应用生命周期

```lua
local runtime = require("runtime")
local display = require("display")
local system = require("system")
```

入口文件是顶层 Lua chunk，执行返回即结束任务。交互应用应通过会让出执行权的循环保持运行。`require("display")` 会取得屏幕能力；能力缺失时可能抛出错误。打包检查通过，不代表这份固件能够显示或运行应用。

## 绘制一帧

- `display.width`、`display.height`：数值型 framebuffer 尺寸，不要把特定屏幕大小写死。
- `display.clear(color)`：填充背景。
- `display.fill_rect(x, y, width, height, color)`：绘制实心矩形。
- `display.draw_line(x0, y0, x1, y1, color)`：绘制线段。
- `display.fill_circle(cx, cy, radius, color)` / `display.draw_circle(...)`：绘制实心圆或圆轮廓。
- `display.draw_text(x, y, text, options)`：绘制文字，选项包括 `color` 和 `font_size`。
- `display.present()`：提交画面并等待传输完成；提交结果不等同于屏幕已完成扫描显示。

颜色可用 `black`、`white`、`red`、`green`、`blue`，也可用 `{ r = 0, g = 128, b = 255 }`，各通道为 0–255 的整数。

```lua
display.clear("black")
display.fill_rect(8, 8, 40, 40, "blue")
display.draw_text(8, 56, "Hello GizOS", { font_size = 14 })
display.present()
```

更复杂的保留绘图和异步画面提交，请阅读绘图与异步 Display 指南。

## 实体按键

组件 ID 由启动器分配，参数以字符串传入。对已有参数使用 `tonumber`，不要猜数字 ID，也不要编造按名称查找组件的接口。

- `runtime.components.get(id)`：返回 Button 代理，或者 `nil, error`。
- `button.get_key_level()`：按住时为 1，否则为 0；需要处理不可用的返回结果。
- `runtime.components.on(id, event_kind, callback)`：订阅事件并返回句柄。
- `runtime.components.off(handle)`：取消订阅。
- 事件包括 `runtime.event.BUTTON_DOWN`、`BUTTON_UP` 和 `BUTTON_ACTION`。

宿主必须向任务投递输入事件。按住时可能反复产生 Down 事件；只处理首次按下时，检查 `event.timestamp_ms == event.pressed_at_ms`。切换状态通常可以用 Up 事件：

```lua
local ok = tonumber(args and args.ok)
if ok then
  runtime.components.on(ok, runtime.event.BUTTON_UP, function()
    print("OK released")
  end)
end
```

## 计时与帧循环

`system.millis()` 读取单调时钟。`runtime.sleep(ms)` 让出当前 coroutine，等待截止时间，让输入和其他任务有机会推进；`require("delay").delay_ms(ms)` 也会让出执行权。不要使用无等待的忙循环。任务超时与内存上限由固件决定。

```lua
while true do
  local started = system.millis()
  display.clear("black")
  display.draw_text(8, 8, "Running", { font_size = 14 })
  display.present()
  runtime.sleep(math.max(1, 33 - (system.millis() - started)))
end
```

绘图与按键示例、完整 Flappy Bird 示例提供了应用代码。使用时将入口和资源路径适配为自己的 `app_id`；可选的触摸和音频功能仍需确认目标设备具备对应能力。

## 来源

接口说明依据 GizOS 修订版本 `75deb77229d3fb07dd10d39683c69009b1eb45e9`：

- [Runtime、按键与计时绑定](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/lua/src/modules/h2_lua_modules.c)
- [Display 绑定](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/lua/src/modules/h2_lua_display.c)
- [官方按键示例](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/projects/example/targets/pkg_tar/lua-script-input/input.lua)
