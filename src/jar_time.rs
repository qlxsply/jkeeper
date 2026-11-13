use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

type Cache = HashMap<PathBuf, (u64, SystemTime, String)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
pub fn display(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return "—".into();
    };
    let Ok(modified) = meta.modified() else {
        return "—".into();
    };
    if let Ok(cache) = CACHE.get_or_init(Default::default).lock()
        && let Some((size, time, text)) = cache.get(path)
        && *size == meta.len()
        && *time == modified
    {
        return text.clone();
    }
    let text = build_time(path).unwrap_or_else(|| crate::manager::format_time(modified));
    if let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() {
        if cache.len() > 1024 {
            cache.clear();
        }
        cache.insert(path.into(), (meta.len(), modified, text.clone()));
    }
    text
}
fn build_time(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    // Only Spring Boot's explicitly generated build.time is interpreted as build metadata.
    for name in [
        "META-INF/build-info.properties",
        "BOOT-INF/classes/META-INF/build-info.properties",
    ] {
        if let Ok(entry) = archive.by_name(name) {
            if entry.size() > 64 * 1024 {
                continue;
            }
            let mut text = String::new();
            entry.take(64 * 1024 + 1).read_to_string(&mut text).ok()?;
            for line in text.lines() {
                if let Some((key, value)) = line.split_once('=')
                    && key.trim() == "build.time"
                {
                    let value = value.trim().replace("\\:", ":");
                    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&value) {
                        return Some(
                            t.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M:%S")
                                .to_string(),
                        );
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn explicit_build_metadata_and_fallback() {
        let path = std::env::temp_dir().join(format!("cis-jar-time-{}.jar", std::process::id()));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(
            "META-INF/build-info.properties",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"build.time=2026-01-02T03\\:04\\:05Z\n")
            .unwrap();
        zip.finish().unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert_eq!(display(&path), expected);
        std::fs::write(&path, b"not a zip").unwrap();
        assert_eq!(
            display(&path),
            crate::manager::format_time(std::fs::metadata(&path).unwrap().modified().unwrap())
        );
        std::fs::remove_file(path).unwrap();
    }
}
