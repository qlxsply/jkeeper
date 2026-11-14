use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub general: General,
    #[serde(default)]
    pub apps: Vec<App>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub manage_dir: PathBuf,
    /// The absolute path to java.exe (not the JDK bin directory).
    pub java_path: PathBuf,
    #[serde(default = "vm_args_default")]
    pub java_vm_args: String,
    #[serde(default = "system_args_default")]
    pub java_system_args: Vec<String>,
    #[serde(default)]
    pub java_agent_args: String,
    #[serde(default = "spring_args_default")]
    pub java_spring_args: Vec<String>,
    #[serde(default = "pid_default")]
    pub pid_file: String,
    #[serde(default = "stop_default")]
    pub stop_timeout_seconds: f64,
    #[serde(default = "logs_default")]
    pub log_dir: PathBuf,
    #[serde(default = "check_default")]
    pub start_check_seconds: f64,
    #[serde(default = "kill_default")]
    pub force_kill_timeout_seconds: f64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub name: String,
    #[serde(rename = "type")]
    pub app_type: String,
    #[serde(default)]
    pub source_files: Vec<PathBuf>,
    pub jar_name: Option<String>,
    pub java_path: Option<PathBuf>,
    pub java_vm_args: Option<String>,
    pub java_system_args: Option<Vec<String>>,
    pub java_agent_args: Option<String>,
    pub java_spring_args: Option<Vec<String>>,
    pub log_dir: Option<PathBuf>,
}

fn pid_default() -> String {
    "{app}.pid".into()
}
fn stop_default() -> f64 {
    15.0
}
fn logs_default() -> PathBuf {
    "logs".into()
}
fn vm_args_default() -> String {
    "-Xms64m -Xmx128m".into()
}
fn system_args_default() -> Vec<String> {
    vec!["-Dfile.encoding=utf-8".into()]
}
fn spring_args_default() -> Vec<String> {
    vec!["--spring.config.location=file:./application.yml,file:./bootstrap.yaml".into()]
}
fn check_default() -> f64 {
    2.0
}
fn kill_default() -> f64 {
    5.0
}

fn absolute(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        base.join(path)
    }
}

fn simple_filename(value: &str) -> bool {
    !value.is_empty()
        && !value.ends_with(['.', ' '])
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && ![
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ]
        .contains(
            &value
                .split('.')
                .next()
                .unwrap_or("")
                .to_uppercase()
                .as_str(),
        )
}

fn validate_relative_log_dir(path: &Path, label: &str) -> Result<(), String> {
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err(format!(
            "{label} 必须是应用目录内的相对路径；空字符串表示禁用控制台日志"
        ));
    }
    Ok(())
}

/// Split one config line using Windows command-line quoting rules. No shell is involved.
pub fn split_windows_args(line: &str) -> Result<Vec<String>, String> {
    if line.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parsed = crate::process::arguments(&format!("cis-args.exe {line}"));
    if parsed.is_empty() {
        return Err("Windows 参数解析失败".into());
    }
    Ok(parsed.into_iter().skip(1).collect())
}

fn validate_args_line(value: &str, label: &str) -> Result<(), String> {
    let mut quoted = false;
    let mut slashes = 0usize;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' && slashes.is_multiple_of(2) {
            quoted = !quoted;
        }
        slashes = 0;
    }
    if quoted {
        return Err(format!("{label} 的双引号不成对"));
    }
    split_windows_args(value)
        .map(|_| ())
        .map_err(|e| format!("{label}: {e}"))
}

