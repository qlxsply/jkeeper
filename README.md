# CIS Cluster Manager

Windows 11 x64 本地 Java 微服务桌面管理器，使用 Rust + Slint 原生界面。配置只在启动时读取；不使用 Python、浏览器、WebView、Node.js 或后台服务。

## 运行与构建

交付目录：

```text
release/
  cis-cluster-manager.exe
  conf
  conf.example
  README.md
```

双击 exe。程序从 **exe 所在目录** 读取 `conf`，与当前工作目录无关。修改配置后需退出并重新打开管理器，刷新按钮只刷新运行状态。

开发环境：Rust Stable、Windows SDK、Visual Studio C++ Build Tools（MSVC 链接器）。使用仓库中的 `Cargo.lock`：

```powershell
cargo build --release --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1
```

打包脚本将现有 `conf` 复制到 `release`（仅在目标不存在时），不会修改根目录或覆盖已有发布配置。Release 使用 Windows GUI 子系统和静态 MSVC CRT。程序运行不要求 Rust 或 JDK GUI 框架；启动 Java 服务仍需用户配置的 Java。

直接执行 `cargo run` 时，请自行在 `target/debug` 放置 `conf`。不会从源码目录隐式回退读取配置。

## 配置

完整示例见 `conf.example`。当前项目原有 `conf` 未被修改。

```toml
[general]
manage_dir = 'D:\cis-cluster'
java_path = 'D:\Java\jdk8\bin'
min_memory = '64m'
max_memory = '128m'
pid_file = '{app}.pid'
start_check_seconds = 2
force_kill_timeout_seconds = 5
log_dir = 'logs'
log_viewer = 'notepad.exe'

[[apps]]
name = 'pay'
source_jar = 'D:\build\pay.jar'
# jar_name = 'pay.jar'
# java_path = 'D:\Java\jdk21\bin\java.exe'
# min_memory = '64m'
# max_memory = '256m'
# log_file = './logs/witlink-cis-pay.log'
```

- 服务列表仅来自 `[[apps]]`，按应用名称排序，不扫描其他目录。
- 应用目录固定为 `manage_dir/<name>`。不存在的目录仍显示错误，不自动创建业务目录或 Spring 配置。
- JAR 默认为 `<name>.jar`；`jar_name` 只接受文件名。
- `manage_dir`、Java 路径、`source_jar` 的相对路径以 **conf 所在目录** 为基准。
- 全局 `log_viewer` 是可执行文件路径，不是 shell 命令。默认 `notepad.exe`；自定义相对路径以 conf 目录为基准。日志路径通过独立参数传递，可包含空格。
- 应用 `log_file` 支持绝对路径；相对路径以 **JAR 的父目录** 为基准。省略时使用 `logs/witlink-cis-{name}.log`。
- `log_dir` 仅控制 Java stdout/stderr 的保存目录，相对于应用目录；与业务日志 `log_file` 不同。
- 每个应用可以覆盖 Java 路径及最小/最大堆。未配置 `source_jar` 仍可启动、停止、重启，但不可更新。
- 堆内存支持正整数字节数或 `k/m/g`（不区分大小写）；最小堆不得大于最大堆。
- 等待时间支持小数秒，范围 0～3600。当前停止方式是强制终止，因此旧文件里的 `stop_timeout_seconds` 和 `[commands]` 不参与行为。
- 首次配置读取/验证失败时显示错误窗口；修正后重新启动程序。

## 操作规则

### 启动与重启

启动前检查 Java、JAR、`application.yml`、`bootstrap.yaml`。缺失时直接失败。实际命令：

```text
java.exe -Xms64m -Xmx128m -Dfile.encoding=utf-8
  -jar D:\cis-cluster\pay\pay.jar
  --spring.config.location=file:./application.yml,file:./bootstrap.yaml
```

工作目录为应用目录，不显示控制台窗口。stdin 为空设备，stdout/stderr 分别追加至 `logs/console.out.log`、`logs/console.err.log`（目录由 `log_dir` 决定）。

存活检查仅确认进程没有立即退出，**不代表 Spring 初始化完成或业务已就绪**。重启先验证启动条件，条件不足时不会先停止原服务；已停止应用的重启等效于启动。

### 停止与更新

- 停止前核验 Java 可执行文件、绝对 JAR 参数和精确进程创建时间。通过同一进程句柄终止并等待退出，避免 PID 重用误杀。
- 使用 `TerminateProcess` 强制停止，明确记录“强制停止”，不会声称执行了优雅关闭。
- 运行时更新按钮置灰；后台也会重新确认停止状态。用户先停止，再更新。
- 更新将来源复制到目标目录的独立临时文件，检查复制长度和来源是否发生变化，刷新磁盘后保留旧包为 `*.jar.bak`，最后通过 Windows 原子替换更新目标。
- 复制/替换失败保留原目标，更新完成后不启动应用。备份保留一份，不提供版本历史或自动启动回滚。

### 互斥、批量与退出

