//! Lo que hay en una carpeta, cómo se ordena, y las operaciones con archivos
//! (copiar, mover, papelera, borrar), que corren en otro hilo y avisan cómo van.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub link: bool,
    /// enlace que apunta a nada
    pub broken: bool,
    pub size: u64,
    pub mtime: SystemTime,
    pub mode: u32,
}

impl Entry {
    pub fn hidden(&self) -> bool {
        self.name.starts_with('.')
    }
    pub fn exec(&self) -> bool {
        !self.dir && self.mode & 0o111 != 0
    }
    pub fn ext(&self) -> String {
        if self.dir {
            return String::new();
        }
        Path::new(&self.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
    }
}

pub fn entry(path: PathBuf) -> Option<Entry> {
    let lmeta = fs::symlink_metadata(&path).ok()?;
    let link = lmeta.file_type().is_symlink();
    let (meta, broken) = if link {
        match fs::metadata(&path) {
            Ok(m) => (m, false),
            Err(_) => (lmeta, true),
        }
    } else {
        (lmeta, false)
    };
    Some(Entry {
        name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into()),
        dir: meta.is_dir(),
        link,
        broken,
        size: meta.len(),
        mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        mode: meta.permissions().mode(),
        path,
    })
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sort {
    Name,
    Time,
    Size,
    Ext,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Name => Sort::Time,
            Sort::Time => Sort::Size,
            Sort::Size => Sort::Ext,
            Sort::Ext => Sort::Name,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => crate::l("nombre", "name"),
            Sort::Time => crate::l("fecha", "date"),
            Sort::Size => crate::l("tamaño", "size"),
            Sort::Ext => crate::l("tipo", "type"),
        }
    }
}

/// "archivo10" después de "archivo9", sin importar mayúsculas
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                let mut nb = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
}

pub fn list(dir: &Path, hidden: bool, sort: Sort, rev: bool) -> io::Result<Vec<Entry>> {
    let mut v: Vec<Entry> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| entry(e.path()))
        .filter(|e| hidden || !e.hidden())
        .collect();
    v.sort_by(|a, b| {
        // carpetas primero, siempre
        b.dir.cmp(&a.dir).then_with(|| {
            let o = match sort {
                Sort::Name => natural(&a.name, &b.name),
                Sort::Time => b.mtime.cmp(&a.mtime),
                Sort::Size => b.size.cmp(&a.size),
                Sort::Ext => a.ext().cmp(&b.ext()).then_with(|| natural(&a.name, &b.name)),
            };
            if rev { o.reverse() } else { o }
        })
    });
    Ok(v)
}

pub fn human(n: u64) -> String {
    const U: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut x = n as f64;
    let mut i = 0;
    while x >= 1024.0 && i < U.len() - 1 {
        x /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}B") } else if x < 10.0 { format!("{x:.1}{}", U[i]) } else { format!("{x:.0}{}", U[i]) }
}

