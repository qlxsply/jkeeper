use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Deserialize)]
pub struct Config {
    pub general: General,
    #[serde(default)]
    pub apps: Vec<App>,
}
#[derive(Clone, Deserialize)]
pub struct General {
    pub manage_dir: PathBuf,
    pub java_path: PathBuf,
    pub min_memory: String,
    pub max_memory: String,
    #[serde(default = "pid_default")]
    pub pid_file: String,
    #[serde(default = "logs_default")]
    pub log_dir: PathBuf,
    #[serde(default = "viewer_default")]
    pub log_viewer: PathBuf,
    #[serde(default = "check_default")]
    pub start_check_seconds: f64,
    #[serde(default = "kill_default")]
    pub force_kill_timeout_seconds: f64,
}
#[derive(Clone, Deserialize)]
pub struct App {
    pub name: String,
    pub source_jar: Option<PathBuf>,
    pub jar_name: Option<String>,
    pub java_path: Option<PathBuf>,
    pub min_memory: Option<String>,
    pub max_memory: Option<String>,
    pub log_file: Option<PathBuf>,
}
fn pid_default() -> String {
    "{app}.pid".into()
}
fn logs_default() -> PathBuf {
    "logs".into()
}
fn viewer_default() -> PathBuf {
    "notepad.exe".into()
}
fn check_default() -> f64 {
    2.0
}
fn kill_default() -> f64 {
    5.0
}
fn absolute(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.into()
    } else {
        base.join(p)
    }
}
fn simple(s: &str) -> bool {
    !s.is_empty()
        && !s.ends_with(['.', ' '])
        && s != "."
        && s != ".."
        && !s
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && ![
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ]
        .contains(&s.split('.').next().unwrap_or("").to_uppercase().as_str())
}
pub fn memory(s: &str) -> Result<u64, String> {
    let (digits, factor) = match s.as_bytes().last().map(u8::to_ascii_lowercase) {
        Some(b'k') => (&s[..s.len() - 1], 1024u64),
        Some(b'm') => (&s[..s.len() - 1], 1024u64.pow(2)),
        Some(b'g') => (&s[..s.len() - 1], 1024u64.pow(3)),
        _ => (s, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("无效堆内存: {s}"))
}
impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        if std::fs::metadata(path)
            .map_err(|e| format!("读取配置失败 {}: {e}", path.display()))?
            .len()
            > 2 * 1024 * 1024
        {
            return Err("conf 超过 2 MiB".into());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取配置失败 {}: {e}", path.display()))?;
        let mut c: Self = toml::from_str(&text).map_err(|e| format!("配置解析失败: {e}"))?;
        let base = path.parent().unwrap();
        c.general.manage_dir = crate::process::ordinary_path(
            &std::fs::canonicalize(absolute(base, &c.general.manage_dir))
                .map_err(|e| format!("管理目录无效: {e}"))?,
        );
        if !c.general.manage_dir.is_dir() {
            return Err("管理目录不是目录".into());
        }
        c.general.java_path = absolute(base, &c.general.java_path);
        if c.general.log_viewer != Path::new("notepad.exe") {
            c.general.log_viewer = absolute(base, &c.general.log_viewer);
        }
        if !c.general.pid_file.contains("{app}")
            || !simple(&c.general.pid_file.replace("{app}", "app"))
        {
            return Err("pid_file 必须是包含 {app} 的文件名".into());
        }
        if c.general.log_dir.as_os_str().is_empty()
            || c.general.log_dir.is_absolute()
            || c.general.log_dir.components().any(|p| {
                matches!(
                    p,
                    std::path::Component::ParentDir
                        | std::path::Component::Prefix(_)
                        | std::path::Component::RootDir
                )
            })
        {
            return Err("log_dir 必须为应用目录内的相对路径".into());
        }
        for n in [
            c.general.start_check_seconds,
            c.general.force_kill_timeout_seconds,
        ] {
            if !n.is_finite() || !(0.0..=3600.0).contains(&n) {
                return Err("等待时间必须在 0～3600 秒内".into());
            }
        }
        let mut names = std::collections::HashSet::new();
        for a in &mut c.apps {
            if !simple(&a.name) || !names.insert(a.name.to_lowercase()) {
                return Err(format!("应用名称非法或重复: {}", a.name));
            }
            if a.jar_name.as_ref().is_some_and(|s| !simple(s)) {
                return Err(format!("{}: jar_name 必须是文件名", a.name));
            }
            if let Some(p) = &mut a.source_jar {
                *p = absolute(base, p);
            }
            if let Some(p) = &mut a.java_path {
                *p = absolute(base, p);
            }
            if memory(a.min_memory.as_ref().unwrap_or(&c.general.min_memory))?
                > memory(a.max_memory.as_ref().unwrap_or(&c.general.max_memory))?
            {
                return Err(format!("{}: 最小堆大于最大堆", a.name));
            }
        }
        if memory(&c.general.min_memory)? > memory(&c.general.max_memory)? {
            return Err("默认最小堆大于最大堆".into());
        }
        if c.general.log_viewer.as_os_str().is_empty() {
            return Err("日志查看程序路径不能为空".into());
        }
        c.apps.sort_by_key(|a| a.name.to_lowercase());
        Ok(c)
    }
    pub fn dir(&self, a: &App) -> PathBuf {
        self.general.manage_dir.join(&a.name)
    }
    pub fn jar(&self, a: &App) -> PathBuf {
        self.dir(a).join(
            a.jar_name
                .clone()
                .unwrap_or_else(|| format!("{}.jar", a.name)),
        )
    }
    pub fn pid(&self, a: &App) -> PathBuf {
        self.dir(a)
            .join(self.general.pid_file.replace("{app}", &a.name))
    }
    pub fn java(&self, a: &App) -> PathBuf {
        let p = a.java_path.as_ref().unwrap_or(&self.general.java_path);
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) {
            p.clone()
        } else {
            p.join("java.exe")
        }
    }
    pub fn heap(&self, a: &App) -> (String, String) {
        (
            a.min_memory
                .clone()
                .unwrap_or(self.general.min_memory.clone()),
            a.max_memory
                .clone()
                .unwrap_or(self.general.max_memory.clone()),
        )
    }
    pub fn log(&self, a: &App) -> PathBuf {
        self.jar(a).parent().unwrap().join(
            a.log_file
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("logs/witlink-cis-{}.log", a.name))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heap_validation() {
        assert_eq!(memory("1G").unwrap(), 1073741824);
        for s in ["0m", "-1m", "1.5g", "999999999999999999g"] {
            assert!(memory(s).is_err());
        }
    }
    #[test]
    fn unsafe_names() {
        for s in ["../pay", "CON", "pay.", "a:b", ""] {
            assert!(!simple(s));
        }
        assert!(simple("usercenter"));
    }
    #[test]
    fn configuration_snapshot_overrides_and_paths() {
        let dir = std::env::temp_dir().join(format!("cis-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = r#"
[general]
manage_dir = '.'
java_path = 'jdk/bin'
min_memory = '64m'
max_memory = '128m'
[[apps]]
name = 'zeta'
[[apps]]
name = 'alpha'
jar_name = 'server.jar'
min_memory = '32m'
max_memory = '256m'
source_jar = 'build/server.jar'
log_file = './logs/custom.log'
"#;
        let path = dir.join("conf");
        std::fs::write(&path, text).unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.apps[0].name, "alpha");
        assert_eq!(c.heap(&c.apps[0]), ("32m".into(), "256m".into()));
        assert_eq!(c.heap(&c.apps[1]), ("64m".into(), "128m".into()));
        assert_eq!(c.jar(&c.apps[0]), dir.join("alpha/server.jar"));
        assert_eq!(c.java(&c.apps[0]), dir.join("jdk/bin/java.exe"));
        assert_eq!(
            c.log(&c.apps[1]),
            dir.join("zeta/logs/witlink-cis-zeta.log")
        );
        assert_eq!(c.log(&c.apps[0]), dir.join("alpha/logs/custom.log"));
        std::fs::write(&path, "broken conf").unwrap();
        assert_eq!(c.apps.len(), 2);
        assert!(Config::load(&path).is_err());
        std::fs::write(&path, text.replace("name = 'zeta'", "name = 'ALPHA'")).unwrap();
        assert!(Config::load(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
