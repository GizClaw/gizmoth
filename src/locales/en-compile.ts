export const enCompile: Record<string, string> = {
  编译: "Compiling",
  "请在桌面应用中使用编译服务。": "Use compiling in the desktop app.",
  编译任务: "Builds",
  缓存: "Caches",
  产物: "Artifacts",
  启动虚拟机: "Starting VM",
  同步源码: "Syncing sources",
  编译中: "Building",
  收集产物: "Collecting artifacts",
  成功: "Succeeded",
  启动中: "Starting",
  正在停止: "Stopping",
  出错: "Error",
  "排队第 {position} 位": "#{position} in queue",
  "{running} 个编译中 · {queued} 个排队": "{running} building · {queued} queued",
  "在本机的 Linux 虚拟机中编译项目。外部 agent 通过 WebSocket 提交编译，断开连接即取消；缓存和产物由所有 slot 共享。":
    "Build projects in Linux virtual machines on this computer. External agents submit builds over a WebSocket, and disconnecting cancels the build. All slots share the caches and artifacts.",
  "演示模式：编译、缓存和产物仅模拟，不会启动虚拟机或修改本机文件。":
    "Demo mode: builds, caches and artifacts are simulated. No virtual machine starts and no local files change.",
  "正在读取编译服务…": "Loading the compile service…",
  "无法读取编译服务：{error}": "Couldn't read the compile service: {error}",
  "此电脑暂时不能运行编译虚拟机。": "This computer can't run compile VMs yet.",
  查看进度: "View progress",

  // System image
  "Linux 系统镜像": "Linux system image",
  已就绪: "Ready",
  正在准备: "Preparing",
  准备失败: "Preparation failed",
  尚未准备: "Not prepared",
  镜像准备进度: "Image preparation progress",
  "{packages} 个软件包 · 占用 {size} · 构建于 {time}": "{packages} packages · {size} used · built {time}",
  "上次准备已取消。": "The last preparation was cancelled.",
  重新构建镜像: "Rebuild image",
  现在准备: "Prepare now",
  镜像日志: "Image log",

  // Slots
  "添加 slot": "Add slot",
  "最多同时编译 {count} 个 · 本机 {cpus} 核 {memory} GB 内存":
    "Up to {count} builds at once · this computer has {cpus} cores and {memory} GB memory",
  "所有 slot 同时运行时共需 {need} GB 内存，超过本机的 {have} GB。":
    "Running every slot at once needs {need} GB of memory, more than this computer's {have} GB.",
  "清空 {name} 的工作盘": "Clear {name}'s work disk",
  "将停止虚拟机并删除工作盘：源码副本、Bazel 输出目录、本地 ccache 和 home 会在下次编译时重新创建。共享缓存和产物不受影响。":
    "The VM stops and its work disk is deleted. The source copy, Bazel output directory, local ccache and home are recreated on the next build. Shared caches and artifacts are not affected.",
  清空工作盘: "Clear work disk",
  "将停止虚拟机并删除这个 slot 的工作盘。共享缓存和产物会保留。":
    "The VM stops and this slot's work disk is deleted. Shared caches and artifacts are kept.",
  "还没有 slot。添加一个 slot 后才能编译。": "No slots yet. Add a slot to start building.",
  "{active} 个进行中 · 最近 {count} 个": "{active} active · {count} recent",
  取消编译: "Cancel build",
  "取消「{title}」？提交它的 agent 会收到已取消的结果。":
    "Cancel “{title}”? The agent that submitted it receives a cancelled result.",
  "还没有编译任务。agent 通过 compile_pipe 找到编译 WebSocket 并发送 build 请求后，会在这里排队并显示日志。":
    "No builds yet. Once an agent finds the compile WebSocket with compile_pipe and sends a build request, it queues here with its log.",
  "{cpus} CPU · {memory} GB 内存": "{cpus} CPU · {memory} GB memory",
  "虚拟机已启动，等待编译{time}": "VM running, waiting for builds{time}",
  "上次使用：{time}": "Last used: {time}",
  "编译 slot": "Build slots",
  有编译任务时自动启动虚拟机: "The VM starts automatically when a build arrives",
  工作盘: "Work disk",
  "{name} 工作盘用量": "{name} work disk usage",
  停止虚拟机: "Stop VM",
  "先取消这个 slot 上的编译。": "Cancel the build on this slot first.",
  "名称需要 1–40 个字符。": "Names need 1–40 characters.",
  "内存需要在 1–{max} GB 之间，并为 macOS 保留至少 1 GB。":
    "Memory must be 1–{max} GB, leaving at least 1 GB for macOS.",
  "工作盘需要在 8–4096 GB 之间。": "The work disk must be 8–4096 GB.",
  "编辑 {name}": "Edit {name}",
  "每个 slot 是一台独立的 Linux 虚拟机，同一时间运行一个编译任务。工作盘保存源码副本和 Bazel 输出，跨编译保留。":
    "Each slot is its own Linux VM and runs one build at a time. Its work disk keeps a source copy and Bazel output between builds.",
  "{count} 核": "{count} cores",
  内存: "Memory",
  "停止虚拟机后才能修改 CPU 和内存。": "Stop the VM to change CPU and memory.",
  "工作盘按需占用空间。清空工作盘后才能修改它的大小。": "The work disk grows as needed. Clear it to change its size.",

  // Builds
  超时: "Timed out",
  "信号 {signal}": "Signal {signal}",
  "已等待 {duration}": "Waiting {duration}",
  工作区: "Workspace",
  命令: "Command",
  工作目录: "Working directory",
  提交: "Submitted",
  结束: "Finished",
  退出码: "Exit code",
  同步: "Sync",
  "{files} 个文件更新 · {bytes} · {removed} 个删除": "{files} files updated · {bytes} · {removed} removed",
  产物规则: "Outputs",
  "{count} 条规则没有匹配到文件": "{count} patterns matched no files",
  查看产物: "View artifact",
  "只显示最新的日志。": "Showing the newest log only.",
  "等待输出…": "Waiting for output…",
  "没有日志。": "No log.",
  "无法读取日志：{error}": "Couldn't read the log: {error}",

  // Caches
  "Bazel 远程缓存": "Bazel remote cache",
  自定义缓存: "Custom cache",
  "编译动作结果（AC/CAS），各 slot 的 Bazel 自动使用":
    "Action results (AC/CAS), used automatically by Bazel in every slot",
  "GCC/Clang 编译结果，所有 slot 共享 · 每个 slot 另有 {local} GB 本地缓存":
    "GCC/Clang results shared by all slots · plus {local} GB local cache per slot",
  "编译命令通过环境变量 {env} 获得缓存地址": "Builds receive its address in {env}",
  添加自定义缓存: "Add custom cache",
  在访达中显示: "Show in Finder",
  "共 {size}，保存在工作目录": "{size} in total, stored in the working directory",
  "已用 / 上限": "Used / limit",
  命中率: "Hit rate",
  不限: "Unlimited",
  "{name} 用量": "{name} usage",
  "命中 {hits} · 未命中 {misses} · 上传 {uploads}": "{hits} hits · {misses} misses · {uploads} uploads",
  "清空 {name}": "Clear {name}",
  "将删除这个缓存的全部内容（{size}）。之后的编译会重新生成并上传结果。":
    "Everything in this cache ({size}) is deleted. Later builds regenerate and upload their results again.",
  "缓存通过各虚拟机内的 127.0.0.1:7421 访问，数据保存在本机，不会上传到其他电脑。命中次数从服务本次启动开始统计；超过上限时自动删除最久未使用的条目。":
    "VMs reach the caches at 127.0.0.1:7421. Data stays on this computer and is never uploaded elsewhere. Hit counts start when the service starts; over a limit, the least recently used entries are deleted.",
  "将删除这个缓存的全部内容并从设置中移除。使用它的环境变量不会再提供给编译命令。":
    "Everything in this cache is deleted and the cache is removed from settings. Builds no longer receive its environment variable.",
  "名称只能使用小写字母、数字和连字符，最多 32 个字符。":
    "Names can use lowercase letters, digits and hyphens, up to 32 characters.",
  "已有同名缓存。": "A cache with this name already exists.",
  "环境变量名只能使用字母、数字和下划线，且不能以数字开头。":
    "Variable names can use letters, digits and underscores, and can't start with a digit.",
  "上限需要是 0 或正整数（0 表示不限）。": "The limit must be 0 or a whole number (0 means unlimited).",
  "本地缓存至少需要 1 GB。": "The local cache needs at least 1 GB.",
  "{name} 设置": "{name} settings",
  "各 slot 的 Bazel 通过 --remote_cache 共享编译结果。修改会在空闲虚拟机下次启动时生效。":
    "Bazel in every slot shares results through --remote_cache. Changes apply when idle VMs next start.",
  "ccache 先查 slot 自己工作盘上的本地缓存，再查所有 slot 共享的缓存。":
    "ccache checks the slot's local cache on its work disk first, then the cache shared by all slots.",
  "GizMoth 为编译命令提供一个 HTTP 键值缓存，地址通过你指定的环境变量传入，可用于 sccache、Gradle 等工具。":
    "GizMoth gives builds an HTTP key-value cache whose address arrives in the variable you choose, for tools such as sccache or Gradle.",
  变量名: "Variable",
  上限: "Limit",
  "每个 slot": "Per slot",
  "上限为 0 表示不限制；超过上限时删除最久未使用的条目。":
    "A limit of 0 means unlimited. Over the limit, the least recently used entries are deleted.",

  // Artifacts
  搜索产物: "Search artifacts",
  "{count} 个 · {size}": "{count} · {size}",
  "无法读取产物：{error}": "Couldn't read artifacts: {error}",
  "还没有产物。编译请求带上 artifacts 规则（例如 bazel-bin/**/*.update.tar.zlib），成功后会在这里保存。":
    "No artifacts yet. Builds that pass artifacts patterns (for example bazel-bin/**/*.update.tar.zlib) save their outputs here when they succeed.",
  已固定: "Pinned",
  "这个产物没有 .update.tar.zlib 固件包。": "This artifact has no .update.tar.zlib firmware package.",
  "安装到设备…": "Install on device…",
  取消固定: "Unpin",
  删除产物: "Delete artifact",
  "将删除「{title}」的 {count} 个文件（{size}）。": "Delete {count} files ({size}) from “{title}”.",
  "产物 ID": "Artifact ID",
  创建时间: "Created",
  "没有匹配的产物。": "No matching artifacts.",
  "自动保留最近 {keep} 个、最多 {size}；固定的产物不会被自动删除。":
    "Keeps the newest {keep} artifacts, up to {size}. Pinned artifacts are never deleted automatically.",
  "安装 {title}": "Install {title}",
  安装: "Install",
  "再显示 {count} 个": "Show {count} more",
  固定: "Pin",
  复制完整路径: "Copy full path",
  "复制 {file} 的完整路径": "Copy the full path of {file}",
  "复制产物 ID": "Copy artifact ID",
  "所在 GizMoth 未连接": "Its GizMoth is disconnected",
  "不是 H2Loader 设备": "Not an H2Loader device",
  正在执行任务: "Busy with a job",
  "已开始在「{port}」上安装 {file}。": "Started installing {file} on “{port}”.",
  安装到设备: "Install on device",
  "通过 GizMoth 管理的串口安装产物中的固件包。过程中请勿断开设备。":
    "Install the artifact's firmware package through a serial port GizMoth manages. Keep the device connected.",
  目标串口: "Serial port",
  "这个串口暂时不能安装：{reason}。": "This port can't install right now: {reason}.",
  "还没有可用的串口。在串口页面添加设备后再安装。":
    "No serial ports yet. Add the device on the Serial ports page first.",
  前往串口: "Open Serial ports",

  // Settings
  "镜像源需要是 http 或 https 地址。": "The mirror must be an http or https URL.",
  "空闲停止时间需要在 1–1440 分钟之间。": "Idle shutdown must be 1–1440 minutes.",
  "环境变量格式应为 NAME=value：{line}": "Environment variables must be NAME=value: {line}",
  "不同步规则需要是工作区内的相对路径：{pattern}": "Exclusions must be relative paths inside the workspace: {pattern}",
  "保留数量需要在 1–10000 之间。": "Keep between 1 and 10000 artifacts.",
  "产物总大小需要是 0 或正整数。": "The total size must be 0 or a whole number.",
  系统镜像: "System image",
  虚拟机: "Virtual machines",
  虚拟机联网: "VM network access",
  "关闭后编译不能下载依赖；源码同步和共享缓存不受影响。":
    "Without it, builds can't download dependencies. Source sync and shared caches still work.",
  空闲后停止虚拟机: "Stop idle VMs after",
  "保持虚拟机运行可以复用 Bazel 的分析结果，停止后释放内存。":
    "A running VM reuses Bazel's analysis; stopping it frees memory.",
  分钟: "min",
  编译环境: "Build environment",
  "Bazel 配置": "Bazel options",
  "每行一条，追加到虚拟机的 /etc/bazel.bazelrc。": "One per line, appended to /etc/bazel.bazelrc in the VM.",
  环境变量: "Environment variables",
  "每行一个 NAME=value，提供给所有编译命令。": "One NAME=value per line, given to every build command.",
  不同步的路径: "Paths not synced",
  "每行一条工作区内的规则，支持 *、? 和 **。根目录的 bazel-* 链接总是跳过。":
    "One workspace pattern per line, with *, ? and **. The root bazel-* links are always skipped.",
  产物保留: "Artifact retention",
  保留最近的产物: "Keep recent artifacts",
  "超过数量时删除最早的产物，固定的产物除外。":
    "Beyond this count the oldest artifacts are deleted, except pinned ones.",
  个: "",
  产物总大小上限: "Total artifact size limit",
  "0 表示不限制大小。": "0 means no size limit.",
  未保存的更改: "Unsaved changes",
  "有未保存的更改。": "You have unsaved changes.",
  还原: "Revert",
  // Settings › 编译
  "编译虚拟机的系统和工具。": "The system and tools of the build VMs.",
  "Linux 系统镜像的软件包是固定的，开发包来自 GizMoth 的工具链目录。两者都会自动下载并保持更新，不需要手动选择。":
    "The Linux image has a fixed package list, and development packages come from GizMoth's toolchain registry. Both download and stay up to date on their own; there is nothing to choose.",
  "演示模式：下载和打包仅模拟，不会启动虚拟机或修改本机文件。":
    "Demo mode: downloads and packing are simulated and never start VMs or change files on this computer.",
  "现在下载 Linux 系统镜像和开发包，第一次编译就不用等待。之后会自动保持更新。":
    "Download the Linux image and development packages now so the first build doesn't wait. They stay up to date afterwards.",
  准备编译环境: "Prepare build environment",
  "Ubuntu 软件源": "Ubuntu package source",
  "下载镜像软件包的位置。留空使用 Ubuntu 官方源，国内网络可以使用就近的镜像；更换后不需要重新构建。":
    "Where the image's packages download from. Leave empty for Ubuntu's official archive, or use a nearby mirror; changing it needs no rebuild.",
  "第一次编译前会自动准备 Ubuntu 系统盘，大约需要 1–2 分钟并需要联网。":
    "The Ubuntu system disk is prepared automatically before the first build. It takes about 1–2 minutes and needs the internet.",
  "GizMoth 更新了系统镜像，没有编译时会自动重新构建。":
    "GizMoth has a newer system image; it rebuilds automatically when no builds are running.",
  镜像与开发包: "Image and packages",
  开发包: "Development packages",
  "{platform} 的全部工具链和 SDK 会自动下载并保持更新，以只读磁盘挂到每个 slot 的 {path}。":
    "Every {platform} toolchain and SDK downloads and stays up to date automatically, mounted read-only at {path} in every slot.",
  "检查更新失败：{error}": "Checking for updates failed: {error}",
  "目录 {commit} · 检查于 {time}": "Registry {commit} · checked {time}",
  还没有读取目录: "Registry not read yet",
  "已打包 {size} · {time}": "Packed {size} · {time}",
  "正在打包：{phase}": "Packing: {phase}",
  下载完成后会重新打包: "Repacks when downloads finish",
  系统镜像就绪后打包: "Packs once the system image is ready",
  "打包失败：{error}": "Packing failed: {error}",
  还没有开发包: "No packages yet",
  等待下载: "Waiting",
  正在下载: "Downloading",
  等待打包: "Waiting to pack",
  正在打包: "Packing",
  已安装: "Installed",
  失败: "Failed",
  暂不支持: "Not supported yet",
  "{count} 个开发包，只读磁盘": "{count} packages, read-only disk",
  // Settings › Storage
  存储: "Storage",
  "请在桌面应用中选择工作目录。": "Choose the working directory in the desktop app.",
  "选择 GizMoth 存放数据的工作目录。": "Choose where GizMoth keeps its data.",
  "编译用的系统镜像、build slot 工作盘、共享缓存和产物都放在这里。可以选择外接硬盘或空间更大的磁盘，更改时现有数据会一起移过去。":
    "The build system image, slot work disks, shared caches and artifacts live here. Choose an external or larger disk; existing data moves with it.",
  "演示模式：移动只是模拟，不会读写本机文件。":
    "Demo mode: moves are simulated and never touch files on this computer.",
  "正在读取工作目录…": "Reading the working directory…",
  "无法读取工作目录：{error}": "Could not read the working directory: {error}",
  "找不到工作目录 {path}。请连接它所在的磁盘，或选择其他位置；新位置会从空目录开始。":
    "The working directory {path} is missing. Connect its disk, or choose another location; the new location starts empty.",
  "已取消移动，数据仍在原来的位置。": "Move cancelled. The data is still in its original location.",
  "移动失败，数据仍在原来的位置：{error}": "The move failed; the data is still in its original location: {error}",
  默认位置: "Default location",
  "可用 {size}": "{size} free",
  "更改…": "Change…",
  恢复默认位置: "Restore default location",
  "Slot 工作盘": "Slot work disks",
  共享缓存: "Shared caches",
  下载文件: "Downloads",
  "工作盘和系统镜像是稀疏文件，只按实际写入的数据占用空间。项目和设置仍保存在应用数据目录中。":
    "Work disks and the system image are sparse files and only use the space they have written. Projects and settings stay in the app data folder.",
  "正在移动到 {path}": "Moving to {path}",
  移动进度: "Move progress",
  "正在关闭空闲的虚拟机…": "Stopping idle VMs…",
  "已复制 {done} / {total}": "Copied {done} of {total}",
  "正在移动…": "Moving…",
  "新的编译会排队等待移动完成。": "New builds wait until the move finishes.",
  更改工作目录: "Change working directory",
  "系统镜像、slot 工作盘、缓存和产物（共 {size}）会移动到：":
    "The system image, slot work disks, caches and artifacts ({size}) will move to:",
  "原来的工作目录不可用，GizMoth 会在这里重新开始：":
    "The current working directory is unavailable. GizMoth will start fresh in:",
  "同一磁盘内会直接移动；换到其他磁盘需要复制，可能需要几分钟。空闲的虚拟机会先关闭，移动期间新的编译会排队等待。":
    "On the same disk the data moves instantly; another disk needs a copy that can take a few minutes. Idle VMs stop first, and new builds wait during the move.",
  "还有 {count} 个编译在进行或排队，等它们结束后才能移动。":
    "{count} builds are running or queued. Move after they finish.",
  恢复并移动: "Restore and move",
  默认: "Default",
  正在构建: "Building",
  尚未构建: "Not built yet",
  "{count} 个 slot": "{count} slots",
  "{count} 个缓存": "{count} caches",
  "{count} 个产物": "{count} artifacts",
  镜像和开发包的原始下载: "Original downloads of the image and packages",
  "在访达中显示 {name}": "Show {name} in Finder",
  还没有内容: "Nothing here yet",
  更改位置: "Change location",
  移动: "Move",
};
