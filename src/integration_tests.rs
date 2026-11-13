//! Tests launch only an isolated Java fixture in a uniquely allocated temporary directory.
use crate::{config::Config, manager, process};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

struct Fixture {
    dir: PathBuf,
    config: Config,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "fixture {} stdout: {:?}\nstderr: {}",
                self.dir.display(),
                fs::read_to_string(self.dir.join("fixture/logs/console.out.log")),
                String::from_utf8_lossy(
                    &fs::read(self.dir.join("fixture/logs/console.err.log")).unwrap_or_default()
                )
            );
        }
        for a in &self.config.apps {
            if let Ok(bytes) = fs::read(self.config.pid(a))
                && let Ok(id) = serde_json::from_slice::<manager::Identity>(&bytes)
                && let Ok(h) = process::verify(id.pid, id.created, &self.config.jar(a), true)
            {
                let _ = process::terminate(&h, 5.0);
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "cis-fixture-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    fs::create_dir_all(dir.join("fixture")).unwrap();
    fs::write(
        dir.join("TestService.java"),
        include_str!("../tests/fixtures/TestService.java"),
    )
    .unwrap();
    let java_home = std::env::var_os("CIS_TEST_JAVA_HOME").map(PathBuf::from);
    let tool = |name: &str| {
        java_home
            .as_ref()
            .map(|home| home.join("bin").join(format!("{name}.exe")))
            .unwrap_or_else(|| PathBuf::from(format!("{name}.exe")))
    };
    let mut compiler = Command::new(tool("javac"));
    if java_home.is_none() {
        compiler.arg("--release").arg("8");
    }
    let output = compiler
        .arg("TestService.java")
        .current_dir(&dir)
        .output()
        .expect("JDK javac is required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(tool("jar"))
        .args([
            "cfe",
            "fixture/fixture.jar",
            "TestService",
            "TestService.class",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let java = if java_home.is_some() {
        tool("java").to_string_lossy().to_string()
    } else {
        let found = Command::new("where.exe").arg("java.exe").output().unwrap();
        String::from_utf8_lossy(&found.stdout)
            .lines()
            .next()
            .expect("java.exe on PATH")
            .trim()
            .to_string()
    };
    fs::write(
        dir.join("conf"),
        format!(
            r#"
[general]
manage_dir = '.'
java_path = '{java}'
min_memory = '32m'
max_memory = '96m'
start_check_seconds = 0.2
force_kill_timeout_seconds = 5
[[apps]]
name = 'fixture'
source_jar = 'source.jar'
"#
        ),
    )
    .unwrap();
    fs::write(dir.join("fixture/application.yml"), "# test\n").unwrap();
    fs::write(dir.join("fixture/bootstrap.yaml"), "# test\n").unwrap();
    fs::copy(dir.join("fixture/fixture.jar"), dir.join("source.jar")).unwrap();
    let config = Config::load(&dir.join("conf")).unwrap();
    Fixture { dir, config }
}

#[test]
#[ignore = "Requires JDK on PATH; launches isolated Java fixture"]
fn java_lifecycle_and_native_monitoring() {
    let f = fixture();
    let c = &f.config;
    assert!(manager::scan(c)[0].can_start);
    assert!(manager::execute(c, 0, "start").unwrap().contains("已启动"));
    let until = Instant::now() + Duration::from_secs(15);
    while !f.dir.join("fixture/ready.txt").exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    let expected = fs::read_to_string(f.dir.join("fixture/ready.txt")).unwrap();
    let s = manager::scan(c).remove(0);
    let id = s.identity.unwrap();
    assert!(!s.uncertain, "{}", s.note);
    assert!(!s.can_update);
    for port in expected.lines() {
        assert!(
            s.ports.split(", ").any(|p| p == port),
            "missing {port} in {}",
            s.ports
        );
    }
    let h = process::verify(id.pid, id.created, &c.jar(&c.apps[0]), false).unwrap();
    assert!(process::memory(&h).unwrap() > 0);
    assert!(process::verify(id.pid, id.created + 1, &c.jar(&c.apps[0]), true).is_err());
    assert!(process::verify(id.pid, id.created, &f.dir.join("other.jar"), true).is_err());
    assert!(manager::execute(c, 0, "update").is_err());
    assert!(manager::execute(c, 0, "start").unwrap().contains("跳过"));
    fs::remove_file(f.dir.join("fixture/application.yml")).unwrap();
    assert!(manager::execute(c, 0, "restart").is_err());
    assert!(
        process::alive(&h),
        "restart precondition failure killed existing service"
    );
    fs::write(f.dir.join("fixture/application.yml"), "# restored\n").unwrap();
    assert!(manager::execute(c, 0, "restart").is_ok());
    assert!(!process::alive(&h));
    let restarted = manager::scan(c).remove(0).identity.unwrap();
    assert_ne!(restarted.created, id.created);
    assert!(manager::execute(c, 0, "stop").unwrap().contains("强制停止"));
    assert!(!c.pid(&c.apps[0]).exists());
    manager::execute(c, 0, "update").unwrap();
    assert!(c.jar(&c.apps[0]).with_extension("jar.bak").is_file());
    assert!(c.log(&c.apps[0]).is_file());
    assert!(
        fs::read_to_string(f.dir.join("fixture/logs/console.out.log"))
            .unwrap()
            .contains("ready")
    );
    assert!(
        fs::read_to_string(f.dir.join("fixture/logs/console.err.log"))
            .unwrap()
            .contains("ready")
    );
    fs::remove_file(f.dir.join("fixture/fixture.jar")).unwrap();
    assert!(manager::execute(c, 0, "start").is_err());
    manager::execute(c, 0, "update").unwrap();
    assert!(manager::scan(c)[0].can_start);
}

#[test]
#[ignore = "Requires approximately 1 GiB free RAM and JDK on PATH; launches 50 isolated JVMs"]
fn fifty_live_services_monitoring_stress() {
    let mut fixture = fixture();
    let mut config = fixture.config.clone();
    for n in 0..50 {
        let name = format!("svc{n:02}");
        let dir = config.general.manage_dir.join(&name);
        fs::create_dir_all(&dir).unwrap();
        fs::copy(
            fixture.dir.join("source.jar"),
            dir.join(format!("{name}.jar")),
        )
        .unwrap();
        fs::write(dir.join("application.yml"), "# test\n").unwrap();
        fs::write(dir.join("bootstrap.yaml"), "# test\n").unwrap();
        config.apps.push(crate::config::App {
            name,
            jar_name: None,
            source_jar: Some(fixture.dir.join("source.jar")),
            java_path: None,
            min_memory: Some("32m".into()),
            max_memory: Some("64m".into()),
            log_file: None,
        });
    }
    config.apps.remove(0);
    config.apps.sort_by_key(|a| a.name.clone());
    let config = Config {
        apps: config.apps,
        ..config
    };
    fixture.config = config.clone();
    let all_ready = (0..50).all(|i| manager::prerequisites(&config, &config.apps[i]).is_ok());
    assert!(all_ready);
    for i in 0..50 {
        manager::execute(&config, i, "start")
            .unwrap_or_else(|e| panic!("{}: {e}", config.apps[i].name));
    }
    let executable = std::env::current_exe().unwrap();
    let target = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let manager_exe = target.join("release/cis-cluster-manager.exe");
    assert!(
        manager_exe.is_file(),
        "Build release executable before the 50-service stress test"
    );
    let test_manager = fixture.dir.join("cis-cluster-manager.exe");
    fs::copy(&manager_exe, &test_manager).unwrap();
    let mut ui = Command::new(&test_manager)
        .current_dir(&fixture.dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let ui_handle = process::open(ui.id(), false).unwrap();
    assert!(process::alive(&ui_handle));
    assert!(
        process::cluster_lock(&config.general.manage_dir).is_err(),
        "second manager must not acquire the live cluster lock"
    );
    let ui_cpu_start = process::cpu_time_100ns(&ui_handle).unwrap();
    let ui_mem_start = process::memory(&ui_handle).unwrap();
    let start = Instant::now();
    for _ in 0..10 {
        let statuses = manager::scan(&config);
        assert_eq!(statuses.len(), 50);
        assert!(
            statuses
                .iter()
                .all(|s| s.identity.is_some() && !s.uncertain),
            "{}",
            statuses
                .iter()
                .filter(|s| s.uncertain)
                .map(|s| &s.note)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
        std::thread::sleep(Duration::from_secs(3));
    }
    let elapsed = start.elapsed();
    let ui_cpu = process::cpu_time_100ns(&ui_handle)
        .unwrap()
        .saturating_sub(ui_cpu_start);
    let ui_mem_peak = process::memory(&ui_handle).unwrap();
    eprintln!(
        "50 running services: ten 3-second native scans took {elapsed:?}; UI Working Set start/peak {} / {} MiB; UI CPU {:.3}% of one logical CPU",
        ui_mem_start / 1024 / 1024,
        ui_mem_peak / 1024 / 1024,
        ui_cpu as f64 / (elapsed.as_secs_f64() * 10_000_000.0) * 100.0
    );
    ui.kill().unwrap();
    ui.wait().unwrap();
    drop(ui_handle);
    let lock = process::cluster_lock(&config.general.manage_dir)
        .expect("cluster lock is released when manager exits");
    drop(lock);
    for i in 0..50 {
        manager::execute(&config, i, "stop")
            .unwrap_or_else(|e| panic!("{}: {e}", config.apps[i].name));
    }
}

#[test]
#[ignore = "Subprocess helper; no action without CIS_TEST_CONF"]
fn launch_and_exit_helper() {
    if let Some(path) = std::env::var_os("CIS_TEST_CONF") {
        let c = Config::load(&PathBuf::from(path)).unwrap();
        manager::execute(&c, 0, "start").unwrap();
    }
}

#[test]
#[ignore = "Requires JDK on PATH; verifies launcher exit and fresh manager recovery"]
fn survives_manager_exit_and_is_recovered() {
    let f = fixture();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "integration_tests::launch_and_exit_helper",
            "--ignored",
            "--exact",
            "--nocapture",
        ])
        .env("CIS_TEST_CONF", f.dir.join("conf"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(exit) = child.try_wait().unwrap() {
            assert!(exit.success());
            break;
        }
        if Instant::now() > until {
            let _ = child.kill();
            panic!("launcher did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let fresh = Config::load(&f.dir.join("conf")).unwrap();
    let s = manager::scan(&fresh).remove(0);
    assert!(s.identity.is_some(), "{}", s.note);
    assert!(!s.uncertain);
    // The full absolute JAR identity also permits recovery if the metadata was lost.
    fs::remove_file(fresh.pid(&fresh.apps[0])).unwrap();
    assert!(manager::scan(&fresh)[0].identity.is_some());
    manager::execute(&fresh, 0, "stop").unwrap();
}
