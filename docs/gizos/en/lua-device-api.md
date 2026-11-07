# Lua device APIs

This guide explains the Lua interfaces used by a graphical GizOS application. API names are the same in the desktop simulator and on a device, provided its firmware supplies the corresponding Runtime capabilities.

## Modules and application lifetime

```lua
local runtime = require("runtime")
local display = require("display")
local system = require("system")
```

The entry file is a top-level Lua chunk. Returning ends the job. An interactive application keeps the chunk alive with a yielding loop. `require("display")` acquires the screen and may raise when the capability is unavailable; a successful package check does not prove this firmware can display or run the app.

## Drawing a frame

- `display.width`, `display.height`: numeric framebuffer dimensions; use them instead of hard-coded screen sizes.
- `display.clear(color)`: fill the frame background.
- `display.fill_rect(x, y, width, height, color)`: fill a rectangle.
- `display.draw_line(x0, y0, x1, y1, color)`: draw a line.
- `display.fill_circle(cx, cy, radius, color)` / `display.draw_circle(...)`: filled or outlined circles.
- `display.draw_text(x, y, text, options)`: draw text; options include `color` and `font_size`.
- `display.present()`: submit the frame and wait for transport completion. This reports submission, not optical scanout.

Colors may be named `black`, `white`, `red`, `green`, or `blue`, or supplied as `{ r = 0, g = 128, b = 255 }` with integer channels from 0 to 255.

```lua
display.clear("black")
display.fill_rect(8, 8, 40, 40, "blue")
display.draw_text(8, 56, "Hello GizOS", { font_size = 14 })
display.present()
```

For advanced retained drawing and asynchronous frame submission, read the drawing and asynchronous Display guides.

## Physical buttons

The launcher assigns component IDs and passes arguments as strings. Convert an available argument with `tonumber`; never guess a numeric ID or invent a name-lookup API.

- `runtime.components.get(id)` returns a Button proxy, or `nil, error`.
- `button.get_key_level()` returns 1 while pressed or 0 otherwise; handle an unavailable result.
- `runtime.components.on(id, event_kind, callback)` subscribes and returns a handle.
- `runtime.components.off(handle)` removes a subscription.
- Event kinds include `runtime.event.BUTTON_DOWN`, `BUTTON_UP`, and `BUTTON_ACTION`.

The host must forward input events into the job. Down events may repeat while held. For a single initial Down edge, check `event.timestamp_ms == event.pressed_at_ms`. Up events are useful for toggles:

```lua
local ok = tonumber(args and args.ok)
if ok then
  runtime.components.on(ok, runtime.event.BUTTON_UP, function()
    print("OK released")
  end)
end
```

## Timing and frame loops

`system.millis()` reads monotonic time. `runtime.sleep(ms)` yields the coroutine until its deadline, allowing input and other work to progress; `require("delay").delay_ms(ms)` also yields. Avoid busy loops. Host timeouts and memory limits are firmware policy.

```lua
while true do
  local started = system.millis()
  display.clear("black")
  display.draw_text(8, 8, "Running", { font_size = 14 })
  display.present()
  runtime.sleep(math.max(1, 33 - (system.millis() - started)))
end
```

See the screen/button example and the complete Flappy Bird example for application code. Adapt their entry and asset paths to your own `app_id`; optional Touch and Audio must be checked against the target's capabilities.

## Sources

These explanations are grounded in GizOS revision `75deb77229d3fb07dd10d39683c69009b1eb45e9`:

- [Runtime, button and timing bindings](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/lua/src/modules/h2_lua_modules.c)
- [Display bindings](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/libs/lua/src/modules/h2_lua_display.c)
- [Official button example](https://github.com/GizClaw/gizos/blob/75deb77229d3fb07dd10d39683c69009b1eb45e9/projects/example/targets/pkg_tar/lua-script-input/input.lua)
