# jkeeper

## 一、安装构建依赖

### 1. 准备 Windows

使用 Windows 10 或 Windows 11 x64，并使用有权安装开发工具的 Windows 帐户。构建时需要联网，以下载 Rust 和 Cargo 依赖。

### 2. 安装 Visual Studio C++ Build Tools 和 Windows SDK

Rust 的 `x86_64-pc-windows-msvc` 工具链依赖 Microsoft C/C++ 编译器、链接器和 Windows SDK，因此先安装它们。

1. 从微软官网下载并运行 **Visual Studio 2022 Build Tools** 安装程序：
   <https://visualstudio.microsoft.com/visual-cpp-build-tools/>
2. 在 Visual Studio Installer 中选择工作负载：
   - **Desktop development with C++**（使用 C++ 的桌面开发）
3. 确认安装组件中包含：
   - MSVC v143 x64/x86 C++ build tools（或更新兼容的 MSVC x64 工具集）
   - Windows 10 SDK 或 Windows 11 SDK
   - MSVC x64/x86 Spectre-mitigated libraries 可选，不是本项目必需项
4. 点击安装，完成后重新打开终端。

**检查安装**：从开始菜单打开 **Developer PowerShell for VS 2022**，执行：

```powershell
cl
where.exe link.exe
```

预期 `cl` 显示 Microsoft C/C++ 编译器版本信息，`where.exe link.exe` 找到 Visual Studio 的 x64 linker 路径。普通 PowerShell 中未找到 `cl` 不一定表示未安装；请使用 Developer PowerShell 检查。

### 3. 安装 Rust Stable（MSVC x64）

1. 打开 Rust 官方安装页面：<https://www.rust-lang.org/tools/install>
2. 下载并运行 `rustup-init.exe`。
3. 选择默认安装，或确认默认主机工具链为：
   ```text
   x86_64-pc-windows-msvc
   ```
4. 安装完成后，关闭并重新打开 Developer PowerShell，使 `%USERPROFILE%\.cargo\bin` 加入当前 PATH。

安装器运行后，在 Developer PowerShell 中明确安装并设为默认：

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
rustup default stable-x86_64-pc-windows-msvc
```

也可以在 PowerShell 中使用官方安装入口：

```powershell
winget install --id Rustlang.Rustup --exact
```

如果 winget 未提供该包，请使用 Rust 官网的 `rustup-init.exe` 安装，然后执行上述 toolchain 命令。项目仓库中的 `.cargo\config.toml` 已启用 MSVC 静态 CRT；它不负责安装 C++ 编译器或 Windows SDK。

**检查 Rust 与 target**：

```powershell
rustc --version
cargo --version
rustup show
```

确认 active toolchain/host 是 `stable-x86_64-pc-windows-msvc`。`rustup show` 中没有该 toolchain 时，执行前述 `rustup toolchain install` 和 `rustup default` 命令。

再次确认 MSVC 编译器、链接器及 Windows SDK 工具可见：

```powershell
cl
where.exe link.exe
where.exe rc.exe
```

`cl` 应输出 Microsoft C/C++ 编译器版本，`link.exe` 和 `rc.exe` 应能找到 Visual Studio/Windows SDK 中的文件。若未找到，请通过 Visual Studio Installer 补装对应 C++ workload 和 SDK，然后重开 Developer PowerShell。

### 4. 取得项目源代码

使用 Git：<https://git-scm.com/download/win>，或从代码托管页面下载并解压项目源码 ZIP。进入包含 `Cargo.toml` 的项目根目录。

确认基础文件存在：

```powershell
Test-Path .\Cargo.toml
Test-Path .\Cargo.lock
Test-Path .\build.rs
```

三项都应返回 `True`。

## 二、编译 Release exe

在项目根目录的 Developer PowerShell 中执行：

```powershell
cargo build --release --locked
```

- 首次编译需要联网下载 `Cargo.lock` 中锁定的 crate。
- `--locked` 要求按仓库 `Cargo.lock` 构建，不会悄悄更新依赖锁定版本。
- 本项目启用了 Windows GUI 子系统和静态 CRT。

检查生成结果：

```powershell
Test-Path .\target\release\jkeeper.exe
```

返回 `True` 即表示 Release exe 已生成。它可以在构建电脑上运行，但启动配置由 exe 同目录的 `conf.toml` 决定；只复制 exe 到空目录时不会有可用应用配置。

## 三、打包发布目录

项目提供打包脚本，会再次执行 Release 构建，并准备 `release` 目录：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\package.ps1
```