- 所有管理操作通过原子任务门控制，一个批量操作作为一个任务顺序执行。
- 同一规范化 `manage_dir` 使用 Windows 全局命名互斥对象，只允许一个可操作实例；重复实例显示原因并禁用操作。
- 批量按名称排序，跳过不适用项，单个错误不阻止其他应用；逐项记录，不显示计数汇总。
- “全选”选中配置中的全部应用；批量操作按应用名称顺序执行。
- 任务期间仍可滚动、监控及打开日志，其余管理操作禁用。
- 列表行固定高度，字段左对齐并垂直居中；错误说明不占用列表行，统一显示在窗口底部状态栏。
- 刷新与批量操作按钮并排；按钮统一高度和样式，宽度按两字/四字标签区分。
- 任务期间点击关闭，状态栏显示“当前任务完成后退出”，整个已提交批量任务执行完再退出；不弹确认框。
- 空闲退出不会停止服务。重新启动管理器可按绝对 JAR 路径重新识别其进程。

### 日志与状态

- 每行固定显示启动、停止、重启、更新、日志五个按钮。
- 日志按钮启动配置的外部程序；文件不存在只显示错误，不创建空日志。
- 不内置业务日志阅读器。界面“操作记录”只保留最近 100 条管理记录。
- 管理器日志在 `%LOCALAPPDATA%\CisClusterManager\logs\manager-<PID>.log`，每文件 4 MiB 后滚动保留一份备份；不同运行产生不同文件，可按需清理旧文件。
- 每 3 秒使用原生 API 采集进程和网络信息，任务期间继续监控；不启动 PowerShell/tasklist/netstat。
- JVM 列为 `最小堆 / Working Set / 最大堆`；Working Set 不等于堆使用量，可能高于最大堆。停止时实际占用显示 `—`。
- TCP 显示 IPv4/IPv6 LISTEN 端口；UDP 显示绑定的本地端口，协议与端口组合去重。
- JAR 时间优先使用 Spring Boot `META-INF/build-info.properties`（含 `BOOT-INF/classes/` 路径）中的有效 RFC3339 `build.time`；否则使用目标 JAR 的修改时间。不是部署时间，也不会用 ZIP 内最新条目的时间伪造构建时间。

## 进程元数据

应用目录下的 `{app}.pid` 是新工具自己的 JSON，记录 PID、Windows 精确创建时间、JAR 路径和启动堆参数。临时文件加原子替换写入。新工具不兼容旧 Python PID 格式，也不依赖旧脚本。

身份无法确认、PID 文件损坏或同一 JAR 存在多个进程时，显示错误并禁止不安全操作。查询权限不足不会冒充“已停止”。不要在仍有身份不明服务运行时删除 PID 文件以绕过错误。

## 源码结构

| 文件 | 职责 |
|---|---|
| `src/config.rs` | 启动时配置快照、路径与参数校验 |
| `src/process.rs` | Windows 进程句柄、身份校验、终止、目录互斥、原子替换 |
| `src/network.rs` | TCP/UDP IPv4/IPv6 原生端口表 |
| `src/manager.rs` | 状态聚合、启动/停止/重启、PID 与 JAR 部署 |
| `src/app.rs` | 任务互斥、后台调度、事件传递及界面模型 |
| `src/scheduler.rs` | 批量顺序执行与单项异常隔离 |
| `src/jar_time.rs` | 可信构建时间解析及按文件变化缓存 |
| `src/logging.rs` | 管理器日志及线程异常日志 |
| `ui/main.slint` | 深色桌面界面 |
| `src/integration_tests.rs` | 隔离 Java 集成测试 |

Windows API 使用 `windows-sys`，与 `windows` 同属 windows-rs 项目。命令行读取通过 Windows 11 支持的 `NtQueryInformationProcess(ProcessCommandLineInformation)`，封装在 `process.rs`，不读取远程 PEB 布局。

## 验证

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
# 需要 PATH 中存在 java / javac / jar；使用专门的临时 Java 程序
cargo test --locked -- --ignored --test-threads=1
# 50 个真实隔离 JVM 并发监控 + UI 采样；先确保约 1 GiB 可用内存并 Release 已构建
cargo test --locked integration_tests::fifty_live_services_monitoring_stress -- --ignored --exact --nocapture
# Release 窗口、13/50 行、CPU/内存采样及截图，使用 target 下的隔离集群
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke.ps1
# 10 分钟空闲稳定性采样
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke.ps1 -ServiceCounts 13 -Seconds 600
# 只操作 smoke 脚本创建的 test01/test02，测试鼠标操作及任务中关闭
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/ui-test.ps1
```

实际测试范围、性能与限制见 `TESTING.md`。测试不会启动或停止用户 `conf` 指向的真实业务服务。

## 当前范围与限制

已实现上述管理、监控、互斥、外部日志和配置快照功能。未实现 Maven 构建、HTTP 健康检查、优雅退出通道、运行中更新、更新并重启快捷操作、配置热加载、内置日志阅读器或旧 Python 兼容，这些不属于本版确定范围。

仍需在无开发环境 Windows 11 机器及不同 DPI/多显示器环境进一步验收。性能目标必须以实测为准；不把可运行等同于已经达到 40 MB 内存目标。依赖许可按各依赖项目适用条款执行，Slint 的许可不由本仓库 MIT 文件替代。
