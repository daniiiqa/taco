//! Memorias USB y discos que se pueden quitar: montar, desmontar y expulsar con
//! udisks (sin sudo), como el plugin mount que usaba yazi.

use serde_json::Value;
use std::process::{Command, Stdio};

pub struct Drive {
    pub dev: String,
    /// el disco entero (para expulsar)
    pub disk: String,
    pub label: String,
    pub size: String,
    pub mount: Option<String>,
}

pub fn list() -> Vec<Drive> {
    let Ok(out) = Command::new("lsblk")
        .args(["-J", "-p", "-o", "PATH,LABEL,SIZE,MOUNTPOINT,RM,HOTPLUG,TYPE,FSTYPE,PKNAME"])
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let mut drives = Vec::new();
    fn walk(devs: &Value, removable: bool, drives: &mut Vec<Drive>) {
        for d in devs.as_array().into_iter().flatten() {
            let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
            let rm = removable || d["rm"].as_bool().unwrap_or(false) || d["hotplug"].as_bool().unwrap_or(false);
            if rm && !s("fstype").is_empty() && s("type") != "loop" {
                let dev = s("path");
                drives.push(Drive {
                    disk: if s("pkname").is_empty() { dev.clone() } else { s("pkname") },
                    label: if s("label").is_empty() { dev.rsplit('/').next().unwrap_or("").to_string() } else { s("label") },
                    size: s("size"),
                    mount: d["mountpoint"].as_str().map(String::from),
                    dev,
                });
            }
            walk(&d["children"], rm, drives);
        }
    }
    walk(&v["blockdevices"], false, &mut drives);
    drives
}

fn udisks(args: &[&str]) -> Result<String, String> {
    let out = Command::new("udisksctl")
        .args(args)
        .arg("--no-user-interaction")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| crate::l("Falta udisks2 (sudo pacman -S udisks2)", "udisks2 is missing (sudo pacman -S udisks2)").to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("udisksctl").to_string())
    }
}

/// Monta y regresa dónde quedó ("Mounted /dev/sdb1 at /run/media/dany/USB").
pub fn mount(d: &Drive) -> Result<String, String> {
    if let Some(m) = &d.mount {
        return Ok(m.clone());
    }
    let out = udisks(&["mount", "-b", &d.dev])?;
    out.split(" at ").nth(1).map(|s| s.trim_end_matches('.').to_string()).ok_or(out)
}

pub fn unmount(d: &Drive) -> Result<(), String> {
    udisks(&["unmount", "-b", &d.dev]).map(|_| ())
}

/// Desmonta todo lo del disco y lo apaga (ya se puede sacar).
pub fn eject(d: &Drive, all: &[Drive]) -> Result<(), String> {
    for p in all.iter().filter(|p| p.disk == d.disk && p.mount.is_some()) {
        unmount(p)?;
    }
    udisks(&["power-off", "-b", &d.disk]).map(|_| ())
}
