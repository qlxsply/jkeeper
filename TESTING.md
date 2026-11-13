# 实测记录

日期：2026-11-13。

## 环境

- Windows 11，版本 `10.0.26300`，12 个逻辑处理器。
- Rust `1.98.1`，Windows MSVC x64；Release、thin LTO、静态 CRT。
- Slint `1.18.1`，winit 后端、software renderer、fluent-dark。
- 测试 Java：OpenJDK `21.0.2` 及 OpenJDK `1.8.0_392`。
- 主机同时运行其他开发软件，因此测试不是专用基准机结果。

## 自动化验证

执行命令：

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo test --locked -- --ignored --test-threads=1
cargo test --locked integration_tests::fifty_live_services_monitoring_stress -- --ignored --exact --nocapture
cargo build --release --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke.ps1 -Seconds 30
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/ui-test.ps1
```

### 单元测试覆盖

- 配置覆盖、排序、相对路径、默认日志路径、重复应用拒绝和配置快照。
- 非法文件名、无效/溢出堆内存。
- 绝对 JAR 参数完整匹配，拒绝同名不同目录和相对名称弱匹配。
- Windows verbatim/UNC 路径规范化。
- 同一集群的跨进程命名对象互斥。
- 20 个线程竞争任务提交，只允许一个成功。
- 批量顺序执行，普通失败与可恢复 panic 后继续剩余应用。
- 部署前检查失败不破坏旧 JAR；成功替换及旧版本备份。
- Spring 构建时间解析、转本地时区、缺失时使用文件修改时间及缓存失效。

### Java 集成测试覆盖

所有 Java 测试使用独立临时目录、自编译的 `tests/fixtures/TestService.java` 和随机系统端口：

- 真正启动 JVM、写入 PID、记录精确创建时间、读取实际 Working Set。
- TCP IPv4/IPv6 和 UDP IPv4/IPv6 端口逐项核对。
- 错误创建时间（PID 重用模拟）与错误 JAR 路径不能通过身份核验。
- 重复启动跳过，运行中更新被拒绝。
- 重启前缺失 Spring 配置时，原 JVM 保持运行。
- 重启产生新进程，强制停止后等待退出并清理 PID。
- stdout/stderr 追加写入真实文件。
- 停止后的 JAR 更新和缺失 JAR 的重新部署。
- 启动器子进程真正退出后 JVM 仍存活；新配置/管理对象能重新识别并停止。
- 丢失 PID 文件时仍可凭绝对 JAR 参数发现进程。
- JDK 8u392：使用该 JDK 的 `javac`/`jar`/`java` 执行同一启动、原生监控、重启及强制停止生命周期，已通过。

50 个真实 JVM 并发时进行 10 次原生扫描（间隔 3 秒），每轮均可靠识别 50 个服务；扫描窗口共 30.54 秒。其间启动 Release GUI 管理器确认跨进程集群锁被占用，结束后确认释放。完整 Slint 窗口在这 30 秒内 Working Set 起始约 50 MiB、结束约 52 MiB，CPU 为 0.972% 单逻辑核。每个测试 JVM 配置 `-Xms32m -Xmx64m`，服务端口动态分配，未操作业务集群。

默认测试中 Java 集成测试带 `ignore`，第二条测试命令显式运行。包含一个只供集成测试子进程调用的 helper；没有测试配置环境变量时 helper 不执行操作。

### 实际窗口测试

`scripts/ui-test.ps1` 使用 Windows 鼠标消息对隔离测试窗口进行操作，已通过：

1. 勾选两个应用并批量启动。
2. 快速重复点击不会提交第二个管理任务。
3. 执行过程中请求关闭，窗口保持到整个批量任务完成。
4. 按名称顺序创建进程。
5. 管理器退出后两个 Java 进程继续存活。
6. 重新打开管理器，通过鼠标批量停止，确认 PID 文件清理。

最终精简版截图位于 `target/smoke-13/window.png`、`target/smoke-50/window.png`。已检查正文标题移除、仅显示工作目录，全选框位于表头首列，表头加高；无搜索框、计时/应用计数，刷新与批量操作同组，按钮高度一致、两字/四字按钮宽度不同；固定表格行左对齐、垂直居中，操作按钮垂直居中，无行内说明文字，底部保留操作记录与异常状态栏。

## 性能数据

最终 30 秒采样使用真实文件的隔离集群，所有测试服务处于**停止状态**。窗口显示、未最小化，约 1400×850 逻辑像素。启动后预热 3 秒，再每秒采样，持续 30 秒；每 3 秒自动刷新状态。

| 配置服务数 | 启动至 InputIdle | Working Set | 采样 Working Set 峰值 | Private Bytes | CPU，单逻辑核为 100% |
|---:|---:|---:|---:|---:|---:|
| 13 | 621 ms | 43.32 MiB | 43.52 MiB | 16.45 MiB | 1.875% |
| 50 | 576 ms | 47.57 MiB | 47.62 MiB | 20.90 MiB | 0.938% |

另对之前的 UI 版做了 13 服务的 10 分钟连续显示窗口测试：窗口始终未最小化，Working Set 43.81 MiB，峰值 44.77 MiB，Private Bytes 16.66 MiB，CPU 0.797% 单逻辑核；测试窗口正常退出。后续 UI 简化移除了搜索输入框、刷新时间和应用计数，完成了构建与鼠标 UI 测试；本报告不把此前 10 分钟数据冒称为新 UI 的长时测量。最新精简 UI 的 3 秒启动后采样为 13 行 40.10 MiB、50 行 43.13 MiB（短采样，不是稳定性能结论）。窗口 DPI 为 96，显示器为 1920×1080。

50 JVM 同时运行时，完整 UI 管理器 Working Set 50→52 MiB，CPU 0.972% 单逻辑核；这与 50 个**停止**服务时的数据分开记录。

CPU 为进程累计 CPU 时间增量除以采样秒数；换算为本机 12 逻辑核整体容量约为 0.156% / 0.078%。服务数更多的一次 CPU 结果反而较低，说明短窗口与主机背景负载影响明显，不应据此推断扩展性曲线。

InputIdle 不是“所有监控数据已加载”的时间，也不是冷启动指标。Working Set 与 Private Bytes 口径不同，不能用 Private Bytes 声称达到内存目标。

**40 MB 以下的空闲 Working Set 目标尚未达到。** 当前实测为约 43～48 MiB。没有使用主动清空 Working Set 等方法美化结果。界面已经使用增量行更新，避免每轮重建整个表格；后台结果直接投递 Slint 事件循环，没有 UI 定时轮询。

## 已知限制与待验收项

- 尚未在无开发环境的全新 Windows 11 机器上验收；已配置静态 CRT，但不把配置等同于干净机器实测。
- 未完成数小时/数天稳定性测试及多显示器全面验收。
- 已在单显示器环境（1920×1080，窗口 DPI 96）验收；未实际切换 125%/150% Windows 缩放，也无第二块物理显示器，相关兼容性需在目标机器验收。
- JDK 8u392 与 JDK 21 生命周期测试已通过；尚未覆盖其他 JDK 8 厂商/发行版。
- 强制停止不保证执行 JVM shutdown hook；这是本版确认的停止方式。
- 外部手动启动、覆盖 JAR 或多个不遵守本工具互斥的程序之间，不提供跨工具事务保证。
- 新 PID 格式不兼容旧 Python 元数据；无法确认的身份会禁止操作，需要先核实实际进程情况。
- 只识别显式绝对 `-jar` 启动的 Java 服务；不管理 `-cp MainClass`、Windows 服务包装器或身份不明进程。
- 构建时间只识别文档列出的 Spring Boot 字段，其他自定义构建时间格式回退到修改时间。
- 日志查看程序按“exe 路径 + 单个日志路径参数”启动，不解析自定义命令模板。

以上限制不会被界面静态数据或模拟成功结果掩盖。
