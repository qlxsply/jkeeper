use crate::{
    AppWindow, Service,
    config::Config,
    manager::{self, Status},
    process,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

enum Event {
    Snapshot(Vec<Status>),
    Record(String),
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
    records: Vec<String>,
    rows: Rc<VecModel<Service>>,
    first_snapshot: bool,
    scan_error: bool,
}
impl View {
    fn render(&mut self, ui: &AppWindow) {
        let rows: Vec<Service> = self
            .config
            .apps
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let a = &self.config.apps[i];
                let s = &self.statuses[i];
                Service {
                    name: a.name.clone().into(),
                    pid: s
                        .identity
                        .as_ref()
                        .map(|p| p.pid.to_string())
                        .unwrap_or_else(|| "—".into())
                        .into(),
                    started: s.started.clone().into(),
                    memory: s.memory.clone().into(),
                    ports: s.ports.clone().into(),
                    jar_time: s.jar_time.clone().into(),
                    running: s.identity.is_some(),
                    uncertain: s.uncertain,
                    problem: !s.note.is_empty(),
                    selected: self.selected.contains(&i),
                    can_start: s.can_start,
                    can_update: s.can_update,
                    can_restart: s.can_restart,
                }
            })
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
    }
    fn record(&mut self, ui: &AppWindow, text: String) {
        let line = format!("{}  {}", chrono::Local::now().format("%H:%M:%S"), text);
        tracing::info!("{text}");
        self.records.insert(0, line);
        self.records.truncate(100);
        ui.set_records(self.records.join("\n").into());
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
        view.borrow_mut().record(ui, "请先选择应用".into());
        return;
    }
    let mut v = view.borrow_mut();
    if !claim(&v.busy) {
        return;
    }
    let c = v.config.clone();
    v.record(ui, format!("开始执行 {action}"));
    v.render(ui);
    drop(v);
    let fallback = tx.clone();
    let spawn = std::thread::Builder::new()
        .name("management-task".into())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::scheduler::run_batch(
                    indices,
                    |index| {
                        let snapshot = manager::scan(&c);
                        let s = &snapshot[index];
                        let skip = match action.as_str() {
                            "start" => s.identity.is_some() && !s.uncertain,
                            "stop" => s.identity.is_none() && !s.uncertain,
                            "update" => s.identity.is_some() && !s.uncertain,
                            _ => false,
                        };
                        if skip {
                            return Ok("跳过不适用操作".into());
                        }
                        let _ = tx.send(Event::Record(format!(
                            "{}：正在执行 {action}",
                            c.apps[index].name
                        )));
                        manager::execute(&c, index, &action)
                    },
                    |index, result| {
                        let text = match result {
                            Ok(s) => s,
                            Err(e) => format!("失败：{e}"),
                        };
                        let _ = tx.send(Event::Record(format!("{}：{text}", c.apps[index].name)));
                    },
                );
            }));
            if outcome.is_err() {
                let _ = tx.send(Event::Record("后台任务发生异常，已结束当前任务".into()));
            }
            let _ = tx.send(Event::Done);
            let _ = wake.try_send(());
        });
    if let Err(e) = spawn {
        let _ = fallback.send(Event::Record(format!("无法创建任务线程: {e}")));
        let _ = fallback.send(Event::Done);
    }
}
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let logging = crate::logging::init();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    let ui = AppWindow::new()?;
    let exe = std::env::current_exe()?;
    let base = exe.parent().unwrap();
    let config = Config::load(&base.join("conf"));
    let c = match config {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!("{e}");
            ui.set_enabled(false);
            ui.set_records(e.clone().into());
            ui.set_status(format!("{e}；修正 conf 后请重新打开管理器").into());
            return ui.run().map_err(Into::into);
        }
    };
    let _lock = match process::cluster_lock(&c.general.manage_dir) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("{e}");
            ui.set_enabled(false);
            ui.set_records(e.clone().into());
            ui.set_status(e.into());
            return ui.run().map_err(Into::into);
        }
    };
    ui.set_directory(c.general.manage_dir.display().to_string().into());
    let v = Rc::new(RefCell::new(View {
        config: c.clone(),
        statuses: vec![Status::default(); c.apps.len()],
        selected: HashSet::new(),
        busy: Arc::new(AtomicBool::new(false)),
        closing: false,
        records: vec![],
        rows: Rc::new(VecModel::default()),
        first_snapshot: true,
        scan_error: false,
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
                    Err(_) => Event::Record("状态监控异常，稍后重试".into()),
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
                            let result = manager::open_log(&c, index).unwrap_or_else(|e| e);
                            let _ = tx.send(Event::Record(result));
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
        ui.window().on_close_requested(move || {
            let mut v = v.borrow_mut();
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
            for event in rx.try_iter() {
                match event {
                    Event::Snapshot(s) => {
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
                    }
                    Event::Record(text) => v.record(&ui, text),
                    Event::Done => {
                        v.busy.store(false, Ordering::Release);
                        v.render(&ui);
                        if v.closing {
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
