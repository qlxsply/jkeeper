use std::collections::{BTreeSet, HashMap};
use windows_sys::Win32::{
    Foundation::ERROR_INSUFFICIENT_BUFFER,
    NetworkManagement::IpHelper::*,
    Networking::WinSock::{AF_INET, AF_INET6},
};

pub fn ports() -> Result<HashMap<u32, Vec<String>>, String> {
    let mut result = HashMap::<u32, BTreeSet<String>>::new();
    for ipv6 in [false, true] {
        for tcp in [true, false] {
            let af = if ipv6 { AF_INET6 } else { AF_INET } as u32;
            let mut size = 0u32;
            let get = |p, size: &mut u32| unsafe {
                if tcp {
                    GetExtendedTcpTable(p, size, 0, af, TCP_TABLE_OWNER_PID_LISTENER, 0)
                } else {
                    GetExtendedUdpTable(p, size, 0, af, UDP_TABLE_OWNER_PID, 0)
                }
            };
            let mut code = get(std::ptr::null_mut(), &mut size);
            if code != 0 && code != ERROR_INSUFFICIENT_BUFFER {
                return Err(format!("端口查询失败: {code}"));
            }
            let mut data = vec![];
            for _ in 0..4 {
                data = vec![0u64; (size as usize).div_ceil(8).max(1)];
                code = get(data.as_mut_ptr().cast(), &mut size);
                if code != ERROR_INSUFFICIENT_BUFFER {
                    break;
                }
            }
            if code != 0 {
                return Err(format!("端口查询失败: {code}"));
            }
            unsafe {
                let p = data.as_ptr().cast::<u8>();
                let count = *p.cast::<u32>() as usize;
                macro_rules! rows {
                    ($ty:ty) => {{
                        if 4 + count * size_of::<$ty>() > data.len() * 8 {
                            return Err("端口表长度无效".into());
                        }
                        for i in 0..count {
                            let row = std::ptr::read_unaligned(
                                p.add(4 + i * size_of::<$ty>()).cast::<$ty>(),
                            );
                            let port = u16::from_be(row.dwLocalPort as u16);
                            result
                                .entry(row.dwOwningPid)
                                .or_default()
                                .insert(format!("{}:{port}", if tcp { "TCP" } else { "UDP" }));
                        }
                    }};
                }
                match (tcp, ipv6) {
                    (true, false) => rows!(MIB_TCPROW_OWNER_PID),
                    (true, true) => rows!(MIB_TCP6ROW_OWNER_PID),
                    (false, false) => rows!(MIB_UDPROW_OWNER_PID),
                    (false, true) => rows!(MIB_UDP6ROW_OWNER_PID),
                }
            }
        }
    }
    Ok(result
        .into_iter()
        .map(|(pid, ports)| (pid, crate::port_format::normalize(ports)))
        .collect())
}
