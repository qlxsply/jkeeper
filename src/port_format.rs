use std::collections::BTreeSet;

/// Normalize, de-duplicate and sort ports by protocol, then numeric port.
pub fn normalize(ports: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut unique = BTreeSet::<(u8, u16)>::new();
    for text in ports {
        let Some((protocol, port)) = text.as_ref().trim().split_once(':') else {
            continue;
        };
        let protocol = match protocol.trim().to_ascii_uppercase().as_str() {
            "TCP" => 0,
            "UDP" => 1,
            _ => continue,
        };
        let Ok(port) = port.trim().parse::<u16>() else {
            continue;
        };
        unique.insert((protocol, port));
    }
    unique
        .into_iter()
        .map(|(protocol, port)| format!("{}:{port}", if protocol == 0 { "TCP" } else { "UDP" }))
        .collect()
}

/// Display all ports through three; strictly fold the third and later ports above three.
pub fn table_summary(ports: &[String]) -> String {
    let normalized = normalize(ports.iter().map(String::as_str));
    match normalized.len() {
        0 => "—".into(),
        1..=3 => normalized.join(", "),
        _ => format!("{}, {}, ...", normalized[0], normalized[1]),
    }
}
