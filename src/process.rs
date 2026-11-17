use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{Diagnostics::ToolHelp::*, ProcessStatus::*, Threading::*},
};

pub fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
pub fn error() -> String {
    std::io::Error::last_os_error().to_string()
}
pub fn show_startup_error(message: &str) {
    let text = wide(message);
    let caption = wide("jkeeper");
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }
}
pub struct Handle(pub HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        info: *mut c_void,
        len: u32,
        returned: *mut u32,
    ) -> i32;
}

pub fn command_line(h: &Handle) -> Result<String, String> {
    unsafe {
        let mut size = 0;
        NtQueryInformationProcess(h.0, 60, null_mut(), 0, &mut size);
        if size == 0 || size > 1024 * 1024 {
            return Err("无法读取进程命令行".into());
        }
        let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        let result = NtQueryInformationProcess(h.0, 60, data.as_mut_ptr().cast(), size, &mut size);
        if result < 0 {
            return Err(format!("读取进程命令行失败: NTSTATUS {result:#x}"));
        }
        let s = &*data.as_ptr().cast::<UnicodeString>();
        let begin = data.as_ptr() as usize;
        let end = begin + data.len() * size_of::<usize>();
        let ptr = s.buffer as usize;
        if ptr < begin
            || ptr.checked_add(s.length as usize).is_none_or(|p| p > end)
            || s.length % 2 != 0
        {
            return Err("无效进程命令行数据".into());
        }
        Ok(String::from_utf16_lossy(std::slice::from_raw_parts(
            s.buffer,
            s.length as usize / 2,
        )))
    }
}
pub fn open(pid: u32, terminate: bool) -> Result<Handle, String> {
    let rights = PROCESS_QUERY_LIMITED_INFORMATION
        | PROCESS_VM_READ
        | SYNCHRONIZE
        | if terminate { PROCESS_TERMINATE } else { 0 };
    let h = unsafe { OpenProcess(rights, 0, pid) };
    if h.is_null() {
        Err(error())
    } else {
        Ok(Handle(h))
    }
}
pub fn alive(h: &Handle) -> bool {
    unsafe { WaitForSingleObject(h.0, 0) == WAIT_TIMEOUT }
}
pub fn creation(h: &Handle) -> Result<u64, String> {
    unsafe {
        let mut a: FILETIME = std::mem::zeroed();
        let mut b = a;
        let mut c = a;
        let mut d = a;
        if GetProcessTimes(h.0, &mut a, &mut b, &mut c, &mut d) == 0 {
            return Err(error());
        }
        Ok((a.dwHighDateTime as u64) << 32 | a.dwLowDateTime as u64)
    }
}
pub fn memory(h: &Handle) -> Result<usize, String> {
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(
            h.0,
            &mut counters,
            size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ) == 0
        {
            Err(error())
        } else {
            Ok(counters.WorkingSetSize)
        }
    }
}
pub fn java_pids() -> Result<Vec<u32>, String> {
    unsafe {
        let h = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
        if h.0 == INVALID_HANDLE_VALUE {
            return Err(error());
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut result = Vec::new();
        let mut ok = Process32FirstW(h.0, &mut e);
        if ok == 0 {
            return Err(error());
        }
        while ok != 0 {
            let name = String::from_utf16_lossy(
                &e.szExeFile[..e
                    .szExeFile
                    .iter()
                    .position(|x| *x == 0)
                    .unwrap_or(e.szExeFile.len())],
            )
            .to_lowercase();
            if name == "java.exe" || name == "javaw.exe" {
                result.push(e.th32ProcessID);
            }
            ok = Process32NextW(h.0, &mut e);
        }
        Ok(result)
    }
}
// Parse Windows argv using the system parser, not substring matching.
#[link(name = "shell32")]
unsafe extern "system" {
    fn CommandLineToArgvW(command: *const u16, count: *mut i32) -> *mut *mut u16;
}
pub fn arguments(command: &str) -> Vec<String> {
    unsafe {
        let mut count = 0;
        let ptr = CommandLineToArgvW(wide(command).as_ptr(), &mut count);
        if ptr.is_null() {
            return vec![];
        }
        let result = std::slice::from_raw_parts(ptr, count as usize)
            .iter()
            .map(|p| {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(*p, n))
            })
            .collect();
        LocalFree(ptr.cast());
        result
    }
}
pub fn ordinary_path(p: &Path) -> std::path::PathBuf {
    // Java's JAR class loader does not reliably accept Win32 verbatim (\\?\) paths.
    let s = p.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}").into()
    } else if let Some(local) = s.strip_prefix(r"\\?\") {
        local.into()
    } else {
        p.into()
    }
}
pub fn path_key(p: &Path) -> String {
    ordinary_path(p)
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase()
}
pub fn matches_jar(command: &str, jar: &Path) -> bool {
    jar_key(command).as_deref() == Some(path_key(jar).as_str())
}
pub fn jar_key(command: &str) -> Option<String> {
    let args = arguments(command);
    args.windows(2)
        .find(|v| v[0] == "-jar")
        .filter(|v| Path::new(&v[1]).is_absolute())
        .map(|v| path_key(Path::new(&v[1])))
}
pub fn verify(pid: u32, created: u64, jar: &Path, kill: bool) -> Result<Handle, String> {
    let h = open(pid, kill)?;
    if !alive(&h) || creation(&h)? != created {
        return Err("进程已退出或 PID 已重用".into());
    }
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(h.0, 0, buf.as_mut_ptr(), &mut len) } == 0 {
        return Err(error());
    }
    let image = String::from_utf16_lossy(&buf[..len as usize]);
    let name = Path::new(&image)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if !matches!(name.as_str(), "java.exe" | "javaw.exe") || !matches_jar(&command_line(&h)?, jar) {
        return Err("进程身份不匹配".into());
    }
    Ok(h)
}
pub fn terminate(h: &Handle, timeout: f64) -> Result<(), String> {
    unsafe {
        if !alive(h) {
            return Ok(());
        }
        if TerminateProcess(h.0, 1) == 0 {
            return Err(error());
        }
        if WaitForSingleObject(h.0, (timeout * 1000.0) as u32) != WAIT_OBJECT_0 {
            return Err("强制停止超时".into());
        }
    }
    Ok(())
}
pub fn replace(temp: &Path, target: &Path) -> Result<(), String> {
    if unsafe {
        MoveFileExW(
            wide(temp).as_ptr(),
            wide(target).as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(error())
    } else {
        Ok(())
    }
}
pub fn global_instance_lock() -> Result<Handle, String> {
    let name = wide("Global\\jkeeper-single-instance");
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if h.is_null() {
            return Err(format!("无法创建全局实例互斥锁: {}", error()));
        }
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let h = Handle(h);
        if exists {
            Err("已有一个 jkeeper.exe 正在运行，请先关闭它".into())
        } else {
            Ok(h)
        }
    }
}
