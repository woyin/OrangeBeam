# Orange Beam（橙现）

原名 Spotlight RS。

用 Rust 实现的 macOS 原生演示效果工具，目标是为第一代 Logitech Spotlight 提供轻量开源替代。

**当前为 1.0 测试版（1.0-beta.2）。** 第一代 Spotlight 蓝牙聚光已通过实机验收：跟随移动、按住静止保持、松开隐藏。USB、实时放大和完整演示场景尚未验收，不能视为已经完成的 Options+ 替代品。

范围已确定：**macOS Apple Silicon（ARM64）、第一代 Spotlight、蓝牙优先且保留 USB 接收器支持**。Windows 与 Spotlight 2 不在当前范围内。

## 已实现

- 原生菜单栏和控制面板，关闭控制面板后可从菜单栏的灯光图标再次打开。
- 聚光：中心透明，周围压暗，可调大小与暗度。
- 数字激光：红色光点，跟随鼠标。
- 放大镜：支持 1.5×、2×、3×、4× 放大，已接入 ScreenCaptureKit 采集；排除本程序窗口，避免递归。
- 方框高亮：第一次按住顶键瞄准并松开定下起点角，第二次按住拖动拉出矩形；翻页、切换特效或按 `Esc` / `⌃⌥⌘H` 清除。
- 双击顶键循环切换已勾选特效（切换时短暂闪现确认）；长按上一页/下一页可配置为从当前页播放（Keynote / PowerPoint）、屏幕变黑/恢复、结束放映或课堂倒计时。
- 演讲计时器（最多 3 个分钟定时点，到点遥控器振动，可选屏幕提醒）与电池电量/充电状态显示。
- 10 秒预览、全局快捷键 `⌃⌥⌘H` 立即隐藏，以及停留效果（黑屏、方框、手动显示、预览）下的 `Esc` 清除。
- 点击穿透、不成为关键窗口的覆盖层；按显示器逻辑坐标定位，检测显示器及缩放变化。
- 第一代设备枚举、HID++ 功能/电量/映射查询、限时输入采集和临时按键通知诊断。
- 应用启动时优先自动连接蓝牙 Spotlight，未找到设备或断连后自动重试。顶键按住/松开独立于鼠标移动，保留系统原生光标和前后翻页。
- 课堂倒计时：把“长按上一页”或“长按下一页”设为“课堂倒计时”后，长按进入设定，短按翻页键加减分钟，停止操作 3 秒自动开始；到点振动一次，并在所有显示器显示“时间到”。设定期间临时接管两个短按键，结束后恢复并读回校验。没有遥控器时用菜单栏的“课堂倒计时…”。

**验证范围：** 当前 66 项自动测试通过，其中含实机顶键样本回放，覆盖约 10.9 秒没有移动时仍保持按住状态。用户已确认蓝牙聚光的跟随、静止保持、松开隐藏三项正常；日志对应覆盖窗口显示数 `1 → 0`。蓝牙功能、电量、映射、前进键，以及应用连接、断开和同进程重连也已有实机证据。USB、物理断连/睡眠恢复、连续放大、课堂倒计时实机验收、全屏和多屏共享仍待验证。详细记录见 [VALIDATION.md](docs/VALIDATION.md)。

## 直接试用

解压 `Orange-Beam-macos-arm64.zip`，打开 `Orange Beam.app`（中文系统中显示为“橙现”）。本次产物用于本地测试，采用 ad-hoc 签名，尚未做 Developer ID 签名和公证。

1. 蓝牙或 USB 连接第一代 Spotlight，打开应用；应用会自动连接并在控制面板和菜单栏显示连接状态与电量。未找到设备或断连后会自动重试；退出应用即释放遥控器。
2. 菜单栏灯光图标中选择特效（聚光、数字激光、实时放大、方框高亮），或双击顶键切换已勾选特效；按住顶键并移动遥控器，保持静止不会隐藏，松开后结束。前后键继续由 macOS 处理。
3. `⌃⌥⌘H` 随时立即隐藏（含黑屏并取消课堂倒计时）；黑屏、画好的方框、手动显示或预览停留时也可按 `Esc` 清除，其余时间 `Esc` 留给演示软件。注册失败会在控制面板提示，仍可使用菜单。
4. “实时放大”需要“屏幕录制”权限，长按翻页键发送播放/结束快捷键需要“辅助功能”权限。按控制面板提示授权后重新启动；聚光、激光与方框无需屏幕录制权限。
5. 无遥控器时，先在菜单栏选择特效，再点击控制面板的“预览当前特效”并移动鼠标；10 秒后结束。
6. 课堂倒计时：控制面板里把“长按上一页”或“长按下一页”设为“课堂倒计时”。放映中长按该键，中央窗口显示上次的分钟数；短按下一页加 1 分钟、上一页减 1 分钟，停止操作 3 秒后开始倒数，最后 10 秒为红色。到 0 时遥控器振动一次并显示“时间到”。运行中长按再次停止。没有遥控器时使用菜单栏的“课堂倒计时…”，行为相同，只是不能用翻页键调整分钟数。

