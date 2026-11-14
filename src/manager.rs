use crate::{
    config::{App, Config},
    network, process,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub created: u64,
    pub jar: String,
}
#[derive(Clone, Default)]
pub struct Status {
    pub identity: Option<Identity>,
    pub uncertain: bool,
    pub note: String,
    pub current_memory: Option<String>,
    pub ports: Vec<String>,
    pub started: String,
    pub jar_time: String,
    pub command_line: String,
    pub command_source: String,
    pub can_start: bool,
    pub can_update: bool,
    pub can_restart: bool,
}
pub fn format_time(t: std::time::SystemTime) -> String {
    let d: chrono::DateTime<chrono::Local> = t.into();
    d.format("%Y-%m-%d %H:%M:%S").to_string()
}
fn created_text(t: u64) -> String {
    format_time(
        std::time::UNIX_EPOCH
            + Duration::from_nanos(t.saturating_sub(116444736000000000).saturating_mul(100)),
    )
}

fn directory_contains(directory: &Path, target: &Path) -> bool {
    let directory = process::path_key(directory);
    let target = process::path_key(target);
    target == directory || target.starts_with(&(directory + "\\"))
}
fn quote_windows_arg(value: &str) -> String {
    if !value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c == '"') {
        return value.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0usize;
    for ch in value.chars() {
        match ch {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                slashes = 0;
                out.push(ch);
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}
fn expand_argument(c: &Config, a: &App, input: &str) -> Result<String, String> {
    let jar_name = a
        .jar_name
        .clone()
        .unwrap_or_else(|| format!("{}.jar", a.name));
    let java_path = c.java(a).to_string_lossy().into_owned();
    let mut output = String::new();
    let mut rest = input;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| format!("未闭合的参数占位符: {input}"))?;
        let key = &after[..close];
        match key {
            "name" => output.push_str(&a.name),
            "jar_name" => output.push_str(&jar_name),
            "java_path" => output.push_str(&java_path),
            _ => return Err(format!("未知参数占位符 {{{key}}}: {input}")),
        }
        rest = &after[close + 1..];
    }
    if rest.contains('}') {
        return Err(format!("未匹配的参数占位符: {input}"));
    }
    output.push_str(rest);
    Ok(output)
}

fn expand_line(c: &Config, a: &App, line: &str) -> Result<Vec<String>, String> {
    crate::config::split_windows_args(line)?
        .iter()
        .map(|arg| expand_argument(c, a, arg))
        .collect()
}

#[derive(Clone)]
struct JavaCommand {
    executable: PathBuf,
    args: Vec<String>,
}

fn java_command(c: &Config, a: &App) -> Result<JavaCommand, String> {
    if a.app_type != "java" {
        return Err(format!("不支持的应用类型: {}", a.app_type));
    }
    let mut args = expand_line(c, a, c.vm_args(a))?;
    for line in c.system_args(a) {
        args.extend(expand_line(c, a, line)?);
    }
    args.extend(expand_line(c, a, c.agent_args(a))?);
    args.push("-jar".into());
    args.push(
        process::ordinary_path(&c.jar(a))
            .to_string_lossy()
            .into_owned(),
    );
    for line in c.spring_args(a) {
        args.extend(expand_line(c, a, line)?);
    }
    Ok(JavaCommand {
        executable: c.java(a),
        args,
    })
}