成功后检查：

```powershell
Test-Path .\release\jkeeper.exe
Test-Path .\release\conf.toml
Test-Path .\release\conf.example.toml
```

发布目录结构：

```text
release\
  jkeeper.exe
  conf.toml
  conf.example.toml
  README.md
```

打包脚本只在 `release\conf.toml` 不存在时复制仓库根目录的 `conf.toml`，不会覆盖已有的目标配置。仓库中的 `conf.toml` 包含本机路径，**发布给其他电脑前应编辑 `release\conf.toml`**，填写目标机的 `manage_dir`、JDK 路径、来源文件绝对路径等设置。

如正在运行的 `release\jkeeper.exe` 被 Windows 锁定，打包脚本会将新版本输出到 `release\jkeeper.next.exe`。关闭旧版后，用该文件替换 `jkeeper.exe`。

## 四、清空编译和打包

在项目根目录的 Developer PowerShell 中执行。请先关闭正在运行的 jkeeper，避免 Windows 锁定 exe。下面的命令会删除 Cargo 编译缓存和 `release` 中的旧发布文件，但**保留 `release\conf.toml`**（如果存在），然后重新编译并打包：

```powershell
cargo clean
if ($LASTEXITCODE -ne 0) { throw 'cargo clean failed.' }

$release = Join-Path (Get-Location).Path 'release'
if (Test-Path -LiteralPath $release) {
    Get-ChildItem -LiteralPath $release |
        Where-Object { $_.Name -ne 'conf.toml' } |
        Remove-Item -Recurse -Force
}

cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\package.ps1
if ($LASTEXITCODE -ne 0) { throw 'Packaging failed.' }
```

如果要连同已编辑的发布配置也重新生成，请在上述命令前手动备份 `release\conf.toml`，并在清理块中移除保留它的 `Where-Object` 筛选。打包后将按脚本规则从项目根目录复制 `conf.toml`。

## 五、在目标电脑运行

把 `release` 目录中的 `jkeeper.exe` 和配置文件一起复制到目标电脑的任意目录；至少需要：

```text
目标目录\
  jkeeper.exe
  conf.toml
```

双击 `jkeeper.exe` 启动。程序从 exe 所在目录读取 `conf.toml`，不依赖当前工作目录。要启动 Java 服务，还需确保 `conf.toml` 配置的 JDK `java.exe`、`source_files` 和 `manage_dir` 中的应用文件在目标机实际存在。

同一 `manage_dir` 同时只允许一个可操作的 jkeeper 实例。重复启动会在创建主窗口前提示并退出。窗口关闭后 Java 服务继续运行。窗口位置和大小会原子保存到 `manage_dir\jkeeper-window.json`；首次运行或状态文件无效时窗口居中打开，保存位置若超出当前虚拟桌面则会限制到屏幕范围内。窗口首次显示后会进行一次轻微重布局，避免恢复尺寸时出现首帧渲染区域不完整。

## 常见检查

- **提示找不到 linker / `link.exe`**：确认已安装 Visual Studio 的 Desktop development with C++、MSVC x64 工具和 Windows SDK；从 Developer PowerShell 运行构建。
- **提示找不到 `conf.toml`**：将 `conf.toml` 放到 `jkeeper.exe` 同目录；文件名必须是 `conf.toml`。
- **配置解析失败**：检查 TOML 语法；VS Code 可通过 `.toml` 扩展名提供语法高亮和格式化。
- **Java 应用启动失败**：确认目标机已安装配置所需的 JDK，`java_path` 指向存在的 `java.exe`。