pub fn perms(e: &Entry) -> String {
    let t = if e.link { 'l' } else if e.dir { 'd' } else { '-' };
    let mut s = String::from(t);
    for shift in [6, 3, 0] {
        let m = (e.mode >> shift) & 7;
        s.push(if m & 4 != 0 { 'r' } else { '-' });
        s.push(if m & 2 != 0 { 'w' } else { '-' });
        s.push(if m & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// Fecha local "2026-10-27 14:05" (sin depender de chrono: localtime_r de libc)
pub fn date(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    unsafe extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    #[repr(C)]
    #[derive(Default)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: i64,
        zone: usize,
    }
    let mut tm = Tm::default();
    unsafe { localtime_r(&secs, &mut tm) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.year + 1900, tm.mon + 1, tm.mday, tm.hour, tm.min)
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

/// /home/dany/Música → ~/Música
pub fn tilde(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// ~ para rutas de texto (los puntos de montaje)
pub fn tilde_str(s: &str) -> String {
    tilde(Path::new(s))
}

/// "foto.png" ya existe en dir → "foto_1.png", "foto_2.png"…
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if fs::symlink_metadata(&p).is_err() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|i| dir.join(format!("{stem}_{i}{ext}")))
        .find(|p| fs::symlink_metadata(p).is_err())
        .unwrap()
}

// ── trabajos en otro hilo ────────────────────────────────────────────────────

pub enum Job {
    Copy(Vec<PathBuf>, PathBuf),
    Move(Vec<PathBuf>, PathBuf),
    Trash(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

pub enum Progress {
    /// (hechos, total, el que va)
    Step(usize, usize, String),
    Done(String),
    Failed(String),
}

pub fn worker() -> (Sender<Job>, Receiver<Progress>) {
    let (jtx, jrx) = channel::<Job>();
    let (ptx, prx) = channel();
    std::thread::spawn(move || {
        for job in jrx {
            let r = run(job, &ptx);
            let _ = ptx.send(match r {
                Ok(msg) => Progress::Done(msg),
                Err(e) => Progress::Failed(e),
            });
        }
    });
    (jtx, prx)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn run(job: Job, tx: &Sender<Progress>) -> Result<String, String> {
    let es = crate::spanish();
    let count = |n: usize| if n == 1 { (if es { "1 archivo" } else { "1 item" }).to_string() } else { format!("{n} {}", if es { "archivos" } else { "items" }) };
    match job {
        Job::Copy(srcs, dst) | Job::Move(srcs, dst) if srcs.iter().any(|s| dst.starts_with(s)) => {
            Err(if es { "No se puede meter una carpeta dentro de sí misma".into() } else { "Can't put a folder inside itself".into() })
        }
        Job::Copy(srcs, dst) => {
            let n = srcs.len();
            for (i, s) in srcs.iter().enumerate() {
                let _ = tx.send(Progress::Step(i, n, name_of(s)));
                let to = free_name(&dst, &name_of(s));
                copy_all(s, &to).map_err(|e| format!("{}: {e}", name_of(s)))?;
            }
            Ok(format!("{} {}", if es { "Copiados:" } else { "Copied:" }, count(n)))
        }
        Job::Move(srcs, dst) => {
            let n = srcs.len();
            for (i, s) in srcs.iter().enumerate() {
                let _ = tx.send(Progress::Step(i, n, name_of(s)));
                if s.parent() == Some(dst.as_path()) {
                    continue;
                }
                let to = free_name(&dst, &name_of(s));
                if fs::rename(s, &to).is_err() {
                    // otro disco: copiar y borrar
                    copy_all(s, &to).map_err(|e| format!("{}: {e}", name_of(s)))?;
                    remove_all(s).map_err(|e| format!("{}: {e}", name_of(s)))?;
                }
            }
            Ok(format!("{} {}", if es { "Movidos:" } else { "Moved:" }, count(n)))
        }
        Job::Trash(srcs) => {
            let n = srcs.len();
            for (i, s) in srcs.iter().enumerate() {
                let _ = tx.send(Progress::Step(i, n, name_of(s)));
                trash(s).map_err(|e| format!("{}: {e}", name_of(s)))?;
            }
            Ok(format!("{} {}", if es { "A la papelera:" } else { "Trashed:" }, count(n)))
        }
        Job::Delete(srcs) => {
            let n = srcs.len();
            for (i, s) in srcs.iter().enumerate() {
                let _ = tx.send(Progress::Step(i, n, name_of(s)));
                remove_all(s).map_err(|e| format!("{}: {e}", name_of(s)))?;
            }
            Ok(format!("{} {}", if es { "Borrados:" } else { "Deleted:" }, count(n)))
        }
    }
}

fn copy_all(from: &Path, to: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(from)?;
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if m.is_dir() {
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_all(&e.path(), &to.join(e.file_name()))?;
        }
        fs::set_permissions(to, m.permissions())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

fn remove_all(p: &Path) -> io::Result<()> {
    if fs::symlink_metadata(p)?.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }
}

/// La papelera de freedesktop (la misma que ven GTK, Dolphin y yazi). Con gio
/// sirve para cualquier disco; sin gio, la de ~/.local/share/Trash.
fn trash(p: &Path) -> io::Result<()> {
    if let Ok(st) = std::process::Command::new("gio").arg("trash").arg(p).stderr(std::process::Stdio::piped()).output() {
        if st.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&st.stderr).trim().to_string();
        if !err.is_empty() && fs::symlink_metadata(p).is_ok() && !same_disk_as_home(p) {
            return Err(io::Error::other(err));
        }
    }
    let base = std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| home().join(".local/share")).join("Trash");
    fs::create_dir_all(base.join("files"))?;
    fs::create_dir_all(base.join("info"))?;
    let to = free_name(&base.join("files"), &name_of(p));
    let tname = name_of(&to);
    let abs = fs::canonicalize(p.parent().unwrap_or(Path::new("/")))?.join(name_of(p));
    let now = date(SystemTime::now()).replace(' ', "T") + ":00";
    fs::write(base.join("info").join(format!("{tname}.trashinfo")), format!("[Trash Info]\nPath={}\nDeletionDate={now}\n", encode(&abs)))?;
    fs::rename(p, &to)
}

fn same_disk_as_home(p: &Path) -> bool {
    match (fs::symlink_metadata(p), fs::metadata(home())) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// %-encoding de la ruta para el .trashinfo
fn encode(p: &Path) -> String {
    let mut s = String::new();
    for b in p.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(*b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}