pub fn configured_command(c: &Config, a: &App) -> Result<String, String> {
    let command = java_command(c, a)?;
    Ok(
        std::iter::once(command.executable.to_string_lossy().into_owned())
            .chain(command.args)
            .map(|arg| quote_windows_arg(&arg))
            .collect::<Vec<_>>()
            .join(" "),
    )
}
pub fn prerequisites(c: &Config, a: &App) -> Result<(), String> {
    if a.app_type != "java" {
        return Err(format!("不支持的应用类型: {}", a.app_type));
    }
    let mut missing = vec![];
    for p in [
        c.jar(a),
        c.dir(a).join("application.yml"),
        c.dir(a).join("bootstrap.yaml"),
    ] {
        if !p.is_file() {
            missing.push(
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    if !missing.is_empty() {
        return Err(format!("缺失必要文件: {}", missing.join(", ")));
    }
    if !c.java(a).is_file() {
        return Err(format!("Java 不存在: {}", c.java(a).display()));
    }
    java_command(c, a)?;
    Ok(())
}
fn read_identity(path: &Path) -> Result<Option<Identity>, String> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if meta.len() > 64 * 1024 {
        return Err("PID 文件过大".into());
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| e.to_string())
}
pub fn scan(c: &Config) -> Vec<Status> {
    struct NativeInfo {
        created: u64,
        command: String,
        memory: String,
        jar: Option<String>,
    }
    let pids = process::java_pids();
    let ports = network::ports();
    let processes: HashMap<u32, Result<NativeInfo, String>> = pids
        .as_ref()
        .map(|ids| {
            ids.iter()
                .map(|pid| {
                    let info = (|| {
                        let h = process::open(*pid, false)?;
                        if !process::alive(&h) {
                            return Err("进程已退出".into());
                        }
                        let command = process::command_line(&h)?;
                        let jar = process::jar_key(&command);
                        Ok(NativeInfo {
                            created: process::creation(&h)?,
                            command,
                            jar,
                            memory: process::memory(&h)
                                .map(|n| format!("{}M", n / 1024 / 1024))
                                .unwrap_or_else(|_| "?".into()),
                        })
                    })();
                    (*pid, info)
                })
                .collect()
        })
        .unwrap_or_default();
    c.apps
        .iter()
        .map(|a| {
            let jar = c.jar(a);
            let mut s = Status {
                started: "—".into(),
                jar_time: crate::jar_time::display(&jar),
                ..Default::default()
            };
            if a.app_type != "java" {
                s.note = format!("不支持的应用类型: {}", a.app_type);
                return s;
            }
            let jar_key = process::path_key(&jar);
            let mut notes = vec![];
            match configured_command(c, a) {
                Ok(command) => {
                    s.command_line = command;
                    s.command_source = "配置启动命令".into();
                }
                Err(error) => notes.push(error),
            }
            let mut matches = vec![];
            for (pid, info) in &processes {
                if let Ok(info) = info
                    && info.jar.as_deref() == Some(jar_key.as_str())
                {
                    matches.push((
                        Identity {
                            pid: *pid,
                            created: info.created,
                            jar: jar_key.clone(),
                        },
                        info.memory.clone(),
                        info.command.clone(),
                    ));
                }
            }
            if let Err(e) = &pids {
                s.uncertain = true;
                notes.push(e.clone());
            }
            if matches.len() > 1 {
                s.uncertain = true;
                notes.push("同一 JAR 存在多个进程，禁止操作".into());
            }
            if let Some((id, mem, command)) = matches.first() {
                s.current_memory = Some(mem.clone());
                s.started = created_text(id.created);
                s.ports = ports
                    .as_ref()
                    .ok()
                    .and_then(|p| p.get(&id.pid))
                    .cloned()
                    .unwrap_or_default();
                s.command_line = command.clone();
                s.command_source = "当前进程实际命令".into();
                s.identity = Some(id.clone());
            }
            // An unreadable live recorded process must never be interpreted as stopped.
            match read_identity(&c.pid(a)) {
                Ok(Some(id)) => {
                    if id.jar != process::path_key(&jar) {
                        s.uncertain = true;
                        notes.push("PID 记录的 JAR 路径不匹配".into());
                    }
                    if processes.get(&id.pid).is_some_and(Result::is_err) {
                        s.uncertain = true;
                        notes.push("无法核验记录中的进程身份".into());
                    }
                    if !s.uncertain && s.identity.is_none() {
                        notes.push("上次记录的进程已退出或记录已失效，可重新启动".into());
                    }
                }
                Err(e) => {
                    s.uncertain = true;
                    notes.push(format!("PID 文件损坏或不可读取: {e}"));
                }
                Ok(None) => {}
            }
            if let Err(e) = &ports {
                notes.push(e.clone());
            }
            let ready = prerequisites(c, a);
            if let Err(e) = &ready {
                notes.push(e.clone());
            }
            s.can_start = !s.uncertain
                && s.identity.is_none()
                && ready.is_ok()
                && s.command_source == "配置启动命令";
            s.can_restart = !s.uncertain && ready.is_ok();
            s.can_update = a.app_type == "java"
                && !s.uncertain
                && s.identity.is_none()
                && c.dir(a).is_dir()
                && !a.source_files.is_empty();
            s.note = notes.join("；");
            s
        })
        .collect()
}
fn current(c: &Config, index: usize) -> Status {
    scan(c).remove(index)
}
pub fn atomic_json(path: &Path, id: &Identity) -> Result<(), String> {
    let tmp = path.with_extension(format!("pid.{}.tmp", std::process::id()));
    let mut created = false;
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        created = true;
        f.write_all(&serde_json::to_vec_pretty(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        drop(f);
        process::replace(&tmp, path)
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&tmp);
    }
    result
}
fn clear_pid(c: &Config, a: &App) -> Result<(), String> {
    match fs::remove_file(c.pid(a)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("进程已退出，但 PID 文件清理失败: {e}")),
    }
}
fn start(c: &Config, index: usize) -> Result<String, String> {
    let a = &c.apps[index];
    prerequisites(c, a)?;
    let s = current(c, index);
    if s.uncertain {
        return Err(format!("进程身份待确认: {}", s.note));
    }
    if s.identity.is_some() {
        return Ok("跳过：应用已运行".into());
    }
    let launch = java_command(c, a)?;
    let (stdout, stderr) = if let Some(logs) = c.log_dir(a) {
        fs::create_dir_all(&logs).map_err(|e| format!("创建日志目录失败: {e}"))?;
        let out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("console.out.log"))
            .map_err(|e| format!("打开标准输出文件失败: {e}"))?;
        let err = OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("console.err.log"))
            .map_err(|e| format!("打开标准错误文件失败: {e}"))?;
        (Stdio::from(out), Stdio::from(err))
    } else {
        (Stdio::null(), Stdio::null())
    };
    let mut child = Command::new(&launch.executable)
        .args(&launch.args)
        .current_dir(c.dir(a))
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .creation_flags(0x08000000 | 0x00000200)
        .spawn()
        .map_err(|e| format!("启动失败: {e}"))?;
    let write = (|| {
        let h = process::open(child.id(), false)?;
        let created = process::creation(&h)?;
        atomic_json(
            &c.pid(a),
            &Identity {
                pid: child.id(),
                created,
                jar: process::path_key(&c.jar(a)),
            },
        )
    })();
    if let Err(e) = write {
        let kill = child.kill();
        let detail = if kill.is_ok() {
            let _ = child.wait();
            "已终止刚创建的进程".into()
        } else {
            format!("终止失败: {:?}，PID={}", kill.err(), child.id())
        };
        return Err(format!("保存 PID 失败: {e}；{detail}"));
    }
    std::thread::sleep(Duration::from_secs_f64(c.general.start_check_seconds));
    if let Some(exit) = child.try_wait().map_err(|e| e.to_string())? {
        clear_pid(c, a)?;
        let hint = if c.log_dir(a).is_some() {
            "，请检查应用日志目录中的 console.err.log / console.out.log"
        } else {
            "（stdout/stderr 已禁用）"
        };
        return Err(format!("进程立即退出 {exit}{hint}"));
    }
    Ok(format!(
        "已启动，PID={}（存活检查通过，不代表业务就绪）",
        child.id()
    ))
}
fn stop(c: &Config, index: usize) -> Result<String, String> {
    let a = &c.apps[index];
    let s = current(c, index);
    if s.uncertain {
        return Err(format!("进程身份待确认: {}", s.note));
    }
    let Some(id) = s.identity else {
        clear_pid(c, a)?;
        return Ok("跳过：应用未运行".into());
    };
    let handle = process::verify(id.pid, id.created, &c.jar(a), true)?;
    process::terminate(&handle, c.general.force_kill_timeout_seconds)?;
    clear_pid(c, a)?;
    Ok(format!("已强制停止，PID={}", id.pid))
}
fn copy_source_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|e| format!("读取来源失败 {}: {e}", source.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("不允许复制符号链接: {}", source.display()));
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(source, destination)
            .map_err(|e| format!("复制 {} 失败: {e}", source.display()))?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!("来源既不是文件也不是目录: {}", source.display()));
    }
    fs::create_dir_all(destination)
        .map_err(|e| format!("创建目录 {} 失败: {e}", destination.display()))?;
    for entry in
        fs::read_dir(source).map_err(|e| format!("读取目录 {} 失败: {e}", source.display()))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        copy_source_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn remove_any(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|e| e.to_string())
        }
        Ok(_) => fs::remove_file(path).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn rollback_deployment(changed: &[(PathBuf, Option<PathBuf>)]) -> Vec<String> {
    let mut errors = vec![];
    for (target, backup) in changed.iter().rev() {
        if let Err(e) = remove_any(target) {
            errors.push(format!("清理 {} 失败: {e}", target.display()));
        }
        if let Some(backup) = backup
            && let Err(e) = fs::rename(backup, target)
        {
            errors.push(format!("恢复 {} 失败: {e}", target.display()));
        }
    }
    errors
}

