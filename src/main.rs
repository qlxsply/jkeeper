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
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, State};

const LAST_CLUSTER_FILE: &str = "jkeeper-last-cluster.txt";

#[derive(Clone)]
struct ClusterFile {
    name: String,
    path: PathBuf,
}

#[derive(Clone)]
struct ActiveCluster {
    name: String,
    config: Arc<config::Config>,
}

struct Backend(Arc<BackendInner>);
struct BackendInner {
    base_dir: PathBuf,
    clusters: Vec<ClusterFile>,
    active: RwLock<ActiveCluster>,
    operation_gate: Mutex<()>,
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
    can_switch_cluster: bool,
    clusters: Vec<String>,
    cluster_name: String,
}

fn discover_clusters(base_dir: &Path) -> Result<Vec<ClusterFile>, String> {
    let entries = fs::read_dir(base_dir)
        .map_err(|error| format!("无法扫描集群配置目录 {}: {error}", base_dir.display()))?;
    let mut clusters = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取配置目录条目失败: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("读取配置文件类型失败: {error}"))?
            .is_file()
        {
            continue;
        }
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(name) = filename
            .strip_prefix("conf-")
            .and_then(|tail| tail.strip_suffix(".toml"))
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        clusters.push(ClusterFile {
            name: name.to_owned(),
            path: entry.path(),
        });
    }
    clusters.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    if clusters.is_empty() {
        return Err(format!(
            "在 {} 中没有找到集群配置文件（conf-<name>.toml）",
            base_dir.display()
        ));
    }
    Ok(clusters)
}

fn write_last_cluster(base_dir: &Path, name: &str) -> Result<(), String> {
    let target = base_dir.join(LAST_CLUSTER_FILE);
    let temp = base_dir.join(format!("{LAST_CLUSTER_FILE}.{}.tmp", std::process::id()));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| format!("创建集群记录临时文件失败: {error}"))?;
        created = true;
        file.write_all(name.as_bytes())
            .map_err(|error| format!("写入集群记录失败: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步集群记录失败: {error}"))?;
        drop(file);
        process::replace(&temp, &target)
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(temp);
    }
    result
}

fn select_startup_cluster(
    base_dir: &Path,
    clusters: &[ClusterFile],
) -> Result<(ClusterFile, Arc<config::Config>), String> {
    let marker_path = base_dir.join(LAST_CLUSTER_FILE);
    let saved_name = match fs::read_to_string(&marker_path) {
        Ok(value) => value.trim().to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("读取上次集群记录失败: {error}")),
    };
    let selected = if saved_name.is_empty() {
        None
    } else {
        clusters.iter().find(|cluster| cluster.name == saved_name)
    };
    let (cluster, update_marker) = match selected {
        Some(cluster) => (cluster.clone(), false),
        None => (clusters[0].clone(), true),
    };
    let config = config::Config::load(&cluster.path)
        .map_err(|error| format!("加载集群配置 {} 失败: {error}", cluster.path.display()))?;
    if update_marker {
        write_last_cluster(base_dir, &cluster.name)?;
    }
    Ok((cluster, Arc::new(config)))
}

#[tauri::command]
fn get_snapshot(backend: State<'_, Backend>) -> Result<Snapshot, String> {
    let active = backend
        .0
        .active
        .read()
        .map_err(|_| "当前集群配置锁异常".to_string())?
        .clone();
    let statuses = manager::scan(&active.config);
    let busy = backend.0.busy.load(Ordering::Acquire);
    let can_switch_cluster = !busy
        && statuses
            .iter()
            .all(|status| status.identity.is_none() && !status.process_scan_failed);
    let rows = active
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
                    .or_else(|| status.external_pid.map(|pid| pid.to_string()))
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
    Ok(Snapshot {
        rows,
        directory: active.config.general.manage_dir.display().to_string(),
        status: backend
            .0
            .status
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| "状态不可用".into()),
        busy,
        can_switch_cluster,
        clusters: backend
            .0
            .clusters
            .iter()
            .map(|cluster| cluster.name.clone())
            .collect(),
        cluster_name: active.name,
    })
}

