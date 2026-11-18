#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod config;
mod jar_time;
mod logging;
mod manager;
mod network;
mod port_format;
mod process;
mod scheduler;

use serde::Serialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{Emitter, State};

struct Backend(Arc<BackendInner>);
struct BackendInner {
    config: Arc<config::Config>,
    busy: AtomicBool,
    status: Mutex<String>,
}

#[derive(Serialize)]
struct ServiceRow {
    name: String,
    pid: String,
    memory: String,
    ports: String,
    ports_full: String,
    started: String,
    updated: String,
    command_line: String,
    display_status: String,
    status_kind: String,
    external_idea: bool,
    running: bool,
    uncertain: bool,
    problem: bool,
    note: String,
    can_start: bool,
    can_stop: bool,
    can_restart: bool,
    can_update: bool,
}

#[derive(Serialize)]
struct Snapshot {
    rows: Vec<ServiceRow>,
    directory: String,
    status: String,
    busy: bool,
}

#[tauri::command]
fn get_snapshot(backend: State<'_, Backend>) -> Snapshot {
    let statuses = manager::scan(&backend.0.config);
    let rows = backend
        .0
        .config
        .apps
        .iter()
        .zip(statuses)
        .map(|(app, status)| {
            let (display_status, status_kind) = if status.ambiguous {
                ("匹配不唯一", "status-ambiguous")
            } else if status.idea_status == Some(manager::IdeaStatus::Debug) {
                ("IDEA Debug", "status-idea-debug")
            } else if status.idea_status == Some(manager::IdeaStatus::Running) {
                ("IDEA 运行", "status-idea-running")
            } else if status.uncertain || !status.note.is_empty() {
                ("异常", "status-error")
            } else if status.identity.is_some() {
                ("运行中", "status-running")
            } else {
                ("已停止", "status-stopped")
            };
            let external_idea = status.idea_status.is_some();
            ServiceRow {
                name: app.name.clone(),
                pid: status
                    .identity
                    .as_ref()
                    .map(|identity| identity.pid.to_string())
                    .unwrap_or_else(|| "—".into()),
                memory: status.current_memory.unwrap_or_else(|| "—".into()),
                ports: crate::port_format::table_summary(&status.ports),
                ports_full: if status.ports.is_empty() {
                    "—".into()
                } else {
                    status.ports.join("\n")
                },
                started: status.started,
                updated: status.jar_time,
                command_line: status.command_line,
                display_status: display_status.into(),
                status_kind: status_kind.into(),
                external_idea,
                running: status.identity.is_some(),
                uncertain: status.uncertain,
                problem: !status.note.is_empty() && !external_idea && !status.ambiguous,
                note: status.note,
                can_start: status.can_start,
                can_stop: status.identity.is_some() && !status.uncertain,
                can_restart: status.can_restart,
                can_update: status.can_update,
            }
        })
        .collect();
    Snapshot {
        rows,
        directory: backend.0.config.general.manage_dir.display().to_string(),
        status: backend
            .0
            .status
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| "状态不可用".into()),
        busy: backend.0.busy.load(Ordering::Acquire),
    }
}

#[tauri::command]
fn perform_action(
    app: tauri::AppHandle,
    backend: State<'_, Backend>,
    indices: Vec<usize>,
    action: String,
) -> Result<(), String> {
    if !matches!(
        action.as_str(),
        "start" | "stop" | "restart" | "update" | "log"
    ) {
        return Err("未知操作".into());
    }
    if indices.is_empty() {
        return Err("请先选择应用".into());
    }
    if indices
        .iter()
        .any(|index| *index >= backend.0.config.apps.len())
    {
        return Err("应用索引无效".into());
    }
    if backend
        .0
        .busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("已有管理任务正在执行".into());
    }
    let inner = backend.0.clone();
    if let Ok(mut status) = inner.status.lock() {
        *status = format!("正在执行 {}", action_label(&action));
    }
    let _ = app.emit("jkeeper-updated", ());
    std::thread::Builder::new()
        .name("management-task".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if action == "log" {
                    for index in indices {
                        let message = manager::open_app_folder(&inner.config, index)
                            .unwrap_or_else(|error| error);
                        if let Ok(mut status) = inner.status.lock() {
                            *status = format!("{}：{message}", inner.config.apps[index].name);
                        }
                    }
                } else {
                    let mut indices = indices;
                    indices.sort_unstable();
                    scheduler::run_batch(
                        indices,
                        |index| manager::execute(&inner.config, index, &action),
                        |index, outcome| {
                            let message = outcome.unwrap_or_else(|error| format!("失败：{error}"));
                            if let Ok(mut status) = inner.status.lock() {
                                *status = format!("{}：{message}", inner.config.apps[index].name);
                            }
                        },
                    );
                }
            }));
            if result.is_err() {
                if let Ok(mut status) = inner.status.lock() {
                    *status = "后台任务发生异常，已结束当前任务".into();
                }
            }
            inner.busy.store(false, Ordering::Release);
            let _ = app.emit("jkeeper-updated", ());
        })
        .map_err(|error| {
            backend.0.busy.store(false, Ordering::Release);
            format!("无法创建任务线程：{error}")
        })?;
    Ok(())
}

fn action_label(action: &str) -> &'static str {
    match action {
        "start" => "启动",
        "stop" => "停止",
        "restart" => "重启",
        "update" => "更新",
        "log" => "打开日志",
        _ => "操作",
    }
}

fn main() {
    if let Err(error) = run() {
        process::show_startup_error(&format!("{error}\njkeeper 将退出。"));
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let logging = logging::init();
    let exe = std::env::current_exe()?;
    let base = exe.parent().ok_or("无法确定 jkeeper.exe 所在目录")?;
    let config = match config::Config::load(&base.join("conf.toml")) {
        Ok(config) => Arc::new(config),
        Err(error) => {
            process::show_startup_error(&format!("{error}\n请修正 conf.toml 后重新启动 jkeeper。"));
            return Ok(());
        }
    };
    let _instance_lock = match process::global_instance_lock() {
        Ok(lock) => lock,
        Err(error) => {
            process::show_startup_error(&format!("{error}\njkeeper 将退出。"));
            return Ok(());
        }
    };
    if let Err(error) = logging {
        tracing::error!("管理器日志不可用: {error}");
    }
    let backend = Backend(Arc::new(BackendInner {
        config,
        busy: AtomicBool::new(false),
        status: Mutex::new("监控中".into()),
    }));
    tauri::Builder::default()
        .manage(backend)
        .invoke_handler(tauri::generate_handler![get_snapshot, perform_action])
        .run(tauri::generate_context!())?;
    Ok(())
}