pub fn deploy_files(
    c: &Config,
    a: &App,
    check_stopped: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    if a.app_type != "java" {
        return Err(format!("不支持的应用类型: {}", a.app_type));
    }
    if a.source_files.is_empty() {
        return Err(format!("{}: source_files 未配置", a.name));
    }
    let app_dir = c.dir(a);
    if !app_dir.is_dir() {
        return Err(format!("目标应用目录不存在: {}", app_dir.display()));
    }
    let mut reserved = vec![c.pid(a)];
    if let Some(log_dir) = c.log_dir(a) {
        reserved.push(log_dir);
    }
    deploy_files_to(&a.source_files, &app_dir, &reserved, check_stopped)
}

fn deploy_files_to(
    source_files: &[PathBuf],
    app_dir: &Path,
    reserved_paths: &[PathBuf],
    check_stopped: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    let mut targets = std::collections::HashSet::new();
    let mut has_jar = false;
    let reserved_keys: Vec<String> = reserved_paths
        .iter()
        .map(|path| process::path_key(path))
        .collect();
    for source in source_files {
        if !source.is_absolute() {
            return Err(format!("source_files 必须为绝对路径: {}", source.display()));
        }
        let name = source
            .file_name()
            .ok_or_else(|| format!("来源路径没有文件名: {}", source.display()))?;
        let target = app_dir.join(name);
        let target_key = process::path_key(&target);
        if !targets.insert(target_key.clone()) {
            return Err(format!(
                "source_files 目标名称冲突: {}",
                name.to_string_lossy()
            ));
        }
        for reserved in &reserved_keys {
            if target_key == *reserved
                || target_key.starts_with(&(reserved.clone() + "\\"))
                || reserved.starts_with(&(target_key.clone() + "\\"))
            {
                return Err(format!(
                    "source_files 目标与 PID/日志保留路径冲突: {}",
                    target.display()
                ));
            }
        }
        let jar_entry = source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"));
        let metadata = fs::symlink_metadata(source)
            .map_err(|e| format!("来源文件更新失败 {}: {e}", source.display()))?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(format!("来源不是普通文件或目录: {}", source.display()));
        }
        if jar_entry && !metadata.is_file() {
            return Err(format!("JAR 来源必须是文件: {}", source.display()));
        }
        has_jar |= jar_entry && metadata.is_file();
        if directory_contains(app_dir, source)
            || (metadata.is_dir() && directory_contains(source, app_dir))
        {
            return Err(format!(
                "source_files 不能指向应用目录或其父目录: {}",
                source.display()
            ));
        }
    }
    if !has_jar {
        return Err("source_files 至少需要单独列出一个 .jar 文件".into());
    }

    let stage = app_dir.join(format!(
        ".cis-update-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::create_dir(&stage).map_err(|e| format!("创建更新暂存目录失败: {e}"))?;
    let mut keep_stage_for_manual_recovery = false;
    let result = (|| {
        let staged_root = stage.join("new");
        let backup_root = stage.join("backup");
        fs::create_dir(&staged_root).map_err(|e| e.to_string())?;
        fs::create_dir(&backup_root).map_err(|e| e.to_string())?;
        let mut staged = Vec::with_capacity(source_files.len());
        for (index, source) in source_files.iter().enumerate() {
            let name = source
                .file_name()
                .ok_or_else(|| format!("来源路径没有文件名: {}", source.display()))?;
            let path = staged_root.join(name);
            copy_source_tree(source, &path)?;
            staged.push((
                path,
                app_dir.join(name),
                backup_root.join(index.to_string()),
            ));
        }
        check_stopped()?;
        let mut changed: Vec<(PathBuf, Option<PathBuf>)> = vec![];
        for (staged_path, target, backup) in staged {
            let had_target = fs::symlink_metadata(&target).is_ok();
            if had_target && let Err(error) = fs::rename(&target, &backup) {
                let rollback_errors = rollback_deployment(&changed);
                if !rollback_errors.is_empty() {
                    keep_stage_for_manual_recovery = true;
                }
                return Err(format!(
                    "备份目标 {} 失败: {error}{}",
                    target.display(),
                    if rollback_errors.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "；回滚未完成，请保留暂存备份目录 {}；{}",
                            stage.display(),
                            rollback_errors.join("；")
                        )
                    }
                ));
            }
            if let Err(error) = fs::rename(&staged_path, &target) {
                let restore_current = if had_target {
                    fs::rename(&backup, &target).err()
                } else {
                    None
                };
                let rollback_errors = rollback_deployment(&changed);
                let mut recovery = rollback_errors;
                if let Some(restore_error) = restore_current {
                    recovery.push(format!("恢复 {} 失败: {restore_error}", target.display()));
                }
                if !recovery.is_empty() {
                    keep_stage_for_manual_recovery = true;
                    return Err(format!(
                        "复制目标 {} 失败: {error}；回滚未完整完成，请保留暂存备份目录 {}；{}",
                        target.display(),
                        stage.display(),
                        recovery.join("；")
                    ));
                }
                return Err(format!(
                    "复制目标 {} 失败: {error}；已回滚",
                    target.display()
                ));
            }
            changed.push((target, had_target.then_some(backup)));
        }
        Ok(format!("{} 个来源文件/目录已更新", source_files.len()))
    })();
    if !keep_stage_for_manual_recovery
        && let Err(error) = fs::remove_dir_all(&stage)
        && result.is_ok()
    {
        return Err(format!(
            "更新已完成，但清理暂存/备份目录失败 {}: {error}",
            stage.display()
        ));
    }
    result
}
pub fn execute(c: &Config, index: usize, action: &str) -> Result<String, String> {
    match action {
        "start" => start(c, index),
        "stop" => stop(c, index),
        "restart" => {
            prerequisites(c, &c.apps[index])?;
            stop(c, index)?;
            start(c, index)
        }
        "update" => {
            let status = current(c, index);
            if status.uncertain {
                return Err(format!("进程身份待确认: {}", status.note));
            }
            if status.identity.is_some() {
                return Ok("跳过：应用运行中，更新操作不可用".into());
            }
            let check = || {
                let s = current(c, index);
                if s.uncertain || s.identity.is_some() {
                    Err("应用运行中或身份待确认，禁止更新".into())
                } else {
                    Ok(())
                }
            };
            check()?;
            let a = &c.apps[index];
            deploy_files(c, a, check)?;
            Ok(format!(
                "{}：所有 source_files 已更新，应用保持停止",
                a.name
            ))
        }
        _ => Err("未知操作".into()),
    }
}
pub fn open_app_folder(c: &Config, index: usize) -> Result<String, String> {
    let app = &c.apps[index];
    let directory = c
        .log_dir(app)
        .ok_or_else(|| format!("{}: 控制台日志目录已禁用", app.name))?;
    if !directory.is_dir() {
        return Err(format!("日志目录不存在: {}", directory.display()));
    }
    Command::new("explorer.exe")
        .arg(&directory)
        .spawn()
        .map_err(|e| format!("无法打开日志目录: {e}"))?;
    Ok(format!("已打开日志目录 {}", directory.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_multi_file_deployment_preserves_all_originals() {
        let dir = std::env::temp_dir().join(format!("cis-deploy-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let app_dir = dir.join("app");
        let source = dir.join("source");
        fs::create_dir_all(&app_dir).unwrap();
        fs::create_dir_all(source.join("settings")).unwrap();
        fs::write(source.join("app.jar"), b"new jar").unwrap();
        fs::write(source.join("settings/application.yml"), b"new config").unwrap();
        fs::write(app_dir.join("app.jar"), b"old jar").unwrap();
        fs::create_dir_all(app_dir.join("settings")).unwrap();
        fs::write(app_dir.join("settings/application.yml"), b"old config").unwrap();
        let files = [source.join("app.jar"), source.join("settings")];
        let duplicate_dir = source.join("duplicate");
        fs::create_dir_all(&duplicate_dir).unwrap();
        fs::write(duplicate_dir.join("app.jar"), b"duplicate").unwrap();
        assert!(
            deploy_files_to(
                &[source.join("app.jar"), duplicate_dir.join("app.jar")],
                &app_dir,
                &[],
                || Ok(())
            )
            .unwrap_err()
            .contains("名称冲突")
        );
        assert_eq!(fs::read(app_dir.join("app.jar")).unwrap(), b"old jar");
        assert!(
            deploy_files_to(
                &[source.join("app.jar"), source.join("missing.yml")],
                &app_dir,
                &[],
                || Ok(())
            )
            .unwrap_err()
            .contains("missing.yml")
        );
        assert_eq!(fs::read(app_dir.join("app.jar")).unwrap(), b"old jar");
        assert!(deploy_files_to(&files, &app_dir, &[], || Err("running".into())).is_err());
        assert_eq!(fs::read(app_dir.join("app.jar")).unwrap(), b"old jar");
        assert_eq!(
            fs::read(app_dir.join("settings/application.yml")).unwrap(),
            b"old config"
        );
        deploy_files_to(&files, &app_dir, &[], || Ok(())).unwrap();
        assert_eq!(fs::read(app_dir.join("app.jar")).unwrap(), b"new jar");
        assert_eq!(
            fs::read(app_dir.join("settings/application.yml")).unwrap(),
            b"new config"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
