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
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub created: u64,
    pub jar: String,
    pub min: String,
    pub max: String,
}
#[derive(Clone, Default)]
pub struct Status {
    pub identity: Option<Identity>,
    pub uncertain: bool,
    pub note: String,
    pub memory: String,
    pub ports: String,
    pub started: String,
    pub jar_time: String,
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
fn heap_from_command(command: &str, c: &Config, a: &App) -> (String, String) {
    let (mut min, mut max) = c.heap(a);
    for arg in process::arguments(command) {
        if let Some(v) = arg.strip_prefix("-Xms") {
            min = v.into();
        }
        if let Some(v) = arg.strip_prefix("-Xmx") {
            max = v.into();
        }
    }
    (min, max)
}
pub fn prerequisites(c: &Config, a: &App) -> Result<(), String> {
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
    let java = c.java(a);
    let name = java
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if name != "java.exe" && name != "javaw.exe" {
        return Err("Java 路径必须指向 java.exe 或 javaw.exe".into());
    }
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
            let jar_key = process::path_key(&jar);
            let (min, max) = c.heap(a);
            let mut s = Status {
                memory: format!("{min} / — / {max}"),
                started: "—".into(),
                ports: "—".into(),
                jar_time: crate::jar_time::display(&jar),
                ..Default::default()
            };
            let mut notes = vec![];
            let mut matches = vec![];
            for (pid, info) in &processes {
                if let Ok(info) = info
                    && info.jar.as_deref() == Some(jar_key.as_str())
                {
                    let (min, max) = heap_from_command(&info.command, c, a);
                    matches.push((
                        Identity {
                            pid: *pid,
                            created: info.created,
                            jar: jar_key.clone(),
                            min,
                            max,
                        },
                        info.memory.clone(),
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
            if let Some((id, mem)) = matches.first() {
                s.memory = format!("{} / {} / {}", id.min, mem, id.max);
                s.started = created_text(id.created);
                s.ports = ports
                    .as_ref()
                    .ok()
                    .and_then(|p| p.get(&id.pid))
                    .map(|p| p.iter().cloned().collect::<Vec<_>>().join(", "))
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| "—".into());
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
            s.can_start = !s.uncertain && s.identity.is_none() && ready.is_ok();
            s.can_restart = !s.uncertain && ready.is_ok();
            s.can_update = !s.uncertain
                && s.identity.is_none()
                && c.dir(a).is_dir()
                && a.source_jar.as_ref().is_some_and(|p| p.is_file());
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
    let logs = c.dir(a).join(&c.general.log_dir);
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
    let (min, max) = c.heap(a);
    let mut child = Command::new(c.java(a))
        .arg(format!("-Xms{min}"))
        .arg(format!("-Xmx{max}"))
        .arg("-Dfile.encoding=utf-8")
        .arg("-jar")
        .arg(c.jar(a))
        .arg("--spring.config.location=file:./application.yml,file:./bootstrap.yaml")
        .current_dir(c.dir(a))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
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
                min,
                max,
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
        return Err(format!(
            "进程立即退出 {exit}，请检查 console.err.log / console.out.log"
        ));
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
pub fn deploy(
    source: &Path,
    target: &Path,
    check_stopped: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if process::path_key(source) == process::path_key(target) {
        return Err("来源与目标 JAR 相同".into());
    }
    let temp = target.with_extension(format!("jar.{}.updating", std::process::id()));
    let mut created = false;
    let result = (|| {
        let before = fs::metadata(source).map_err(|e| format!("来源文件不可用: {e}"))?;
        if before.len() == 0 || !before.is_file() {
            return Err("来源 JAR 为空或不是文件".into());
        }
        let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        created = true;
        let bytes = std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        let after = fs::metadata(source).map_err(|e| e.to_string())?;
        if bytes != before.len()
            || after.len() != before.len()
            || after.modified().ok() != before.modified().ok()
        {
            return Err("复制期间来源 JAR 已变化，请重新构建后再更新".into());
        }
        check_stopped()?;
        if target.exists() {
            fs::copy(target, target.with_extension("jar.bak"))
                .map_err(|e| format!("备份失败，未替换: {e}"))?;
        }
        process::replace(&temp, target)
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temp);
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
            let source = a.source_jar.as_ref().ok_or("未配置 source_jar")?;
            deploy(source, &c.jar(a), check)?;
            Ok("JAR 更新完成，应用保持停止".into())
        }
        _ => Err("未知操作".into()),
    }
}
pub fn open_log(c: &Config, index: usize) -> Result<String, String> {
    let log = c.log(&c.apps[index]);
    if !log.is_file() {
        return Err(format!("日志文件不存在: {}", log.display()));
    }
    Command::new(&c.general.log_viewer)
        .arg(&log)
        .spawn()
        .map_err(|e| format!("无法打开日志查看程序: {e}"))?;
    Ok(format!("已打开 {}", log.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_deployment_preserves_original() {
        let dir = std::env::temp_dir().join(format!("cis-deploy-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("new.jar");
        let dst = dir.join("app.jar");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        assert!(deploy(&src, &dst, || Err("running".into())).is_err());
        assert_eq!(fs::read(&dst).unwrap(), b"old");
        deploy(&src, &dst, || Ok(())).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        assert_eq!(fs::read(dst.with_extension("jar.bak")).unwrap(), b"old");
        fs::remove_dir_all(dir).unwrap();
    }
}