fn validate_args_list(values: &[String], label: &str) -> Result<(), String> {
    for (index, value) in values.iter().enumerate() {
        validate_args_line(value, &format!("{label}[{}]", index + 1))?;
    }
    Ok(())
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let meta =
            std::fs::metadata(path).map_err(|e| format!("读取配置失败 {}: {e}", path.display()))?;
        if meta.len() > 2 * 1024 * 1024 {
            return Err("conf 超过 2 MiB".into());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取配置失败 {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text).map_err(|e| format!("配置解析失败: {e}"))?;
        let base = path.parent().ok_or("无法确定 conf 所在目录")?;

        config.general.manage_dir = crate::process::ordinary_path(
            &std::fs::canonicalize(absolute(base, &config.general.manage_dir))
                .map_err(|e| format!("管理目录无效: {e}"))?,
        );
        if !config.general.manage_dir.is_dir() {
            return Err("管理目录不是目录".into());
        }
        Self::validate_java_path(&config.general.java_path, "general.java_path")?;
        if !config.general.pid_file.contains("{app}")
            || !simple_filename(&config.general.pid_file.replace("{app}", "app"))
        {
            return Err("general.pid_file 必须是包含 {app} 的文件名".into());
        }
        validate_relative_log_dir(&config.general.log_dir, "general.log_dir")?;
        for (key, value) in [
            (
                "general.start_check_seconds",
                config.general.start_check_seconds,
            ),
            (
                "general.stop_timeout_seconds",
                config.general.stop_timeout_seconds,
            ),
            (
                "general.force_kill_timeout_seconds",
                config.general.force_kill_timeout_seconds,
            ),
        ] {
            if !value.is_finite() || !(0.0..=3600.0).contains(&value) {
                return Err(format!("{key} 必须在 0～3600 秒内"));
            }
        }
        validate_args_line(&config.general.java_vm_args, "general.java_vm_args")?;
        validate_args_list(&config.general.java_system_args, "general.java_system_args")?;
        validate_args_line(&config.general.java_agent_args, "general.java_agent_args")?;
        validate_args_list(&config.general.java_spring_args, "general.java_spring_args")?;

        let mut names = std::collections::HashSet::new();
        for app in &mut config.apps {
            if !simple_filename(&app.name) || !names.insert(app.name.to_lowercase()) {
                return Err(format!("应用名称非法或重复: {}", app.name));
            }
            if app.app_type.trim().is_empty() {
                return Err(format!("{}: type 不能为空", app.name));
            }
            if app.jar_name.as_ref().is_some_and(|value| {
                !simple_filename(value) || !value.to_ascii_lowercase().ends_with(".jar")
            }) {
                return Err(format!("{}: jar_name 必须是 .jar 文件名", app.name));
            }
            if app.app_type == "java" {
                if app.source_files.is_empty() {
                    return Err(format!("{}: Java 应用必须配置 source_files", app.name));
                }
                if !app.source_files.iter().any(|file| {
                    file.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
                }) {
                    return Err(format!(
                        "{}: source_files 至少要列出一个 JAR 文件",
                        app.name
                    ));
                }
                for source in &app.source_files {
                    if !source.is_absolute() {
                        return Err(format!(
                            "{}: source_files 必须使用绝对路径: {}",
                            app.name,
                            source.display()
                        ));
                    }
                }
                if let Some(path) = &app.java_path {
                    Self::validate_java_path(path, &format!("{}.java_path", app.name))?;
                }
                if let Some(value) = &app.java_vm_args {
                    validate_args_line(value, &format!("{}.java_vm_args", app.name))?;
                }
                if let Some(values) = &app.java_system_args {
                    validate_args_list(values, &format!("{}.java_system_args", app.name))?;
                }
                if let Some(value) = &app.java_agent_args {
                    validate_args_line(value, &format!("{}.java_agent_args", app.name))?;
                }
                if let Some(values) = &app.java_spring_args {
                    validate_args_list(values, &format!("{}.java_spring_args", app.name))?;
                }
            }
            if let Some(path) = &app.log_dir {
                validate_relative_log_dir(path, &format!("{}.log_dir", app.name))?;
            }
        }
        config.apps.sort_by_key(|app| app.name.to_lowercase());
        Ok(config)
    }

    fn validate_java_path(path: &Path, label: &str) -> Result<(), String> {
        if !path.is_absolute()
            || !path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("java.exe"))
        {
            return Err(format!("{label} 必须是指向 java.exe 的绝对路径"));
        }
        Ok(())
    }

    pub fn dir(&self, app: &App) -> PathBuf {
        self.general.manage_dir.join(&app.name)
    }
    pub fn jar(&self, app: &App) -> PathBuf {
        self.dir(app).join(
            app.jar_name
                .clone()
                .unwrap_or_else(|| format!("{}.jar", app.name)),
        )
    }
    pub fn pid(&self, app: &App) -> PathBuf {
        self.dir(app)
            .join(self.general.pid_file.replace("{app}", &app.name))
    }
    pub fn java(&self, app: &App) -> PathBuf {
        app.java_path
            .clone()
            .unwrap_or_else(|| self.general.java_path.clone())
    }
    pub fn log_dir(&self, app: &App) -> Option<PathBuf> {
        let path = app.log_dir.as_ref().unwrap_or(&self.general.log_dir);
        (!path.as_os_str().is_empty()).then(|| self.dir(app).join(path))
    }
    pub fn vm_args<'a>(&'a self, app: &'a App) -> &'a str {
        app.java_vm_args
            .as_deref()
            .unwrap_or(&self.general.java_vm_args)
    }
    pub fn system_args<'a>(&'a self, app: &'a App) -> &'a [String] {
        app.java_system_args
            .as_deref()
            .unwrap_or(&self.general.java_system_args)
    }
    pub fn agent_args<'a>(&'a self, app: &'a App) -> &'a str {
        app.java_agent_args
            .as_deref()
            .unwrap_or(&self.general.java_agent_args)
    }
    pub fn spring_args<'a>(&'a self, app: &'a App) -> &'a [String] {
        app.java_spring_args
            .as_deref()
            .unwrap_or(&self.general.java_spring_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_arg_groups_preserve_quotes_and_spaces() {
        assert_eq!(
            split_windows_args("-Xms64m -XX:MaxGCPauseMillis=200").unwrap(),
            ["-Xms64m", "-XX:MaxGCPauseMillis=200"]
        );
        assert_eq!(
            split_windows_args("-Dvalue=\"two words\"").unwrap(),
            ["-Dvalue=two words"]
        );
        assert!(validate_args_line("-Dvalue=\"missing", "test.args").is_err());
        assert!(split_windows_args("").unwrap().is_empty());
    }
    #[test]
    fn configuration_groups_and_absolute_paths() {
        let dir = std::env::temp_dir().join(format!("cis-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = format!(
            r#"
[general]
manage_dir = '.'
java_path = '{}'
java_vm_args = '-Xms64m -Xmx128m'
java_system_args = ['-Dfile.encoding=utf-8 -Duser.timezone=UTC']
java_agent_args = ''
java_spring_args = ['--spring.config.location=file:./application.yml,file:./bootstrap.yaml']
[[apps]]
name = 'zeta'
type = 'java'
source_files = ['{}']
java_vm_args = '-Xms32m -Xmx256m'
java_system_args = ['-Dfile.encoding=utf-8 -Dspring.profiles.active=test']
[[apps]]
name = 'alpha'
type = 'future-type'
"#,
            dir.join("jdk space/bin/java.exe").display(),
            dir.join("alpha.jar").display()
        );
        let path = dir.join("conf.toml");
        std::fs::write(&path, &text).unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.apps[0].name, "alpha");
        assert!(c.apps[0].source_files.is_empty());
        assert_eq!(c.apps[1].source_files[0], dir.join("alpha.jar"));
        assert_eq!(c.java(&c.apps[1]), dir.join("jdk space/bin/java.exe"));
        assert_eq!(
            c.system_args(&c.apps[0]),
            ["-Dfile.encoding=utf-8 -Duser.timezone=UTC"]
        );
        assert!(
            c.log_dir(&c.apps[0])
                .unwrap()
                .ends_with(Path::new("alpha").join("logs"))
        );
        assert_eq!(c.vm_args(&c.apps[1]), "-Xms32m -Xmx256m");
        assert_eq!(c.vm_args(&c.apps[0]), "-Xms64m -Xmx128m");
        assert_eq!(
            c.system_args(&c.apps[1]),
            ["-Dfile.encoding=utf-8 -Dspring.profiles.active=test"]
        );
        let argv =
            crate::process::arguments(&crate::manager::configured_command(&c, &c.apps[1]).unwrap());
        assert_eq!(argv[0], c.java(&c.apps[1]).to_string_lossy());
        assert_eq!(argv[1], "-Xms32m");
        assert_eq!(argv[2], "-Xmx256m");
        assert_eq!(argv[3], "-Dfile.encoding=utf-8");
        assert_eq!(argv[4], "-Dspring.profiles.active=test");
        assert_eq!(argv[5], "-jar");
        assert_eq!(PathBuf::from(&argv[6]), c.jar(&c.apps[1]));
        assert_eq!(
            argv[7],
            "--spring.config.location=file:./application.yml,file:./bootstrap.yaml"
        );

        let mut custom = c.apps[1].clone();
        custom.java_vm_args = Some("-Djava.exe={java_path}".into());
        let argv =
            crate::process::arguments(&crate::manager::configured_command(&c, &custom).unwrap());
        assert_eq!(argv[1], format!("-Djava.exe={}", c.java(&custom).display()));
        custom.java_vm_args = Some("-Dinvalid={unknown}".into());
        assert!(crate::manager::configured_command(&c, &custom).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repository_conf_uses_new_schema() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("conf.toml");
        let raw = std::fs::read_to_string(path).unwrap();
        let config: Config = toml::from_str(&raw).unwrap();
        assert_eq!(config.apps.len(), 13);
        assert!(
            config
                .apps
                .iter()
                .all(|app| app.app_type == "java" && !app.source_files.is_empty())
        );
        assert_eq!(config.general.java_vm_args, "-Xms64m -Xmx128m");
    }
}