#[tauri::command]
fn switch_cluster(
    app: tauri::AppHandle,
    backend: State<'_, Backend>,
    name: String,
) -> Result<(), String> {
    let _gate = backend
        .0
        .operation_gate
        .lock()
        .map_err(|_| "集群切换锁异常".to_string())?;
    if backend.0.busy.load(Ordering::Acquire) {
        return Err("管理任务正在执行，暂时不能切换集群".into());
    }
    let target = backend
        .0
        .clusters
        .iter()
        .find(|cluster| cluster.name == name)
        .cloned()
        .ok_or_else(|| format!("找不到集群配置: {name}"))?;
    let current = backend
        .0
        .active
        .read()
        .map_err(|_| "当前集群配置锁异常".to_string())?
        .clone();
    if current.name == target.name {
        return Ok(());
    }

    let new_config = Arc::new(
        config::Config::load(&target.path)
            .map_err(|error| format!("加载集群配置 {} 失败: {error}", target.path.display()))?,
    );
    match manager::has_managed_running_process(&current.config)? {
        true => return Err("当前集群仍有 jkeeper 管理的应用正在运行，停止全部应用后再切换".into()),
        false => {}
    }
    let mut active = backend
        .0
        .active
        .write()
        .map_err(|_| "当前集群配置锁异常".to_string())?;
    write_last_cluster(&backend.0.base_dir, &target.name)?;
    *active = ActiveCluster {
        name: target.name.clone(),
        config: new_config,
    };
    if let Ok(mut status) = backend.0.status.lock() {
        *status = format!("已切换到集群 {}", target.name);
    }
    let _ = app.emit("jkeeper-updated", ());
    Ok(())
}

#[tauri::command]
fn open_manage_directory(backend: State<'_, Backend>) -> Result<(), String> {
    let directory = backend
        .0
        .active
        .read()
        .map_err(|_| "当前集群配置锁异常".to_string())?
        .config
        .general
        .manage_dir
        .clone();
    if !directory.is_dir() {
        return Err(format!("管理目录不存在: {}", directory.display()));
    }
    Command::new("explorer.exe")
        .arg(&directory)
        .spawn()
        .map_err(|error| format!("无法打开管理目录: {error}"))?;
    Ok(())
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
    let gate = backend
        .0
        .operation_gate
        .lock()
        .map_err(|_| "集群切换锁异常".to_string())?;
    if backend.0.busy.load(Ordering::Acquire) {
        return Err("已有管理任务正在执行".into());
    }
    let active = backend
        .0
        .active
        .read()
        .map_err(|_| "当前集群配置锁异常".to_string())?
        .clone();
    if indices
        .iter()
        .any(|index| *index >= active.config.apps.len())
    {
        return Err("应用索引无效".into());
    }
    backend
        .0
        .busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "已有管理任务正在执行".to_string())?;
    drop(gate);

    let inner = backend.0.clone();
    let config = active.config;
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
                        let message =
                            manager::open_app_folder(&config, index).unwrap_or_else(|error| error);
                        if let Ok(mut status) = inner.status.lock() {
                            *status = format!("{}：{message}", config.apps[index].name);
                        }
                    }
                } else {
                    let mut indices = indices;
                    indices.sort_unstable();
                    scheduler::run_batch(
                        indices,
                        |index| manager::execute(&config, index, &action),
                        |index, outcome| {
                            let message = outcome.unwrap_or_else(|error| format!("失败：{error}"));
                            if let Ok(mut status) = inner.status.lock() {
                                *status = format!("{}：{message}", config.apps[index].name);
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

fn run() -> Result<(), String> {
    let logging = logging::init();
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let base_dir = exe
        .parent()
        .ok_or_else(|| "无法确定 jkeeper.exe 所在目录".to_string())?
        .to_path_buf();
    let clusters = discover_clusters(&base_dir)?;
    let (selected, config) = select_startup_cluster(&base_dir, &clusters)?;
    let _instance_lock = process::global_instance_lock()?;
    if let Err(error) = logging {
        tracing::error!("管理器日志不可用: {error}");
    }
    let backend = Backend(Arc::new(BackendInner {
        base_dir,
        clusters,
        active: RwLock::new(ActiveCluster {
            name: selected.name,
            config,
        }),
        operation_gate: Mutex::new(()),
        busy: AtomicBool::new(false),
        status: Mutex::new("监控中".into()),
    }));
    tauri::Builder::default()
        .manage(backend)
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            switch_cluster,
            open_manage_directory,
            perform_action
        ])
        .run(tauri::generate_context!())
        .map_err(|error| error.to_string())?;
    Ok(())
}