“立即隐藏”也会取消当前顶键触发，直到松开再重新按住；遥控器松开不会取消用户单独开启的手动显示。应用正常退出或收到终止信号时恢复已接管的按键配置；意外中断遗留的接管记录会在下次启动时自动接回并恢复。设备读取失败时清除遥控器触发状态，并自动重连。

全局快捷键尚未在最终包完成桌面验收：打包后 Mac 已锁定。不要同时启动多个开发/发布副本，避免抢占快捷键或混淆系统权限对应的应用。

屏幕内容只用于本机放大，不保存、不上传。唯一的网络请求是检查更新：读取 GitHub 上公开的发布列表，不发送任何个人信息，不自动下载或安装；频率可在控制面板设为不自动检查、每天、每周或每月（默认每周），也可随时手动检查。没有 WebView、账号或开机启动项。效果关闭后停止屏幕采集，空闲时不持续高频绘制。

最低部署版本设置为 macOS 13；当前仅在 macOS 27 Apple Silicon 编译和验证，尚未建立旧版本兼容矩阵。屏幕共享时，“整个显示器”和“单个窗口”需要分别测试；单窗口共享不能保证包含独立覆盖层。

## 从源码构建

需要 Apple Silicon Mac、Rust stable 和 Xcode Command Line Tools。

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo run -- app
sh scripts/bundle.sh
```

应用生成到 `dist/Orange Beam.app`。可以给打包脚本传入输出目录。`Cargo.lock` 固定依赖版本；许可证和第三方声明随包携带。更新依赖后，用 `cargo metadata` 与 `scripts/collect-licenses.py` 更新第三方声明（仅此维护脚本需要 Python 3 和联网）。

## 安装

从 [Releases](https://github.com/woyin/OrangeBeam/releases) 下载 `Orange-Beam-macos-arm64.zip`，解压后把 `Orange Beam.app` 放进「应用程序」。

也可以用 Homebrew 安装，tap 仓库为 [woyin/homebrew-orangebeam](https://github.com/woyin/homebrew-orangebeam)，每次发布后会自动更新到新版本：

```sh
brew install --cask woyin/orangebeam/orange-beam
```

应用为 ad-hoc 签名、未经公证，首次打开会被 Gatekeeper 拦下：在「系统设置 → 隐私与安全性」中点「仍要打开」，或执行 `xattr -dr com.apple.quarantine "/Applications/Orange Beam.app"`。随后按需授权输入监控（读取遥控器）、辅助功能（长按翻页发送播放快捷键）和屏幕录制（实时放大），控制面板会显示这三项状态。

## 自动构建

每次推送到 `main` 或提交 Pull Request，GitHub Actions（`.github/workflows/build.yml`）会自动构建，产物在对应运行的 Artifacts 中下载：

- `Orange-Beam-macos-arm64.zip`：完整应用，macOS Apple Silicon；先经过格式、Clippy、测试和离屏绘制检查。ad-hoc 签名，未公证。
- `orange-beam-cli-linux-x86_64.tar.gz`、`orange-beam-cli-windows-x86_64.zip`：仅命令行诊断工具（`list`、`probe`、`inspect`、`capture`、`watch-controls`、`watch-page-keys`、`vibrate` 等）。图形界面依赖 AppKit，只有 macOS 版本。

推送 `v*` 标签（如 `v0.3.0`）时，全部构建成功后会自动创建 GitHub Release 并附上这些文件。

## 无设备检查

```sh
cargo run -- demo spotlight 10
cargo run -- demo laser 10
cargo run -- render-fixtures /tmp/spotlight-render-qa
```

`render-fixtures` 使用真实覆盖层绘制代码和合成画面，生成三张效果图并检查透明度。它不截取桌面、不需要屏幕录制权限，不能代替实时采集验收。示例图见 [聚光](docs/qa/spotlight.png)、[激光](docs/qa/laser.png)、[放大](docs/qa/magnify.png)。

设置环境变量 `ORANGE_BEAM_TRACE_FRAMES=1` 可测量每帧耗时：效果结束时向 stderr 输出一行，含帧间隔和每帧工作时间的 p50、p95、最大值。`demo` 模式下到期退出前也会输出。在 Apple Silicon、release 构建下实测，聚光 `work_ms` 的 p50 约 0.02 ms、p95 约 0.02–0.03 ms，帧间隔 p50 为 16.7 ms（60 Hz）。

## 设备验证

```sh
cargo run -- list
# 0 是示例：使用 list 给出的实际接口序号。
cargo run -- probe 0
cargo run -- inspect 0
cargo run -- capture 0 20 > spotlight-input.log
```

只匹配第一代设备 `046d:c53e`（USB 接收器）和 `046d:b503`（蓝牙）。设备识别规则来自 Projecteur，不等于本项目已通过这两种连接的实机测试。

`probe` 选择厂商自定义接口（usage page 通常为 `ff00`），动态查询功能索引，默认槽位 1；可用 `probe DEVICE INDEX` 指定 1–6。查询超时即退出，不把超时解释为功能不支持，也不做设备配置、按键接管、震动或固件操作。

`inspect` 使用相同接口/槽位参数，只读查询电量与当前按键映射，输出能力和转发标志的十六进制值；不修改映射。已在第一代蓝牙设备读取到电量和 7 个控制项。电量读数来自设备报告，不能代表实测续航。

`capture` 只读取选定的 Spotlight 接口，可能需要为启动终端/程序授予“输入监控”。分别录制顶键短按、按住移动、按住静止、松开、双击和前后键短按/长按。鼠标、键盘和 HID++ 接口可能需要分别录制。重新连接后重新执行 `list`，接口序号可能变化。

`watch-top DEVICE [SECONDS] [INDEX]` 用于顶键状态诊断，默认等待 30 秒、槽位 1，最多 300 秒。它验证顶键能力后，只临时开启 `00d8` 的按住/松开通知，不启用 raw XY、不改持久设置或映射。检测到一次完整按住/松开后自动结束；超时或收到 SIGINT / SIGTERM / SIGHUP 也会结束。退出时恢复原值并读回校验。实机固件的空配置回复已兼容，成功与否以独立状态查询为准。蓝牙查询使用槽位 `01`，通知地址使用实测的 `ff`；USB 按接收器槽位区分。诊断命令只输出数据，正常 `app` 模式负责屏幕效果。

`watch-page-keys DEVICE [SECONDS] [INDEX]` 为课堂倒计时的接管做阶段 0 验证：只临时接管两个短按键 `00d9`、`00db`，默认 60 秒，同时输出解码后的按下/松开事件和期间收到的键盘报文（report ID `01`）。接管成功时不出现键盘报文、幻灯片也不翻页；出现 `KEYBOARD` 行说明设备仍在发送方向键。结束时恢复并读回校验。

若顶键已被其他软件接管，命令会退出并保持原状。配置和恢复依赖设备仍连接且接口可用；SIGKILL、断电、断连或安全输入导致访问受限时无法保证恢复。临时标志不会因为进程消失就必然复位，遇到 `RESTORE FAILED` 应保留日志并重新查询状态，不能当作成功退出。

`recover-controls-default DEVICE [INDEX]` 覆盖 `00d8`、`00df`、`00da`、`00dc`、`00d9`、`00db`，每项只在原值为 `0000` 且映射到自身时才接受。

`recover-top-default DEVICE [INDEX]` 仅供已中断会话的显式恢复：必须已有日志证明会话前顶键是 `flags=0000 mapped_to=00d8`，且其他设备控制软件已停止。它只接受当前顶键仍为自身映射、无其他转发标志的状态，清除临时 divert 并读回校验。应用不会自动调用此命令，也不会借此覆盖其他软件的配置。

采集前必须在连接设备的 Mac 上解锁进入桌面，并离开密码输入框。macOS 安全输入开启时，`probe` / `capture` 会立即说明原因并退出；采集中出现这一状态也会停止。输入监控授权不能解除锁屏期间的限制。

## 接下来

1. 完成课堂倒计时阶段 0 短按键接管验证（`watch-page-keys`）与实机放映验收。
2. 继续验证激光、方框高亮、立即隐藏快捷键、全屏和鼠标点击穿透；蓝牙聚光的核心三项已通过。
3. USB 实机重复验证，并完成物理断连、睡眠恢复和全屏演示测试。
4. 验证真实屏幕放大、外接屏和会议共享。

[实施计划](docs/PLAN.zh-CN.md) · [协议与 API 依据](docs/PROTOCOL.md) · [验证记录](docs/VALIDATION.md)

MIT。与 Logitech 无隶属关系，不包含官方应用代码、固件或素材。依赖声明见 [THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt)。
