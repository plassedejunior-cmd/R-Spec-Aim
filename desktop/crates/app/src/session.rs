//! Gravação da sessão (seção 10 da spec): session.json, trials.json e raw.csv.

use std::fmt::Write as _;
use std::path::PathBuf;

use rspec_core::RawSample;
use serde_json::{json, Value};

pub fn sessions_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("RSpecAim").join("sessions")
}

pub fn save(session: &Value, trials: &Value, raw: &[(usize, RawSample)]) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = sessions_dir().join(stamp.to_string());
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("session.json"), serde_json::to_vec_pretty(session)?)?;
    std::fs::write(dir.join("trials.json"), serde_json::to_vec_pretty(trials)?)?;
    let mut csv = String::from("trial,t,dx,dy,device,buttons,batched\n");
    for (i, s) in raw {
        let _ = writeln!(csv, "{i},{:.7},{},{},{},{},{}", s.t, s.dx, s.dy, s.device, s.buttons, s.batched as u8);
    }
    std::fs::write(dir.join("raw.csv"), csv)?;
    Ok(dir)
}

pub fn meta() -> Value {
    json!({
        "app_version": env!("CARGO_PKG_VERSION"),
        "analysis_version": rspec_core::ANALYSIS_VERSION,
    })
}
