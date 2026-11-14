use crate::{
    AppWindow, Service,
    config::{App as AppConfig, Config},
    manager::{self, Status},
    process,
};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

fn persist_window_geometry(
    window: &slint::Window,
    path: &Path,
    last_saved: &RefCell<Option<crate::window_state::WindowState>>,
) {
    let state = crate::window_state::WindowState::current(window);
    if last_saved.borrow().as_ref() == Some(&state) {
        return;
    }
    match crate::window_state::WindowState::write_atomic(path, state) {
        Ok(()) => *last_saved.borrow_mut() = Some(state),
        Err(error) => tracing::warn!("保存窗口状态失败: {error}"),
    }
}

enum Event {
    Snapshot(Vec<Status>),
    Status(String),
    Done,
}
#[derive(Clone)]
struct Events {
    tx: mpsc::Sender<Event>,
    ui: slint::Weak<AppWindow>,
}
impl Events {
    fn send(&self, event: Event) -> Result<(), String> {
        self.tx.send(event).map_err(|e| e.to_string())?;
        self.ui
            .upgrade_in_event_loop(|ui| ui.invoke_dispatch_events())
            .map_err(|e| e.to_string())
    }
}
struct View {
    config: Arc<Config>,
    statuses: Vec<Status>,
    selected: HashSet<usize>,
    busy: Arc<AtomicBool>,
    closing: bool,
    rows: Rc<VecModel<Service>>,
    first_snapshot: bool,
    scan_error: bool,
    detail_index: Option<usize>,
}
fn service_model(app: &AppConfig, status: &Status, selected: bool) -> Service {
    let command_lines = status
        .command_line
        .lines()
        .map(|line| line.chars().count().max(1).div_ceil(84))
        .sum::<usize>()
        .max(1) as i32;
    Service {
        name: app.name.clone().into(),
        pid: status
            .identity
            .as_ref()
            .map(|p| p.pid.to_string())
            .unwrap_or_else(|| "—".into())
            .into(),
        current_memory: status
            .current_memory
            .clone()
            .unwrap_or_else(|| "—".into())
            .into(),
        ports_summary: crate::port_format::table_summary(&status.ports).into(),
        ports_count: status.ports.len() as i32,
        command_lines,
        ports_full: if status.ports.is_empty() {
            "—".into()
        } else {
            status.ports.join("\n").into()
        },
        started: status.started.clone().into(),
        jar_time: status.jar_time.clone().into(),
        command_line: status.command_line.clone().into(),
        running: status.identity.is_some(),
        uncertain: status.uncertain,
        problem: !status.note.is_empty(),
        selected,
        can_start: status.can_start,
        can_update: status.can_update,
        can_restart: status.can_restart,
    }
}
impl View {
    fn render(&mut self, ui: &AppWindow) {
        let rows: Vec<Service> = self
            .config
            .apps
            .iter()
            .enumerate()
            .map(|(i, a)| service_model(a, &self.statuses[i], self.selected.contains(&i)))
            .collect();
        if self.rows.row_count() != rows.len() {
            self.rows.set_vec(rows);
        } else {
            for (index, row) in rows.into_iter().enumerate() {
                if self.rows.row_data(index).as_ref() != Some(&row) {
                    self.rows.set_row_data(index, row);
                }
            }
        }
        ui.set_busy(self.busy.load(Ordering::Acquire));
        ui.set_bulk_start(self.selected.iter().any(|i| self.statuses[*i].can_start));
        ui.set_bulk_stop(
            self.selected
                .iter()
                .any(|i| self.statuses[*i].identity.is_some() && !self.statuses[*i].uncertain),
        );
        ui.set_bulk_restart(self.selected.iter().any(|i| self.statuses[*i].can_restart));
        ui.set_bulk_update(self.selected.iter().any(|i| self.statuses[*i].can_update));
        ui.set_all_selected(
            !self.config.apps.is_empty() && self.selected.len() == self.config.apps.len(),
        );
        if let Some(index) = self.detail_index {
            ui.set_detail_service(service_model(
                &self.config.apps[index],
                &self.statuses[index],
                false,
            ));
        }
    }
    fn set_status(&mut self, ui: &AppWindow, text: String) {
        ui.set_status(if self.closing {
            "当前任务完成后退出".into()
        } else {
            text.into()
        });
    }
}
pub fn claim(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}
fn submit(
    view: &Rc<RefCell<View>>,
    ui: &AppWindow,
    indices: Vec<usize>,
    action: String,
    tx: Events,
    wake: mpsc::SyncSender<()>,
) {
    if indices.is_empty() {
        view.borrow_mut().set_status(ui, "请先选择应用".into());
        return;
    }
    let mut v = view.borrow_mut();
    if !claim(&v.busy) {
        return;
    }
    let c = v.config.clone();
    v.set_status(ui, format!("开始执行 {action}"));
    v.render(ui);
    drop(v);
    let fallback = tx.clone();
    let spawn = std::thread::Builder::new()
        .name("management-task".into())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::scheduler::run_batch(
                    indices,
                    |index| manager::execute(&c, index, &action),
                    |index, result| {
                        let text = match result {
                            Ok(s) => s,
                            Err(e) => format!("失败：{e}"),
                        };
                        let _ = tx.send(Event::Status(format!("{}：{text}", c.apps[index].name)));
                    },
                );
            }));
            if outcome.is_err() {
                let _ = tx.send(Event::Status("后台任务发生异常，已结束当前任务".into()));
            }
            let _ = tx.send(Event::Done);
            let _ = wake.try_send(());
        });
    if let Err(e) = spawn {
        let _ = fallback.send(Event::Status(format!("无法创建任务线程: {e}")));
        let _ = fallback.send(Event::Done);
    }
}
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let logging = crate::logging::init();
    let exe = std::env::current_exe()?;
    let base = exe.parent().unwrap();
    let c = match Config::load(&base.join("conf.toml")) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!("{e}");
            process::show_startup_error(&format!("{e}\n请修正 conf.toml 后重新启动 jkeeper。"));
            return Ok(());
        }
    };
    let _lock = match process::cluster_lock(&c.general.manage_dir) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("{e}");
            process::show_startup_error(&format!("{e}\njkeeper 将退出。"));
            return Ok(());
        }
    };
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    let ui = AppWindow::new()?;
    let window = ui.window();
    let scale = window.scale_factor();
    let min_width = (1480.0 * scale).ceil() as u32;
    let min_height = (650.0 * scale).ceil() as u32;
    let state_path = crate::window_state::WindowState::path(&c.general.manage_dir);
    let restored = match crate::window_state::WindowState::read(&state_path, min_width, min_height)
    {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("{error}，使用屏幕居中默认位置");
            None
        }
    };
    if let Some(state) = restored {
        // Apply the persisted geometry before the native window is first shown,
        // so the window never flashes at a temporary centered position.
        state.apply(window);
    } else {
        crate::window_state::WindowState::center(window);
    }
    let last_window_state = Rc::new(RefCell::new(restored));
    let state_timer = Timer::default();
    {
        let weak = ui.as_weak();
        let state_path = state_path.clone();
        let last = last_window_state.clone();
        state_timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
            if let Some(ui) = weak.upgrade() {
                persist_window_geometry(ui.window(), &state_path, &last);
            }
        });
    }
    ui.set_directory(c.general.manage_dir.display().to_string().into());
    let v = Rc::new(RefCell::new(View {
        config: c.clone(),
        statuses: vec![Status::default(); c.apps.len()],
        selected: HashSet::new(),
        busy: Arc::new(AtomicBool::new(false)),
        closing: false,
        rows: Rc::new(VecModel::default()),
        first_snapshot: true,
        scan_error: false,
        detail_index: None,
    }));
    ui.set_services(ModelRc::from(v.borrow().rows.clone()));
    if let Err(e) = logging {
        tracing::error!("管理器日志不可用: {e}");
    }
    ui.set_status("正在扫描服务".into());
    v.borrow_mut().render(&ui);
    let (tx, rx) = mpsc::channel();
    let tx = Events {
        tx,
        ui: ui.as_weak(),
    };
    let (wake, monitor_rx) = mpsc::sync_channel(1);
    let monitor_tx = tx.clone();
    let monitor_c = c.clone();
    std::thread::Builder::new()
        .name("monitor".into())
        .spawn(move || {
            loop {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    manager::scan(&monitor_c)
                }));
                let event = match result {
                    Ok(s) => Event::Snapshot(s),
                    Err(_) => Event::Status("状态监控异常，稍后重试".into()),
                };
                if monitor_tx.send(event).is_err() {
                    break;
                }
                if let Err(mpsc::RecvTimeoutError::Disconnected) =
                    monitor_rx.recv_timeout(Duration::from_secs(3))
                {
                    break;
                }
            }
        })?;
    {
        let v = v.clone();
        let weak = ui.as_weak();
        ui.on_open_detail(move |index| {
            if let Some(ui) = weak.upgrade() {
                let mut v = v.borrow_mut();
                let index = index as usize;
                if v.detail_index.is_none() && index < v.config.apps.len() {
                    v.detail_index = Some(index);
                    v.render(&ui);
                    ui.set_detail_open(true);
                }
            }
        });
    }
    {
        let v = v.clone();
        let weak = ui.as_weak();
        ui.on_close_detail(move || {
            if let Some(ui) = weak.upgrade() {
                v.borrow_mut().detail_index = None;
                ui.set_detail_open(false);
            }
        });
    }
    {
        let v = v.clone();
        let weak = ui.as_weak();
        ui.on_select(move |row, selected| {
            if let Some(ui) = weak.upgrade() {
                let mut v = v.borrow_mut();
                let i = row as usize;
                if i < v.config.apps.len() {
                    if selected {
                        v.selected.insert(i);
                    } else {
                        v.selected.remove(&i);
                    }
                    v.render(&ui);
                }
            }
        });
    }
    {
        let v = v.clone();
        let weak = ui.as_weak();
        ui.on_select_all(move |selected| {
            if let Some(ui) = weak.upgrade() {
                let mut v = v.borrow_mut();
                for i in 0..v.config.apps.len() {
                    if selected {
                        v.selected.insert(i);
                    } else {
                        v.selected.remove(&i);
                    }
                }
                v.render(&ui);
            }
        });
    }
    {
        let v = v.clone();
        let weak = ui.as_weak();
        let tx = tx.clone();
        let wake = wake.clone();
        ui.on_action(move |row, action| {
            if let Some(ui) = weak.upgrade() {
                let index = row as usize;
                if index < v.borrow().config.apps.len() {
                    if action == "log" {
                        let c = v.borrow().config.clone();
                        let tx = tx.clone();
                        std::thread::spawn(move || {
                            let result = manager::open_app_folder(&c, index).unwrap_or_else(|e| e);
                            let _ = tx.send(Event::Status(result));
                        });
                    } else {
                        submit(
                            &v,
                            &ui,
                            vec![index],
                            action.to_string(),
                            tx.clone(),
                            wake.clone(),
                        );
                    }
                }
            }
        });
    }
    {
        let v = v.clone();
        let weak = ui.as_weak();
        let tx = tx.clone();
        let wake = wake.clone();
        ui.on_bulk(move |action| {
            if let Some(ui) = weak.upgrade() {
                let mut indices: Vec<_> = v.borrow().selected.iter().copied().collect();
                indices.sort_unstable();
                submit(
                    &v,
                    &ui,
                    indices,
                    action.to_string(),
                    tx.clone(),
                    wake.clone(),
                );
            }
        });
    }
    ui.on_refresh(move || {
        let _ = wake.try_send(());
    });
    {
        let v = v.clone();
        let weak = ui.as_weak();
        let state_path = state_path.clone();
        let last_window_state = last_window_state.clone();
        ui.window().on_close_requested(move || {
            let mut v = v.borrow_mut();
            if let Some(ui) = weak.upgrade() {
                persist_window_geometry(ui.window(), &state_path, &last_window_state);
            }
            if v.busy.load(Ordering::Acquire) {
                v.closing = true;
                if let Some(ui) = weak.upgrade() {
                    ui.set_status("当前任务完成后退出".into());
                }
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                slint::CloseRequestResponse::HideWindow
            }
        });
    }
    let weak = ui.as_weak();
    ui.on_dispatch_events(move || {
        if let Some(ui) = weak.upgrade() {
            let mut v = v.borrow_mut();
            if let Ok(event) = rx.try_recv() {
                match event {
                    Event::Snapshot(s) => {
                        let first_snapshot = v.first_snapshot;
                        let error = s
                            .iter()
                            .enumerate()
                            .find(|(_, status)| !status.note.is_empty())
                            .map(|(index, status)| {
                                format!("异常 · {}：{}", v.config.apps[index].name, status.note)
                            });
                        for (index, status) in s.iter().enumerate() {
                            if !status.note.is_empty() && status.note != v.statuses[index].note {
                                tracing::warn!(app = %v.config.apps[index].name, "{}", status.note);
                            }
                        }
                        v.statuses = s;
                        if !v.busy.load(Ordering::Acquire) {
                            if let Some(error) = error {
                                v.scan_error = true;
                                ui.set_status(error.into());
                            } else if v.scan_error {
                                v.scan_error = false;
                                ui.set_status("监控中".into());
                            }
                        }
                        if v.first_snapshot {
                            v.first_snapshot = false;
                            if !v.scan_error {
                                ui.set_status("监控中".into());
                            }
                        }
                        v.render(&ui);
                        if first_snapshot {
                            // Request a full redraw after the initial service model is populated.
                            // Avoid resizing/repositioning the native window after it is shown.
                            ui.window().request_redraw();
                        }
                    }
                    Event::Status(text) => v.set_status(&ui, text),
                    Event::Done => {
                        v.busy.store(false, Ordering::Release);
                        v.render(&ui);
                        if v.closing {
                            persist_window_geometry(ui.window(), &state_path, &last_window_state);
                            let _ = slint::quit_event_loop();
                        }
                    }
                }
            }
        }
    });
    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_submission_is_exclusive() {
        let flag = Arc::new(AtomicBool::new(false));
        let threads: Vec<_> = (0..20)
            .map(|_| {
                let flag = flag.clone();
                std::thread::spawn(move || claim(&flag))
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|t| t.join().ok())
                .filter(|won| *won)
                .count(),
            1
        );
        flag.store(false, Ordering::Release);
        assert!(claim(&flag));
    }
}
