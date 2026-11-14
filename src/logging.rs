use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    bytes: u64,
}
impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.bytes + buf.len() as u64 > 4 * 1024 * 1024 {
            self.file.take();
            let _ = std::fs::remove_file(self.path.with_extension("log.1"));
            std::fs::rename(&self.path, self.path.with_extension("log.1"))?;
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
            self.bytes = 0;
        }
        let n = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("日志文件不可用"))?
            .write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = &mut self.file {
            f.flush()
        } else {
            Ok(())
        }
    }
}
#[derive(Clone)]
struct Writer(Arc<Mutex<RotatingFile>>);
impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("日志锁异常"))?
            .write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("日志锁异常"))?
            .flush()
    }
}
pub fn init() -> Result<PathBuf, String> {
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("jkeeper")
        .join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("manager-{}.log", std::process::id()));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let bytes = file.metadata().map_err(|e| e.to_string())?.len();
    let writer = Writer(Arc::new(Mutex::new(RotatingFile {
        path: path.clone(),
        file: Some(file),
        bytes,
    })));
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .map_err(|e| e.to_string())?;
    std::panic::set_hook(Box::new(|info| tracing::error!("线程异常: {info}")));
    Ok(path)
}
