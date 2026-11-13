#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Windows 11 Java 应用集群交互式运维工具。

约束：
- 配置文件固定读取脚本或 PyInstaller 可执行文件同级目录下的 ``conf``。
- 管理目录、菜单快捷键、默认 JVM 参数和每个应用的 JAR 来源均由 conf 配置。
- 仅使用 Python 标准库，便于 PyInstaller 打包为单文件程序。
- PID 文件采用 JSON 格式，默认位于应用目录内，文件名为 ``<应用名>.pid``。
"""

from __future__ import annotations

import csv
import ctypes
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import unicodedata
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Sequence

PROGRAM_NAME = "CIS Cluster 运维工具"
PROGRAM_VERSION = "1.0.1"
CONF_FILE_NAME = "conf"
PID_SCHEMA_VERSION = 1
MAX_CONF_SIZE = 2 * 1024 * 1024

ACTIONS = ("update", "stop", "start", "restart", "quit", "refresh")
ACTION_LABELS = {
    "update": "更新 JAR 包",
    "stop": "停止应用",
    "start": "启动应用",
    "restart": "重启应用",
    "quit": "退出",
    "refresh": "刷新",
}

CREATE_NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)
CREATE_NEW_PROCESS_GROUP = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)


class OpsError(RuntimeError):
    """可直接展示给用户的业务异常。"""


@dataclass(slots=True, frozen=True)
class GeneralConfig:
    manage_dir: Path
    java_path: Path
    min_memory: str
    max_memory: str
    pid_file: str = "{app}.pid"
    stop_timeout_seconds: float = 15.0
    force_kill_timeout_seconds: float = 5.0
    start_check_seconds: float = 2.0
    log_dir: str = "logs"


@dataclass(slots=True, frozen=True)
class AppConfig:
    name: str
    source_jar: Path | None
    jar_name: str
    java_path: Path | None = None
    min_memory: str | None = None
    max_memory: str | None = None


@dataclass(slots=True, frozen=True)
class Config:
    path: Path
    general: GeneralConfig
    commands: dict[str, str]
    apps: dict[str, AppConfig]

    @property
    def command_to_action(self) -> dict[str, str]:
        return {key.upper(): action for action, key in self.commands.items()}


@dataclass(slots=True)
class ProcessInfo:
    pid: int
    name: str
    executable_path: str = ""
    command_line: str = ""
    start_time: str = ""


@dataclass(slots=True)
class AppStatus:
    index: int
    name: str
    app_dir: Path
    app_config: AppConfig | None
    jar_path: Path
    pid_path: Path
    missing_files: list[str] = field(default_factory=list)
    config_missing: bool = False
    running: bool = False
    pid: int | None = None
    start_time: str = ""
    min_memory: str = ""
    max_memory: str = ""
    ports: list[str] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)
    process: ProcessInfo | None = None

    @property
    def memory_range(self) -> str:
        if not self.min_memory and not self.max_memory:
            return ""
        return f"{self.min_memory or '?'} ~ {self.max_memory or '?'}"

    @property
    def issue_text(self) -> str:
        parts: list[str] = []
        if self.config_missing:
            parts.append("配置缺失")
        if self.missing_files:
            parts.append("缺少文件: " + ", ".join(self.missing_files))
        parts.extend(self.notes)
        return "；".join(parts)


# ---------------------------------------------------------------------------
# 配置读取
# ---------------------------------------------------------------------------


def executable_dir() -> Path:
    """返回脚本或 PyInstaller 可执行文件所在目录。"""
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parent


def expand_path(value: str, base_dir: Path) -> Path:
    expanded = os.path.expanduser(os.path.expandvars(value.strip()))
    path = Path(expanded)
    if not path.is_absolute():
        path = base_dir / path
    return path.resolve()


def require_string(table: dict[str, Any], key: str, section: str) -> str:
    value = table.get(key)
    if not isinstance(value, str) or not value.strip():
        raise OpsError(f"配置 [{section}] 的 {key} 必须是非空字符串")
    return value.strip()


def optional_string(table: dict[str, Any], key: str) -> str | None:
    value = table.get(key)
    if value is None:
        return None
    if not isinstance(value, str):
        raise OpsError(f"配置项 {key} 必须是字符串")
    value = value.strip()
    return value or None


def validate_memory(value: str, label: str) -> str:
    text = value.strip()
    if not re.fullmatch(r"\d+[kKmMgGtT]?", text):
        raise OpsError(f"{label}={value!r} 格式无效，应类似 64m、1g")
    return text


def validate_simple_filename(value: str, label: str) -> str:
    name = value.strip()
    if not name or Path(name).name != name or name in {".", ".."}:
        raise OpsError(f"{label} 必须是单个文件名，不能包含目录")
    return name


def load_config(path: Path) -> Config:
    if not path.exists():
        raise OpsError(f"配置文件不存在: {path}")
    if not path.is_file():
        raise OpsError(f"配置路径不是文件: {path}")
    if path.stat().st_size > MAX_CONF_SIZE:
        raise OpsError(f"配置文件超过 {MAX_CONF_SIZE // 1024 // 1024} MiB，拒绝读取")

    try:
        with path.open("rb") as file:
            raw = tomllib.load(file)
    except tomllib.TOMLDecodeError as exc:
        line = getattr(exc, "lineno", "?")
        column = getattr(exc, "colno", "?")
        raise OpsError(f"conf TOML 语法错误，行 {line}，列 {column}: {exc}") from exc
    except OSError as exc:
        raise OpsError(f"读取配置文件失败: {exc}") from exc

    if not isinstance(raw, dict):
        raise OpsError("conf 顶层结构无效")

    base_dir = path.parent
    general_raw = raw.get("general")
    if not isinstance(general_raw, dict):
        raise OpsError("conf 缺少 [general] 配置段")

    manage_dir = expand_path(require_string(general_raw, "manage_dir", "general"), base_dir)
    java_path = expand_path(require_string(general_raw, "java_path", "general"), base_dir)
    min_memory = validate_memory(
        require_string(general_raw, "min_memory", "general"), "general.min_memory"
    )
    max_memory = validate_memory(
        require_string(general_raw, "max_memory", "general"), "general.max_memory"
    )
    pid_file = validate_simple_filename(
        str(general_raw.get("pid_file", "{app}.pid")), "general.pid_file"
    )
    if "{app}" not in pid_file:
        raise OpsError("general.pid_file 必须包含 {app} 占位符，以免不同应用共用 PID 文件")

    def number_value(key: str, default: float, minimum: float = 0.0) -> float:
        value = general_raw.get(key, default)
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            raise OpsError(f"general.{key} 必须是数字")
        result = float(value)
        if result < minimum:
            raise OpsError(f"general.{key} 不能小于 {minimum}")
        return result

    log_dir = str(general_raw.get("log_dir", "logs")).strip()
    if not log_dir:
        raise OpsError("general.log_dir 不能为空")
    log_path = Path(log_dir)
    if log_path.is_absolute() or ".." in log_path.parts:
        raise OpsError("general.log_dir 必须是应用目录内的相对路径")

    general = GeneralConfig(
        manage_dir=manage_dir,
        java_path=java_path,
        min_memory=min_memory,
        max_memory=max_memory,
        pid_file=pid_file,
        stop_timeout_seconds=number_value("stop_timeout_seconds", 15.0),
        force_kill_timeout_seconds=number_value("force_kill_timeout_seconds", 5.0),
        start_check_seconds=number_value("start_check_seconds", 2.0),
        log_dir=log_dir,
    )

    commands_raw = raw.get("commands")
    if not isinstance(commands_raw, dict):
        raise OpsError("conf 缺少 [commands] 配置段")

    commands: dict[str, str] = {}
    used_keys: set[str] = set()
    for action in ACTIONS:
        key = require_string(commands_raw, action, "commands").upper()
        if len(key) != 1:
            raise OpsError(f"commands.{action} 必须是单个字符")
        if key in used_keys:
            raise OpsError(f"菜单快捷键重复: {key}")
        used_keys.add(key)
        commands[action] = key

    apps_raw = raw.get("apps", [])
    if not isinstance(apps_raw, list):
        raise OpsError("[[apps]] 必须是表数组")

    apps: dict[str, AppConfig] = {}
    for position, item in enumerate(apps_raw, start=1):
        if not isinstance(item, dict):
            raise OpsError(f"第 {position} 个 [[apps]] 配置不是表")
        section = f"apps#{position}"
        name = require_string(item, "name", section)
        if any(ch in name for ch in '<>:"/\\|?*') or name in {".", ".."}:
            raise OpsError(f"应用名称包含 Windows 非法字符: {name}")
        key = name.casefold()
        if key in apps:
            raise OpsError(f"应用配置重复: {name}")

        source_text = optional_string(item, "source_jar")
        source_jar = expand_path(source_text, base_dir) if source_text else None
        jar_name = validate_simple_filename(
            str(item.get("jar_name", f"{name}.jar")), f"{section}.jar_name"
        )
        java_text = optional_string(item, "java_path")
        app_java_path = expand_path(java_text, base_dir) if java_text else None

        app_min = optional_string(item, "min_memory")
        app_max = optional_string(item, "max_memory")
        if app_min:
            app_min = validate_memory(app_min, f"{section}.min_memory")
        if app_max:
            app_max = validate_memory(app_max, f"{section}.max_memory")

        apps[key] = AppConfig(
            name=name,
            source_jar=source_jar,
            jar_name=jar_name,
            java_path=app_java_path,
            min_memory=app_min,
            max_memory=app_max,
        )

    return Config(path=path, general=general, commands=commands, apps=apps)


# ---------------------------------------------------------------------------
# Windows 进程和端口查询
# ---------------------------------------------------------------------------


def run_hidden(
    args: Sequence[str],
    *,
    cwd: Path | None = None,
    timeout: float = 30.0,
    encoding: str = "utf-8",
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(args),
        cwd=str(cwd) if cwd else None,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding=encoding,
        errors="replace",
        timeout=timeout,
        creationflags=CREATE_NO_WINDOW,
        check=False,
    )


def query_java_processes_powershell() -> dict[int, ProcessInfo]:
    script = r"""
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$items = @(
    Get-CimInstance Win32_Process |
    Where-Object { $_.Name -eq 'java.exe' -or $_.Name -eq 'javaw.exe' } |
    ForEach-Object {
        [PSCustomObject]@{
            pid = [int]$_.ProcessId
            name = [string]$_.Name
            executable_path = [string]$_.ExecutablePath
            command_line = [string]$_.CommandLine
            start_time = $(if ($_.CreationDate) { $_.CreationDate.ToString('yyyy-MM-dd HH:mm:ss') } else { '' })
        }
    }
)
ConvertTo-Json -InputObject $items -Compress -Depth 3
""".strip()
    result = run_hidden(
        [
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ],
        timeout=30.0,
    )
    if result.returncode != 0:
        raise OpsError(result.stderr.strip() or "PowerShell 进程查询失败")

    text = result.stdout.lstrip("\ufeff").strip()
    if not text:
        return {}
    try:
        data = json.loads(text)
    except json.JSONDecodeError as exc:
        raise OpsError(f"PowerShell 返回了无法解析的进程数据: {exc}") from exc

    if isinstance(data, dict):
        items = [data]
    elif isinstance(data, list):
        items = data
    else:
        items = []

    processes: dict[int, ProcessInfo] = {}
    for item in items:
        if not isinstance(item, dict):
            continue
        try:
            pid = int(item.get("pid"))
        except (TypeError, ValueError):
            continue
        if pid <= 0:
            continue
        processes[pid] = ProcessInfo(
            pid=pid,
            name=str(item.get("name") or ""),
            executable_path=str(item.get("executable_path") or ""),
            command_line=str(item.get("command_line") or ""),
            start_time=str(item.get("start_time") or ""),
        )
    return processes


def query_java_processes_tasklist() -> dict[int, ProcessInfo]:
    processes: dict[int, ProcessInfo] = {}
    for image_name in ("java.exe", "javaw.exe"):
        result = run_hidden(
            ["tasklist.exe", "/FI", f"IMAGENAME eq {image_name}", "/FO", "CSV", "/NH"],
            timeout=15.0,
            encoding=console_code_page(),
        )
        if result.returncode != 0:
            continue
        for row in csv.reader(result.stdout.splitlines()):
            if len(row) < 2 or row[0].casefold() != image_name.casefold():
                continue
            try:
                pid = int(row[1].replace(",", ""))
            except ValueError:
                continue
            processes[pid] = ProcessInfo(pid=pid, name=row[0])
    return processes


def query_java_processes() -> tuple[dict[int, ProcessInfo], str | None]:
    try:
        return query_java_processes_powershell(), None
    except (OpsError, OSError, subprocess.SubprocessError) as exc:
        fallback = query_java_processes_tasklist()
        return fallback, f"PowerShell 进程详情查询失败，已降级使用 tasklist: {exc}"


def parse_endpoint_port(endpoint: str) -> int | None:
    endpoint = endpoint.strip()
    if not endpoint or endpoint == "*:*" or ":" not in endpoint:
        return None
    try:
        port_text = endpoint.rsplit(":", 1)[1]
        port = int(port_text)
    except (ValueError, IndexError):
        return None
    return port if 0 <= port <= 65535 else None


def query_ports_by_pid() -> tuple[dict[int, list[str]], str | None]:
    try:
        result = run_hidden(["netstat.exe", "-ano"], timeout=30.0, encoding=console_code_page())
    except (OSError, subprocess.SubprocessError) as exc:
        return {}, f"端口查询失败: {exc}"
    if result.returncode != 0:
        return {}, f"端口查询失败: {result.stderr.strip() or 'netstat 返回非零状态'}"

    collected: dict[int, set[str]] = {}
    for raw_line in result.stdout.splitlines():
        parts = raw_line.split()
        if not parts:
            continue
        protocol = parts[0].upper()
        if protocol == "TCP":
            if len(parts) < 5 or parts[3].upper() != "LISTENING":
                continue
            local_endpoint = parts[1]
            pid_text = parts[4]
        elif protocol == "UDP":
            if len(parts) < 4:
                continue
            local_endpoint = parts[1]
            pid_text = parts[-1]
        else:
            continue

        try:
            pid = int(pid_text)
        except ValueError:
            continue
        port = parse_endpoint_port(local_endpoint)
        if port is None:
            continue
        collected.setdefault(pid, set()).add(f"{protocol}:{port}")

    def port_sort_key(value: str) -> tuple[int, int]:
        protocol, port_text = value.split(":", 1)
        return (0 if protocol == "TCP" else 1, int(port_text))

    return {
        pid: sorted(values, key=port_sort_key)
        for pid, values in collected.items()
    }, None


def console_code_page() -> str:
    """返回适合解析 Windows 原生命令输出的编码。"""
    try:
        import locale

        return locale.getpreferredencoding(False) or "mbcs"
    except Exception:
        return "mbcs"


def process_exists(pid: int) -> bool:
    if pid <= 0:
        return False
    if os.name != "nt":
        return False
    PROCESS_SYNCHRONIZE = 0x00100000
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    kernel32.OpenProcess.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = kernel32.OpenProcess(PROCESS_SYNCHRONIZE, False, pid)
    if not handle:
        return False
    kernel32.CloseHandle(handle)
    return True


def wait_process_exit(pid: int, timeout_seconds: float) -> bool:
    if pid <= 0:
        return True
    if os.name != "nt":
        return True

    SYNCHRONIZE = 0x00100000
    WAIT_OBJECT_0 = 0x00000000
    WAIT_TIMEOUT = 0x00000102
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    kernel32.OpenProcess.restype = ctypes.c_void_p
    kernel32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    kernel32.WaitForSingleObject.restype = ctypes.c_ulong
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]

    handle = kernel32.OpenProcess(SYNCHRONIZE, False, pid)
    if not handle:
        return True
    try:
        timeout_ms = max(0, min(int(timeout_seconds * 1000), 0xFFFFFFFE))
        result = kernel32.WaitForSingleObject(handle, timeout_ms)
        if result == WAIT_OBJECT_0:
            return True
        if result == WAIT_TIMEOUT:
            return False
        return not process_exists(pid)
    finally:
        kernel32.CloseHandle(handle)


# ---------------------------------------------------------------------------
# PID 文件
# ---------------------------------------------------------------------------


def read_pid_file(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    if not path.exists():
        return None, None
    try:
        if path.stat().st_size > 1024 * 1024:
            return None, "PID 文件过大"
        with path.open("r", encoding="utf-8-sig") as file:
            data = json.load(file)
    except (OSError, json.JSONDecodeError) as exc:
        return None, f"PID 文件损坏: {exc}"
    if not isinstance(data, dict):
        return None, "PID 文件结构无效"
    return data, None


def atomic_write_json(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as file:
            json.dump(data, file, ensure_ascii=False, indent=2)
            file.write("\n")
            file.flush()
            os.fsync(file.fileno())
        os.replace(temp_name, path)
    except Exception:
        try:
            os.unlink(temp_name)
        except OSError:
            pass
        raise


def safe_unlink(path: Path) -> str | None:
    try:
        path.unlink(missing_ok=True)
        return None
    except OSError as exc:
        return str(exc)


# ---------------------------------------------------------------------------
# 核心运维逻辑
# ---------------------------------------------------------------------------


class ClusterManager:
    def __init__(self, config: Config):
        self.config = config

    def list_app_dirs(self) -> list[Path]:
        root = self.config.general.manage_dir
        if not root.exists():
            raise OpsError(f"管理目录不存在: {root}")
        if not root.is_dir():
            raise OpsError(f"管理路径不是目录: {root}")
        try:
            return sorted(
                (path for path in root.iterdir() if path.is_dir()),
                key=lambda path: path.name.casefold(),
            )
        except OSError as exc:
            raise OpsError(f"读取管理目录失败: {exc}") from exc

    def app_config_for(self, app_name: str) -> AppConfig | None:
        return self.config.apps.get(app_name.casefold())

    def jar_name_for(self, app_name: str, app_config: AppConfig | None) -> str:
        return app_config.jar_name if app_config else f"{app_name}.jar"

    def pid_path_for(self, app_dir: Path, app_name: str) -> Path:
        try:
            filename = self.config.general.pid_file.format(app=app_name)
        except (KeyError, ValueError) as exc:
            raise OpsError(f"general.pid_file 模板无效: {exc}") from exc
        filename = validate_simple_filename(filename, "PID 文件名")
        return app_dir / filename

    def expected_required_files(
        self, app_dir: Path, jar_name: str
    ) -> list[tuple[str, Path]]:
        return [
            (jar_name, app_dir / jar_name),
            ("application.yml", app_dir / "application.yml"),
            ("bootstrap.yaml", app_dir / "bootstrap.yaml"),
        ]

    @staticmethod
    def normalized_command_text(value: str) -> str:
        return value.casefold().replace("/", "\\")

    def process_matches_app(self, process: ProcessInfo, jar_path: Path) -> bool:
        if process.name.casefold() not in {"java.exe", "javaw.exe"}:
            return False
        command = process.command_line.strip()
        if not command:
            # tasklist 降级模式无法获得命令行；PID 文件 + java.exe 作为弱校验。
            return True

        actual = self.normalized_command_text(command)
        absolute_jar = self.normalized_command_text(str(jar_path.resolve()))
        if absolute_jar in actual:
            return True

        # 启动命令在应用目录中以相对 JAR 名运行，例如：-jar gateway.jar。
        # 不能只校验绝对路径，否则刷新时会把正常进程误判成陈旧 PID。
        jar_name = re.escape(jar_path.name.casefold())
        return re.search(
            rf"(?i)(?:^|\s)-jar\s+(?:\"{jar_name}\"|{jar_name})(?=\s|$)",
            command,
        ) is not None

    @staticmethod
    def memory_from_command_line(command_line: str) -> tuple[str, str]:
        min_match = re.search(r"(?i)(?:^|\s)-Xms([^\s\"]+)", command_line)
        max_match = re.search(r"(?i)(?:^|\s)-Xmx([^\s\"]+)", command_line)
        return (
            min_match.group(1) if min_match else "",
            max_match.group(1) if max_match else "",
        )

    def refresh(self) -> tuple[list[AppStatus], list[str]]:
        processes, process_warning = query_java_processes()
        ports_by_pid, port_warning = query_ports_by_pid()
        global_warnings = [warning for warning in (process_warning, port_warning) if warning]

        statuses: list[AppStatus] = []
        for index, app_dir in enumerate(self.list_app_dirs(), start=1):
            app_name = app_dir.name
            app_config = self.app_config_for(app_name)
            jar_name = self.jar_name_for(app_name, app_config)
            jar_path = app_dir / jar_name
            pid_path = self.pid_path_for(app_dir, app_name)
            missing_files = [
                display_name
                for display_name, file_path in self.expected_required_files(app_dir, jar_name)
                if not file_path.is_file()
            ]
            status = AppStatus(
                index=index,
                name=app_name,
                app_dir=app_dir,
                app_config=app_config,
                jar_path=jar_path,
                pid_path=pid_path,
                missing_files=missing_files,
                config_missing=app_config is None,
            )

            pid_data, pid_error = read_pid_file(pid_path)
            if pid_error:
                status.notes.append(pid_error)
                unlink_error = safe_unlink(pid_path)
                if unlink_error:
                    status.notes.append(f"PID 文件清理失败: {unlink_error}")
                else:
                    status.notes.append("无效 PID 已清理")
                statuses.append(status)
                continue
            if not pid_data:
                statuses.append(status)
                continue

            try:
                pid = int(pid_data.get("pid"))
            except (TypeError, ValueError):
                pid = 0
            process = processes.get(pid)
            app_in_pid = str(pid_data.get("app") or "")
            pid_app_matches = not app_in_pid or app_in_pid.casefold() == app_name.casefold()

            if pid <= 0 or process is None or not pid_app_matches or not self.process_matches_app(process, jar_path):
                unlink_error = safe_unlink(pid_path)
                status.notes.append("陈旧 PID 已清理" if not unlink_error else f"陈旧 PID 清理失败: {unlink_error}")
                statuses.append(status)
                continue

            status.running = True
            status.pid = pid
            status.process = process
            status.start_time = process.start_time or str(pid_data.get("start_time") or "")
            status.ports = ports_by_pid.get(pid, [])
            actual_min, actual_max = self.memory_from_command_line(process.command_line)
            status.min_memory = actual_min or str(pid_data.get("min_memory") or "")
            status.max_memory = actual_max or str(pid_data.get("max_memory") or "")

            updated_pid = {
                "schema_version": PID_SCHEMA_VERSION,
                "app": app_name,
                "pid": pid,
                "start_time": status.start_time,
                "min_memory": status.min_memory,
                "max_memory": status.max_memory,
                "ports": status.ports,
                "jar_path": str(jar_path.resolve()),
                "command_line": process.command_line or str(pid_data.get("command_line") or ""),
                "updated_at": now_text(),
            }
            try:
                atomic_write_json(pid_path, updated_pid)
            except OSError as exc:
                status.notes.append(f"PID 文件更新失败: {exc}")
            statuses.append(status)

        return statuses, global_warnings

    def copy_jar(self, status: AppStatus) -> tuple[bool, str]:
        app_config = status.app_config
        if app_config is None:
            return False, f"{status.name}: 配置缺失，无法确定来源 JAR"
        source = app_config.source_jar
        if source is None:
            return False, f"{status.name}: source_jar 未配置"
        if not source.exists():
            return False, f"{status.name}: 复制失败，来源文件不存在: {source}"
        if not source.is_file():
            return False, f"{status.name}: 复制失败，来源路径不是文件: {source}"
        if not status.app_dir.is_dir():
            return False, f"{status.name}: 目标应用目录不存在: {status.app_dir}"

        target = status.jar_path
        target_existed = target.exists()
        temp_target = target.with_name(target.name + ".updating")
        try:
            shutil.copy2(source, temp_target)
            os.replace(temp_target, target)
            action = "覆盖" if target_existed else "复制"
            return True, f"{status.name}: JAR {action}成功 -> {target}"
        except OSError as exc:
            safe_unlink(temp_target)
            return False, f"{status.name}: JAR 更新失败: {exc}"

    def stop_app(self, status: AppStatus) -> tuple[bool, str]:
        if not status.running or status.pid is None:
            safe_unlink(status.pid_path)
            return True, f"{status.name}: 当前未运行"

        pid = status.pid
        # refresh 已进行 JAR 路径校验；停止前再检查一次 PID 是否仍存在。
        if not process_exists(pid):
            safe_unlink(status.pid_path)
            return True, f"{status.name}: 进程已退出，PID 文件已清理"

        try:
            graceful = run_hidden(
                ["taskkill.exe", "/PID", str(pid), "/T"],
                timeout=max(5.0, self.config.general.stop_timeout_seconds),
                encoding=console_code_page(),
            )
        except (OSError, subprocess.SubprocessError) as exc:
            graceful = None
            graceful_error = str(exc)
        else:
            graceful_error = graceful.stderr.strip() or graceful.stdout.strip()

        if wait_process_exit(pid, self.config.general.stop_timeout_seconds):
            safe_unlink(status.pid_path)
            return True, f"{status.name}: 已停止，PID={pid}"

        try:
            forced = run_hidden(
                ["taskkill.exe", "/F", "/PID", str(pid), "/T"],
                timeout=max(5.0, self.config.general.force_kill_timeout_seconds),
                encoding=console_code_page(),
            )
        except (OSError, subprocess.SubprocessError) as exc:
            return False, f"{status.name}: 强制停止失败，PID={pid}: {exc}"

        if wait_process_exit(pid, self.config.general.force_kill_timeout_seconds):
            safe_unlink(status.pid_path)
            return True, f"{status.name}: 已强制停止，PID={pid}"

        details = forced.stderr.strip() or forced.stdout.strip() or graceful_error
        return False, f"{status.name}: 停止失败，PID={pid}: {details or '进程仍然存在'}"

    def effective_start_settings(self, status: AppStatus) -> tuple[Path, str, str]:
        app_config = status.app_config
        if app_config is None:
            raise OpsError(f"{status.name}: 配置缺失，禁止启动")
        java_path = app_config.java_path or self.config.general.java_path
        min_memory = app_config.min_memory or self.config.general.min_memory
        max_memory = app_config.max_memory or self.config.general.max_memory
        return java_path, min_memory, max_memory

    @staticmethod
    def resolve_java_exe(java_path: Path) -> Path:
        if java_path.name.casefold() in {"java.exe", "javaw.exe"}:
            return java_path
        return java_path / "java.exe"

    def validate_start(self, status: AppStatus) -> tuple[Path, str, str]:
        if status.running:
            raise OpsError(f"{status.name}: 已在运行，PID={status.pid}")
        if status.missing_files:
            raise OpsError(f"{status.name}: 缺少文件: {', '.join(status.missing_files)}")
        settings = self.effective_start_settings(status)
        java_exe = self.resolve_java_exe(settings[0])
        if not java_exe.is_file():
            raise OpsError(f"{status.name}: java.exe 不存在: {java_exe}")
        return java_exe, settings[1], settings[2]

    def start_app(self, status: AppStatus) -> tuple[bool, str]:
        try:
            java_exe, min_memory, max_memory = self.validate_start(status)
        except OpsError as exc:
            return False, str(exc)

        # 必须与人工验证通过的命令保持一致。cwd 已设置为应用目录，
        # 因此 JAR 和两个 Spring 配置文件均使用相对路径。
        command = [
            str(java_exe),
            f"-Xms{min_memory}",
            f"-Xmx{max_memory}",
            "-Dfile.encoding=utf-8",
            "-jar",
            status.jar_path.name,
            "--spring.config.location=file:./application.yml,file:./bootstrap.yaml",
        ]

        log_dir = status.app_dir / self.config.general.log_dir
        try:
            log_dir.mkdir(parents=True, exist_ok=True)
            stdout_path = log_dir / "console.out.log"
            stderr_path = log_dir / "console.err.log"
            stdout_handle = stdout_path.open("ab", buffering=0)
            stderr_handle = stderr_path.open("ab", buffering=0)
        except OSError as exc:
            return False, f"{status.name}: 创建日志文件失败: {exc}"

        try:
            process = subprocess.Popen(
                command,
                cwd=str(status.app_dir),
                stdin=subprocess.DEVNULL,
                stdout=stdout_handle,
                stderr=stderr_handle,
                close_fds=True,
                creationflags=CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
            )
        except OSError as exc:
            stdout_handle.close()
            stderr_handle.close()
            return False, f"{status.name}: 启动失败: {exc}"
        finally:
            # Popen 创建成功时，子进程已经继承句柄，父进程可以关闭。
            if not stdout_handle.closed:
                stdout_handle.close()
            if not stderr_handle.closed:
                stderr_handle.close()

        provisional_pid = {
            "schema_version": PID_SCHEMA_VERSION,
            "app": status.name,
            "pid": process.pid,
            "start_time": now_text(),
            "min_memory": min_memory,
            "max_memory": max_memory,
            "ports": [],
            "jar_path": str(status.jar_path.resolve()),
            "command_line": subprocess.list2cmdline(command),
            "updated_at": now_text(),
        }
        try:
            atomic_write_json(status.pid_path, provisional_pid)
        except OSError as exc:
            # 无法建立 PID 管理关系时立即终止刚启动的进程，避免产生孤儿服务。
            try:
                run_hidden(["taskkill.exe", "/F", "/PID", str(process.pid), "/T"], timeout=10.0)
            except Exception:
                pass
            return False, f"{status.name}: 写入 PID 文件失败，已尝试终止进程: {exc}"

        if self.config.general.start_check_seconds > 0:
            time.sleep(self.config.general.start_check_seconds)
        exit_code = process.poll()
        if exit_code is not None:
            safe_unlink(status.pid_path)
            return (
                False,
                f"{status.name}: 进程启动后立即退出，退出码={exit_code}；"
                f"请检查 {stderr_path} 和 {stdout_path}",
            )
        if not process_exists(process.pid):
            safe_unlink(status.pid_path)
            return False, f"{status.name}: 未检测到启动后的进程，PID={process.pid}"

        return True, f"{status.name}: 启动成功，PID={process.pid}"

    def restart_app(self, status: AppStatus) -> tuple[bool, str]:
        # 先验证启动前置条件，避免因配置错误先停掉一个原本正常运行的服务。
        if status.running:
            shadow = AppStatus(
                index=status.index,
                name=status.name,
                app_dir=status.app_dir,
                app_config=status.app_config,
                jar_path=status.jar_path,
                pid_path=status.pid_path,
                missing_files=status.missing_files,
                config_missing=status.config_missing,
                running=False,
            )
            try:
                self.validate_start(shadow)
            except OpsError as exc:
                return False, str(exc)
            stopped, stop_message = self.stop_app(status)
            if not stopped:
                return False, f"{stop_message}；未执行重新启动"
        else:
            stop_message = f"{status.name}: 原进程未运行"

        refreshed_status = self.status_for_name(status.name)
        started, start_message = self.start_app(refreshed_status)
        return started, f"{stop_message}；{start_message}"

    def status_for_name(self, app_name: str) -> AppStatus:
        statuses, _warnings = self.refresh()
        for status in statuses:
            if status.name.casefold() == app_name.casefold():
                return status
        raise OpsError(f"应用目录已不存在: {app_name}")


# ---------------------------------------------------------------------------
# 交互界面
# ---------------------------------------------------------------------------


def now_text() -> str:
    return datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %z")


def setup_console() -> None:
    if os.name == "nt":
        os.system("chcp 65001 > nul")
        try:
            ctypes.windll.kernel32.SetConsoleTitleW(f"{PROGRAM_NAME} {PROGRAM_VERSION}")
        except Exception:
            pass
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            try:
                reconfigure(encoding="utf-8", errors="replace")
            except (OSError, ValueError):
                pass


def clear_screen() -> None:
    os.system("cls" if os.name == "nt" else "clear")


def display_width(text: str) -> int:
    width = 0
    for char in text:
        if unicodedata.combining(char):
            continue
        width += 2 if unicodedata.east_asian_width(char) in {"W", "F"} else 1
    return width


def truncate_display(text: str, max_width: int) -> str:
    if display_width(text) <= max_width:
        return text
    if max_width <= 1:
        return "…"[:max_width]
    output: list[str] = []
    width = 0
    target = max_width - 1
    for char in text:
        char_width = 0 if unicodedata.combining(char) else (
            2 if unicodedata.east_asian_width(char) in {"W", "F"} else 1
        )
        if width + char_width > target:
            break
        output.append(char)
        width += char_width
    return "".join(output) + "…"


def pad_display(text: str, width: int) -> str:
    text = truncate_display(text, width)
    return text + " " * max(0, width - display_width(text))


def render_table(headers: Sequence[str], rows: Sequence[Sequence[str]], caps: Sequence[int]) -> str:
    if not (len(headers) == len(caps)):
        raise ValueError("headers 与 caps 长度不一致")
    normalized_rows = [[str(cell) for cell in row] for row in rows]
    widths: list[int] = []
    for column, header in enumerate(headers):
        values = [display_width(header)]
        values.extend(display_width(row[column]) for row in normalized_rows)
        widths.append(min(max(values), caps[column]))

    top = "┌" + "┬".join("─" * (width + 2) for width in widths) + "┐"
    middle = "├" + "┼".join("─" * (width + 2) for width in widths) + "┤"
    bottom = "└" + "┴".join("─" * (width + 2) for width in widths) + "┘"

    def line(values: Sequence[str]) -> str:
        return "│ " + " │ ".join(
            pad_display(value, width) for value, width in zip(values, widths, strict=True)
        ) + " │"

    output = [top, line(headers), middle]
    output.extend(line(row) for row in normalized_rows)
    output.append(bottom)
    return "\n".join(output)


def status_rows(statuses: Sequence[AppStatus]) -> list[list[str]]:
    rows: list[list[str]] = []
    for status in statuses:
        if status.running:
            rows.append(
                [
                    str(status.index),
                    status.name,
                    "运行中",
                    str(status.pid or ""),
                    status.start_time,
                    status.memory_range,
                    ", ".join(status.ports),
                    status.issue_text,
                ]
            )
        else:
            # 按需求：未运行时主要展示名称；缺失项放在最后一列。
            rows.append(
                [
                    str(status.index),
                    status.name,
                    "",
                    "",
                    "",
                    "",
                    "",
                    status.issue_text,
                ]
            )
    return rows


def show_dashboard(
    config: Config,
    statuses: Sequence[AppStatus],
    warnings: Sequence[str],
    last_results: Sequence[tuple[bool, str]] = (),
) -> None:
    clear_screen()
    print(f"{PROGRAM_NAME}  v{PROGRAM_VERSION}")
    print(f"管理目录: {config.general.manage_dir}")
    print(f"配置文件: {config.path}")
    print(f"刷新时间: {now_text()}")
    print()
    print(
        render_table(
            ["序号", "应用名称", "状态", "PID", "启动时间", "内存范围", "监听端口", "问题"],
            status_rows(statuses),
            [6, 24, 8, 10, 25, 18, 34, 48],
        )
    )
    if not statuses:
        print("\n管理目录下没有应用子目录。")
    if warnings:
        print("\n警告:")
        for warning in warnings:
            print(f"  - {warning}")
    if last_results:
        print("\n上次操作结果:")
        for success, message in last_results:
            print(f"  [{'成功' if success else '失败'}] {message}")

    menu = "  ".join(
        f"[{config.commands[action]}] {ACTION_LABELS[action]}" for action in ACTIONS
    )
    print("\n" + menu)


def show_selection(title: str, statuses: Sequence[AppStatus]) -> None:
    print(f"\n{title}")
    rows = []
    for index, status in enumerate(statuses, start=1):
        marker_parts: list[str] = []
        if status.running:
            marker_parts.append(f"运行中 PID={status.pid}")
        else:
            marker_parts.append("未运行")
        if status.config_missing:
            marker_parts.append("配置缺失")
        if status.missing_files:
            marker_parts.append("缺少: " + ", ".join(status.missing_files))
        rows.append([str(index), status.name, "；".join(marker_parts)])
    print(render_table(["序号", "应用名称", "说明"], rows, [6, 28, 70]))
    print("0 = 全部；多个序号使用逗号分隔；直接回车 = 取消")


def parse_selection(text: str, item_count: int) -> list[int] | None:
    value = text.strip().replace("，", ",")
    if not value:
        return None
    parts = [part.strip() for part in value.split(",")]
    if any(not part for part in parts):
        raise OpsError("序号格式错误：存在空项")
    if "0" in parts:
        if len(parts) != 1:
            raise OpsError("0 表示全部，不能与其它序号同时输入")
        return list(range(item_count))

    indexes: list[int] = []
    seen: set[int] = set()
    for part in parts:
        if not part.isdecimal():
            raise OpsError(f"无效序号: {part}")
        number = int(part)
        if number < 1 or number > item_count:
            raise OpsError(f"序号超出范围: {number}")
        zero_based = number - 1
        if zero_based not in seen:
            indexes.append(zero_based)
            seen.add(zero_based)
    return indexes


def select_statuses(title: str, candidates: Sequence[AppStatus]) -> list[AppStatus] | None:
    if not candidates:
        print(f"\n{title}: 没有可操作的应用。")
        return []
    show_selection(title, candidates)
    while True:
        raw = input("请选择: ")
        try:
            indexes = parse_selection(raw, len(candidates))
        except OpsError as exc:
            print(f"输入错误: {exc}")
            continue
        if indexes is None:
            return None
        return [candidates[index] for index in indexes]


def perform_update(
    manager: ClusterManager, statuses: Sequence[AppStatus]
) -> list[tuple[bool, str]] | None:
    selected = select_statuses("更新 JAR 包", list(statuses))
    if selected is None:
        return None
    return [manager.copy_jar(status) for status in selected]


def perform_stop(
    manager: ClusterManager, statuses: Sequence[AppStatus]
) -> list[tuple[bool, str]] | None:
    running = [status for status in statuses if status.running]
    selected = select_statuses("停止应用", running)
    if selected is None:
        return None
    return [manager.stop_app(status) for status in selected]


def perform_start(
    manager: ClusterManager, statuses: Sequence[AppStatus]
) -> list[tuple[bool, str]] | None:
    stopped = [status for status in statuses if not status.running]
    selected = select_statuses("启动应用", stopped)
    if selected is None:
        return None
    return [manager.start_app(status) for status in selected]


def perform_restart(
    manager: ClusterManager, statuses: Sequence[AppStatus]
) -> list[tuple[bool, str]] | None:
    selected = select_statuses("重启应用", list(statuses))
    if selected is None:
        return None
    return [manager.restart_app(status) for status in selected]


def main() -> int:
    setup_console()
    if os.name != "nt":
        print("错误: 本工具仅支持 Windows。", file=sys.stderr)
        return 2

    conf_path = executable_dir() / CONF_FILE_NAME
    last_results: list[tuple[bool, str]] = []

    while True:
        try:
            config = load_config(conf_path)
            manager = ClusterManager(config)
            statuses, warnings = manager.refresh()
        except OpsError as exc:
            clear_screen()
            print(f"{PROGRAM_NAME}  v{PROGRAM_VERSION}\n")
            print(f"错误: {exc}")
            print(f"配置文件必须位于程序同级目录: {conf_path}")
            input("\n修正后按 Enter 重试，或按 Ctrl+C 退出...")
            continue
        except OSError as exc:
            clear_screen()
            print(f"系统错误: {exc}")
            input("\n按 Enter 重试，或按 Ctrl+C 退出...")
            continue

        show_dashboard(config, statuses, warnings, last_results)
        choice = input("\n请输入操作: ").strip().upper()
        action = config.command_to_action.get(choice)
        if action is None:
            print(f"无效操作: {choice or '<空>'}")
            time.sleep(0.8)
            continue
        if action == "quit":
            print("已退出。")
            return 0
        if action == "refresh":
            last_results = []
            continue

        try:
            results: list[tuple[bool, str]] | None = None
            if action == "update":
                results = perform_update(manager, statuses)
            elif action == "stop":
                results = perform_stop(manager, statuses)
            elif action == "start":
                results = perform_start(manager, statuses)
            elif action == "restart":
                results = perform_restart(manager, statuses)
            if results is not None:
                last_results = results
        except OpsError as exc:
            last_results = [(False, f"操作失败: {exc}")]
        except (OSError, subprocess.SubprocessError) as exc:
            last_results = [(False, f"系统调用失败: {exc}")]


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("\n已退出。")
        raise SystemExit(130)