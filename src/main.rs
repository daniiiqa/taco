//! taco — tus archivos bien envueltos: el explorador de archivos de discada,
//! en la terminal, con el look del rice (como rola y vato).
//!
//!   taco [carpeta|archivo] [--cwd-file RUTA] [--choose RUTA]
//!
//! `--choose`: para elegir archivos desde otra app (picha): enter sobre un archivo
//! (o lo marcado) escribe sus rutas en RUTA, una por renglón, y sale; q sale sin
//! elegir.
//!
//! Tres columnas: la carpeta de arriba, la de ahora y la vista previa. Las
//! teclas son las de yazi/ranger (h j k l, espacio marca, y copia, x corta,
//! p pega, d a la papelera); `?` las enseña todas. El ratón, como en vato:
//! clic elige, doble clic abre, clic derecho marca, la rueda va donde esté el
//! puntero y las teclas de abajo se pueden picar.

mod files;
mod icons;
mod look;
mod mounts;
mod preview;

use files::{Entry, Job, Progress, Sort};
use look::{Look, Select};
use preview::{Kind, Preview, Request};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};
use unicode_width::UnicodeWidthStr;

/// Idioma de discada (hypr/discada.conf → LANGUAGE=es|en); por defecto inglés.
pub fn spanish() -> bool {
    static ES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ES.get_or_init(|| {
        let f = files::home().join(".config/hypr/discada.conf");
        std::fs::read_to_string(f)
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("LANGUAGE=").map(|v| v.trim_start().starts_with("es"))))
            .unwrap_or(false)
    })
}

/// es / en
pub fn l(es: &'static str, en: &'static str) -> &'static str {
    if spanish() { es } else { en }
}

enum Ask {
    Rename(PathBuf),
    Create,
    Filter,
    OpenWith(Vec<PathBuf>),
}

enum Sure {
    Trash(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

enum Mode {
    Normal,
    /// después de `g`: a dónde ir
    Goto,
    Input { ask: Ask, buf: String, cur: usize },
    Confirm { sure: Sure, msg: String },
    Mounts { drives: Vec<mounts::Drive>, sel: usize },
    Help,
}

/// Lo que necesita la terminal entera (se suspende taco mientras).
enum Run {
    Edit(Vec<PathBuf>),
    Shell,
    Zoxide,
    Find,
}

/// Lo que hay bajo un clic fuera de las columnas.
#[derive(Clone, Copy)]
enum Hit {
    Key(KeyCode),
    Row(usize),
}

struct App {
    look: Look,
    cwd: PathBuf,
    all: Vec<Entry>,
    /// índices de `all` que pasan el filtro
    view: Vec<usize>,
    cur: usize,
    off: usize,
    parent: Vec<Entry>,
    parent_sel: Option<usize>,
    hidden: bool,
    sort: Sort,
    rev: bool,
    filter: String,
    marked: BTreeSet<PathBuf>,
    /// (rutas, cortar)
    clip: Option<(Vec<PathBuf>, bool)>,
    mode: Mode,
    /// en qué archivo iba el cursor en cada carpeta
    remember: HashMap<PathBuf, String>,
    prev_dir: Option<PathBuf>,
    status: Option<(String, Instant, bool)>,
    /// copiando/moviendo: (hechos, total, el que va)
    job: Option<(usize, usize, String)>,
    jobs: Sender<Job>,
    progress: Receiver<Progress>,
    pv_tx: Sender<Request>,
    pv_rx: Receiver<Preview>,
    preview: Option<Preview>,
    pv_want: Option<PathBuf>,
    pv_scroll: usize,
    dir_mtime: Option<SystemTime>,
    /// las tres columnas por dentro (arriba, ahora, vista previa)
    cols: [Rect; 3],
    hits: Vec<(Rect, Hit)>,
    last_click: Option<(Instant, u16, u16)>,
    images: bool,
    img_placed: Option<(PathBuf, Rect)>,
    img_want: Option<(PathBuf, Rect)>,
    img_since: Instant,
    run: Option<Run>,
    quit: bool,
    cwd_file: Option<PathBuf>,
    /// `--choose`: enter elige en vez de abrir, y escribe las rutas aquí.
    choose: Option<PathBuf>,
}

fn xdg_dir(key: &str, fallback: &str) -> PathBuf {
    let home = files::home();
    std::fs::read_to_string(home.join(".config/user-dirs.dirs"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let v = l.strip_prefix(key)?.strip_prefix('=')?.trim().trim_matches('"');
                Some(PathBuf::from(v.replace("$HOME", &home.to_string_lossy())))
            })
        })
        .unwrap_or_else(|| home.join(fallback))
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// Abrir sin atar el programa a taco (sigue vivo si se cierra la terminal).
fn detach(cmd: &mut Command) {
    let _ = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn();
}

fn trunc(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut n = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if n + cw + 1 > w {
            break;
        }
        out.push(c);
        n += cw;
    }
    out.push('…');
    out
}

impl App {
    fn new(start: PathBuf, select: Option<String>, cwd_file: Option<PathBuf>) -> App {
        let (jobs, progress) = files::worker();
        let (pv_tx, pv_rx) = preview::worker();
        let mut a = App {
            look: Look::load(),
            cwd: start.clone(),
            all: Vec::new(),
            view: Vec::new(),
            cur: 0,
            off: 0,
            parent: Vec::new(),
            parent_sel: None,
            hidden: true,
            sort: Sort::Name,
            rev: false,
            filter: String::new(),
            marked: BTreeSet::new(),
            clip: None,
            mode: Mode::Normal,
            remember: HashMap::new(),
            prev_dir: None,
            status: None,
            job: None,
            jobs,
            progress,
            pv_tx,
            pv_rx,
            preview: None,
            pv_want: None,
            pv_scroll: 0,
            dir_mtime: None,
            cols: [Rect::default(); 3],
            hits: Vec::new(),
            last_click: None,
            images: std::env::var_os("KITTY_WINDOW_ID").is_some() && have("kitten"),
            img_placed: None,
            img_want: None,
            img_since: Instant::now(),
            run: None,
            quit: false,
            cwd_file,
            choose: None,
        };
        if !a.load(&start, select) {
            a.load(&files::home(), None);
        }
        a
    }

    fn say(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), false));
    }
    fn warn(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), true));
    }

    fn current(&self) -> Option<&Entry> {
        self.view.get(self.cur).map(|&i| &self.all[i])
    }

    /// Lo marcado o, si no hay nada, lo que está bajo el cursor.
    fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() { self.current().map(|e| vec![e.path.clone()]).unwrap_or_default() } else { self.marked.iter().cloned().collect() }
    }

    fn refilter(&mut self) {
        let f = self.filter.to_lowercase();
        self.view = (0..self.all.len()).filter(|&i| f.is_empty() || self.all[i].name.to_lowercase().contains(&f)).collect();
        self.cur = self.cur.min(self.view.len().saturating_sub(1));
    }

    fn select_name(&mut self, name: &str) {
        if let Some(p) = self.view.iter().position(|&i| self.all[i].name == name) {
            self.cur = p;
        }
    }

    /// Entrar a una carpeta (o releer la de ahora). false si no se pudo.
    fn load(&mut self, dir: &Path, select: Option<String>) -> bool {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let list = match files::list(&dir, self.hidden, self.sort, self.rev) {
            Ok(v) => v,
            Err(e) => {
                self.warn(format!("{}: {e}", files::tilde(&dir)));
                return false;
            }
        };
        let same = dir == self.cwd && !self.all.is_empty();
        if let Some(e) = self.current() {
            let name = e.name.clone();
            self.remember.insert(self.cwd.clone(), name);
        }
        let keep = if same { self.current().map(|e| e.name.clone()) } else { None };
        if !same {
            if self.cwd != dir {
                self.prev_dir = Some(self.cwd.clone());
            }
            self.filter.clear();
            self.off = 0;
            if have("zoxide") {
                detach(Command::new("zoxide").arg("add").arg(&dir));
            }
        }
        self.cwd = dir.clone();
        self.all = list;
        self.cur = 0;
        self.refilter();
        if let Some(n) = select.or(keep).or_else(|| self.remember.get(&dir).cloned()) {
            self.select_name(&n);
        }
        self.marked.retain(|p| p.exists());
        self.dir_mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        self.parent.clear();
        self.parent_sel = None;
        if let Some(up) = dir.parent() {
            if let Ok(v) = files::list(up, self.hidden, self.sort, self.rev) {
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.parent_sel = v.iter().position(|e| e.name == name);
                self.parent = v;
            }
        }
        self.pv_want = None;
        true
    }

    fn reload(&mut self) {
        let cwd = self.cwd.clone();
        self.load(&cwd, None);
    }

    fn ask_preview(&mut self) {
        let Some(e) = self.current().cloned() else {
            self.preview = None;
            self.pv_want = None;
            return;
        };
        if self.pv_want.as_ref() == Some(&e.path) {
            return;
        }
        self.pv_want = Some(e.path.clone());
        self.pv_scroll = 0;
        let _ = self.pv_tx.send(Request { entry: e, hidden: self.hidden, sort: self.sort, rev: self.rev, images: self.images });
    }

    fn go_up(&mut self) {
        if let Some(up) = self.cwd.parent().map(Path::to_path_buf) {
            let name = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            self.load(&up, name);
        }
    }

    fn open_current(&mut self) {
        let Some(e) = self.current().cloned() else { return };
        if e.dir {
            self.load(&e.path, None);
            return;
        }
        if let Some(f) = &self.choose {
            let paths: Vec<String> = self.targets().iter().map(|p| p.to_string_lossy().into_owned()).collect();
            let _ = std::fs::write(f, paths.join("\n") + "\n");
            if let Some(c) = &self.cwd_file {
                let _ = std::fs::write(c, self.cwd.as_os_str().as_encoded_bytes());
            }
            self.quit = true;
            return;
        }
        if self.marked.len() > 1 {
            let t = self.targets();
            self.open(t);
        } else {
            self.open(vec![e.path]);
        }
    }

    /// Texto al editor (aquí mismo); lo demás con la app que diga xdg-open.
    fn open(&mut self, paths: Vec<PathBuf>) {
        let text = |p: &PathBuf| {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            p.is_file() && (preview::is_text(&ext) || preview::text(p).is_some())
        };
        if paths.iter().all(text) {
            self.run = Some(Run::Edit(paths));
            return;
        }
        for p in &paths {
            detach(Command::new("xdg-open").arg(p));
        }
        let name = paths.first().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.say(format!("{} {name}", l("Abriendo", "Opening")));
    }

    fn move_by(&mut self, d: isize) {
        let n = self.view.len();
        if n == 0 {
            return;
        }
        self.cur = (self.cur as isize + d).clamp(0, n as isize - 1) as usize;
    }

    fn page(&self) -> isize {
        (self.cols[1].height as isize / 2).max(1)
    }

    fn submit(&mut self, ask: Ask, buf: String) {
        match ask {
            Ask::Filter => {}
            Ask::Create => {
                let name = buf.trim();
                if name.is_empty() {
                    return;
                }
                let p = self.cwd.join(name.trim_end_matches('/'));
                let r = if name.ends_with('/') {
                    std::fs::create_dir_all(&p)
                } else if p.exists() {
                    Err(std::io::Error::other(l("ya existe", "already exists")))
                } else {
                    p.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::File::create(&p).map(|_| ()))
                };
                match r {
                    Ok(()) => {
                        let first = Path::new(name.trim_end_matches('/')).components().next().map(|c| c.as_os_str().to_string_lossy().into_owned());
                        self.reload();
                        if let Some(f) = first {
                            self.select_name(&f);
                        }
                    }
                    Err(e) => self.warn(format!("{name}: {e}")),
                }
            }
            Ask::Rename(from) => {
                let name = buf.trim();
                if name.is_empty() || name.contains('/') {
                    return;
                }
                let to = from.with_file_name(name);
                if to == from {
                    return;
                }
                if to.exists() {
                    self.warn(format!("{name}: {}", l("ya existe", "already exists")));
                    return;
                }
                match std::fs::rename(&from, &to) {
                    Ok(()) => {
                        self.reload();
                        self.select_name(name);
                    }
                    Err(e) => self.warn(format!("{name}: {e}")),
                }
            }
            Ask::OpenWith(paths) => {
                let cmd = buf.trim();
                if cmd.is_empty() {
                    return;
                }
                detach(Command::new("sh").arg("-c").arg(format!("{cmd} \"$@\"")).arg("sh").args(&paths).current_dir(&self.cwd));
                self.say(format!("{} {cmd}", l("Abriendo con", "Opening with")));
            }
        }
    }

    fn paste(&mut self) {
        let Some((paths, cut)) = self.clip.clone() else {
            self.warn(l("No hay nada copiado (y copia, x corta)", "Nothing copied (y copies, x cuts)"));
            return;
        };
        let dst = self.cwd.clone();
        let _ = self.jobs.send(if cut { Job::Move(paths, dst) } else { Job::Copy(paths, dst) });
        if cut {
            self.clip = None;
        }
        self.job = Some((0, 0, l("pegando…", "pasting…").into()));
    }

    /// Lo marcado (o lo de abajo del cursor) al cel, con picha, en su propia ventana.
    fn send_to_phone(&mut self) {
        let files: Vec<PathBuf> = self.targets().into_iter().filter(|p| p.is_file()).collect();
        if files.is_empty() {
            return self.warn(l("Elige archivos (no carpetas) para el cel", "Pick files (not folders) for the phone"));
        }
        let sent = Command::new("setsid")
            .arg("-f")
            .args(["kitty", "--class", "picha", "-e", "picha"])
            .args(&files)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if sent {
            let n = files.len();
            self.marked.clear();
            self.say(format!("{} {n} {}", l("Al cel:", "To the phone:"), if n == 1 { l("archivo", "file") } else { l("archivos", "files") }));
        } else {
            self.warn(l("Falta picha", "picha is missing"));
        }
    }

    fn copy_to_clipboard(&mut self, text: String) {
        let child = Command::new("wl-copy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        match child {
            Ok(mut c) => {
                if let Some(mut i) = c.stdin.take() {
                    let _ = i.write_all(text.as_bytes());
                }
                let _ = c.wait();
                self.say(format!("{} {}", l("Copiado:", "Copied:"), trunc(&text, 60)));
            }
            Err(_) => self.warn(l("Falta wl-clipboard", "wl-clipboard is missing")),
        }
    }

    fn goto(&mut self, c: char) {
        let home = files::home();
        let dir = match c {
            'h' | '~' => home.clone(),
            'd' => xdg_dir("XDG_DOWNLOAD_DIR", "Downloads"),
            'D' => xdg_dir("XDG_DOCUMENTS_DIR", "Documents"),
            'p' => xdg_dir("XDG_PICTURES_DIR", "Pictures"),
            'v' => xdg_dir("XDG_VIDEOS_DIR", "Videos"),
            'm' => xdg_dir("XDG_MUSIC_DIR", "Music"),
            'c' => home.join(".config"),
            't' => std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| home.join(".local/share")).join("Trash/files"),
            'r' | '/' => PathBuf::from("/"),
            'u' => PathBuf::from("/run/media").join(std::env::var("USER").unwrap_or_default()),
            'g' => {
                self.cur = 0;
                return;
            }
            _ => return,
        };
        self.load(&dir, None);
    }

    fn open_mounts(&mut self) {
        let drives = mounts::list();
        if drives.is_empty() {
            self.say(l("No hay memorias ni discos externos conectados", "No USB drives or external disks plugged in"));
            return;
        }
        self.mode = Mode::Mounts { drives, sel: 0 };
    }

    // ── teclas ───────────────────────────────────────────────────────────────
    fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.key_normal(k, ctrl),
            Mode::Goto => {
                if let KeyCode::Char(c) = k.code {
                    self.goto(c);
                }
            }
            Mode::Help => {}
            Mode::Confirm { sure, msg } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('s') | KeyCode::Enter => {
                    let _ = self.jobs.send(match sure {
                        Sure::Trash(p) => Job::Trash(p),
                        Sure::Delete(p) => Job::Delete(p),
                    });
                    self.marked.clear();
                    self.job = Some((0, 0, "…".into()));
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {}
                _ => self.mode = Mode::Confirm { sure, msg },
            },
            Mode::Mounts { drives, mut sel } => {
                let n = drives.len();
                match k.code {
                    KeyCode::Char('j') | KeyCode::Down => sel = (sel + 1).min(n - 1),
                    KeyCode::Char('k') | KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('m') => {
                        match mounts::mount(&drives[sel]) {
                            Ok(at) => {
                                self.load(Path::new(&at), None);
                            }
                            Err(e) => self.warn(e),
                        }
                        return;
                    }
                    KeyCode::Char('u') => {
                        if self.cwd.starts_with(drives[sel].mount.as_deref().unwrap_or("\0")) {
                            self.load(&files::home(), None);
                        }
                        match mounts::unmount(&drives[sel]) {
                            Ok(()) => self.say(format!("{} {}", l("Desmontado:", "Unmounted:"), drives[sel].label)),
                            Err(e) => self.warn(e),
                        }
                        self.mode = Mode::Mounts { drives: mounts::list(), sel };
                        self.clamp_mounts();
                        return;
                    }
                    KeyCode::Char('e') => {
                        if drives.iter().any(|d| d.disk == drives[sel].disk && d.mount.as_deref().is_some_and(|m| self.cwd.starts_with(m))) {
                            self.load(&files::home(), None);
                        }
                        match mounts::eject(&drives[sel], &drives) {
                            Ok(()) => self.say(format!("{} {}", drives[sel].label, l("ya se puede sacar", "is safe to remove"))),
                            Err(e) => self.warn(e),
                        }
                        let left = mounts::list();
                        if !left.is_empty() {
                            self.mode = Mode::Mounts { drives: left, sel };
                            self.clamp_mounts();
                        }
                        return;
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('M') => return,
                    _ => {}
                }
                self.mode = Mode::Mounts { drives, sel };
            }
            Mode::Input { ask, mut buf, mut cur } => {
                let at = |s: &str, c: usize| s.char_indices().nth(c).map(|(i, _)| i).unwrap_or(s.len());
                match k.code {
                    KeyCode::Esc => {
                        if matches!(ask, Ask::Filter) {
                            self.filter.clear();
                            self.refilter();
                        }
                        return;
                    }
                    KeyCode::Enter => {
                        self.submit(ask, buf);
                        return;
                    }
                    KeyCode::Char('u') if ctrl => {
                        buf.clear();
                        cur = 0;
                    }
                    KeyCode::Char('w') if ctrl => {
                        let before: String = buf.chars().take(cur).collect();
                        let keep = before.trim_end().rfind(' ').map(|i| i + 1).unwrap_or(0);
                        let after: String = buf.chars().skip(cur).collect();
                        buf = format!("{}{after}", &before[..keep]);
                        cur = before[..keep].chars().count();
                    }
                    KeyCode::Char(c) => {
                        let i = at(&buf, cur);
                        buf.insert(i, c);
                        cur += 1;
                    }
                    KeyCode::Backspace if cur > 0 => {
                        let i = at(&buf, cur - 1);
                        buf.remove(i);
                        cur -= 1;
                    }
                    KeyCode::Delete if cur < buf.chars().count() => {
                        let i = at(&buf, cur);
                        buf.remove(i);
                    }
                    KeyCode::Left => cur = cur.saturating_sub(1),
                    KeyCode::Right => cur = (cur + 1).min(buf.chars().count()),
                    KeyCode::Home => cur = 0,
                    KeyCode::End => cur = buf.chars().count(),
                    _ => {}
                }
                if matches!(ask, Ask::Filter) {
                    self.filter = buf.clone();
                    self.refilter();
                }
                self.mode = Mode::Input { ask, buf, cur };
            }
        }
    }

    fn clamp_mounts(&mut self) {
        if let Mode::Mounts { drives, sel } = &mut self.mode {
            if drives.is_empty() {
                self.mode = Mode::Normal;
            } else {
                *sel = (*sel).min(drives.len() - 1);
            }
        }
    }

    fn key_normal(&mut self, k: KeyEvent, ctrl: bool) {
        match k.code {
            KeyCode::Char('q') => {
                if let Some(f) = &self.cwd_file {
                    let _ = std::fs::write(f, self.cwd.as_os_str().as_encoded_bytes());
                }
                self.quit = true;
            }
            KeyCode::Char('Q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('d') if ctrl => self.move_by(self.page()),
            KeyCode::Char('u') if ctrl => self.move_by(-self.page()),
            KeyCode::Char('a') if ctrl => {
                for &i in &self.view {
                    self.marked.insert(self.all[i].path.clone());
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(self.page() * 2),
            KeyCode::PageUp => self.move_by(-self.page() * 2),
            KeyCode::Char('G') | KeyCode::End => self.cur = self.view.len().saturating_sub(1),
            KeyCode::Home => self.cur = 0,
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.go_up(),
            KeyCode::Char('l') | KeyCode::Right => {
                if self.current().is_some_and(|e| e.dir) {
                    self.open_current();
                }
            }
            KeyCode::Enter => self.open_current(),
            KeyCode::Char('g') => self.mode = Mode::Goto,
            KeyCode::Char('~') => self.goto('h'),
            KeyCode::Char('-') => {
                if let Some(p) = self.prev_dir.clone() {
                    self.load(&p, None);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(p) = self.current().map(|e| e.path.clone()) {
                    if !self.marked.remove(&p) {
                        self.marked.insert(p);
                    }
                    self.move_by(1);
                }
            }
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.refilter();
                } else {
                    self.marked.clear();
                }
            }
            KeyCode::Char('y') | KeyCode::Char('x') => {
                let t = self.targets();
                if t.is_empty() {
                    return;
                }
                let cut = k.code == KeyCode::Char('x');
                let n = t.len();
                self.clip = Some((t, cut));
                self.marked.clear();
                self.say(format!(
                    "{} {n} · {}",
                    if cut { l("Cortados:", "Cut:") } else { l("Copiados:", "Copied:") },
                    l("p pega en la carpeta donde estés", "p pastes into the folder you're in")
                ));
            }
            KeyCode::Char('p') => self.paste(),
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Char('D') => {
                let t = self.targets();
                if t.is_empty() {
                    return;
                }
                let forever = k.code == KeyCode::Char('D');
                let what = if t.len() == 1 {
                    t[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    format!("{} {}", t.len(), l("archivos", "items"))
                };
                let msg = if forever {
                    format!("{} {what}? {}", l("¿Borrar para siempre", "Delete forever"), l("No se puede deshacer.", "This can't be undone."))
                } else {
                    format!("{} {what} {}", l("¿Mandar", "Move"), l("a la papelera?", "to the trash?"))
                };
                let sure = if forever { Sure::Delete(t) } else { Sure::Trash(t) };
                self.mode = Mode::Confirm { sure, msg };
            }
            KeyCode::Char('r') | KeyCode::F(2) => {
                if let Some(e) = self.current().cloned() {
                    // el cursor antes de la extensión, como en yazi
                    let cur = match e.name.rfind('.') {
                        Some(i) if i > 0 && !e.dir => e.name[..i].chars().count(),
                        _ => e.name.chars().count(),
                    };
                    self.mode = Mode::Input { ask: Ask::Rename(e.path), buf: e.name, cur };
                }
            }
            KeyCode::Char('a') => self.mode = Mode::Input { ask: Ask::Create, buf: String::new(), cur: 0 },
            KeyCode::Char('/') => {
                let buf = self.filter.clone();
                let cur = buf.chars().count();
                self.mode = Mode::Input { ask: Ask::Filter, buf, cur };
            }
            KeyCode::Char('o') => {
                let t = self.targets();
                if !t.is_empty() {
                    self.mode = Mode::Input { ask: Ask::OpenWith(t), buf: String::new(), cur: 0 };
                }
            }
            KeyCode::Char('e') => {
                let t: Vec<_> = self.targets().into_iter().filter(|p| !p.is_dir()).collect();
                if !t.is_empty() {
                    self.run = Some(Run::Edit(t));
                }
            }
            KeyCode::Char('!') | KeyCode::Char(':') => self.run = Some(Run::Shell),
            KeyCode::Char('z') => self.run = Some(Run::Zoxide),
            KeyCode::Char('f') => self.run = Some(Run::Find),
            KeyCode::Char('M') => self.open_mounts(),
            KeyCode::Char('.') => {
                self.hidden = !self.hidden;
                self.reload();
                self.say(if self.hidden { l("Ocultos: se ven", "Hidden: shown") } else { l("Ocultos: no se ven", "Hidden: not shown") });
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.reload();
            }
            KeyCode::Char('S') => {
                self.rev = !self.rev;
                self.reload();
            }
            KeyCode::Char('c') => {
                let t: Vec<String> = self.targets().iter().map(|p| p.display().to_string()).collect();
                if !t.is_empty() {
                    self.copy_to_clipboard(t.join("\n"));
                }
            }
            KeyCode::Char('C') => self.copy_to_clipboard(self.cwd.display().to_string()),
            KeyCode::Char('J') => self.pv_scroll += 5,
            KeyCode::Char('K') => self.pv_scroll = self.pv_scroll.saturating_sub(5),
            KeyCode::Char('P') => self.send_to_phone(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    // ── ratón ────────────────────────────────────────────────────────────────
    fn row_at(r: Rect, x: u16, y: u16) -> Option<usize> {
        r.contains(Position { x, y }).then(|| (y - r.y) as usize)
    }

    fn mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = matches!(m.kind, MouseEventKind::ScrollDown);
                if let Mode::Mounts { drives, sel } = &mut self.mode {
                    *sel = if down { (*sel + 1).min(drives.len() - 1) } else { sel.saturating_sub(1) };
                } else if self.cols[2].contains(Position { x, y }) {
                    self.pv_scroll = if down { self.pv_scroll + 3 } else { self.pv_scroll.saturating_sub(3) };
                } else if matches!(self.mode, Mode::Normal) {
                    self.move_by(if down { 3 } else { -3 });
                }
            }
            MouseEventKind::Down(b) => {
                let double = self.last_click.is_some_and(|(t, cx, cy)| t.elapsed() < Duration::from_millis(400) && cy == y && cx.abs_diff(x) < 3);
                self.last_click = Some((Instant::now(), x, y));
                let hit = self.hits.iter().find(|(r, _)| r.contains(Position { x, y })).map(|h| h.1);
                match (&mut self.mode, hit) {
                    (Mode::Mounts { sel, .. }, Some(Hit::Row(i))) => {
                        *sel = i;
                        if double {
                            self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                        }
                        return;
                    }
                    (_, Some(Hit::Key(code))) => {
                        self.key(KeyEvent::new(code, KeyModifiers::NONE));
                        return;
                    }
                    (Mode::Normal, _) => {}
                    // un clic fuera de una ventanita la cierra
                    (Mode::Help | Mode::Goto | Mode::Mounts { .. }, _) => {
                        self.mode = Mode::Normal;
                        return;
                    }
                    _ => return,
                }
                if let Some(r) = Self::row_at(self.cols[1], x, y) {
                    let i = self.off + r;
                    if i >= self.view.len() {
                        return;
                    }
                    self.cur = i;
                    match b {
                        MouseButton::Right => self.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
                        MouseButton::Left if double => self.open_current(),
                        _ => {}
                    }
                } else if let Some(r) = Self::row_at(self.cols[0], x, y) {
                    let off = self.parent_sel.map(|s| s.saturating_sub(self.cols[0].height as usize / 2)).unwrap_or(0);
                    let off = off.min(self.parent.len().saturating_sub(self.cols[0].height as usize));
                    if let Some(e) = self.parent.get(off + r).cloned() {
                        if e.dir {
                            self.load(&e.path, None);
                        } else if let Some(up) = e.path.parent() {
                            self.load(up, Some(e.name));
                        }
                    }
                } else if let Some(r) = Self::row_at(self.cols[2], x, y) {
                    let child = match &self.preview {
                        Some(Preview { kind: Kind::Dir(v), path }) if Some(path) == self.pv_want.as_ref() => v.get(self.pv_scroll + r).cloned(),
                        _ => None,
                    };
                    if let (Some(c), Some(dir)) = (child, self.current().map(|e| e.path.clone())) {
                        self.load(&dir, Some(c.name.clone()));
                        if double {
                            self.open_current();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ── dibujo ───────────────────────────────────────────────────────────────
    fn entry_style(&self, e: &Entry) -> Style {
        let t = self.look.theme;
        let cut = self.clip.as_ref().is_some_and(|(p, cut)| *cut && p.contains(&e.path));
        let s = if e.broken {
            Style::new().fg(t.red)
        } else if e.dir {
            Style::new().fg(t.primary).bold()
        } else if e.link {
            Style::new().fg(t.accent).italic()
        } else if e.exec() {
            Style::new().fg(t.green)
        } else if e.hidden() {
            Style::new().fg(t.muted)
        } else {
            Style::new().fg(t.fg)
        };
        if cut { s.add_modifier(Modifier::DIM | Modifier::CROSSED_OUT) } else { s }
    }

    /// Una fila de archivo: marca, icono, nombre y (si cabe) el tamaño.
    fn entry_line(&self, e: &Entry, w: usize, selected: bool, focused: bool, size: bool) -> Line<'static> {
        let t = self.look.theme;
        let marked = self.marked.contains(&e.path);
        let (sel_style, sym) = self.look.sel(focused);
        let mark = if marked { "+" } else { " " };
        let icon = if self.look.icons { format!("{} ", icons::icon(e)) } else { String::new() };
        let suffix = if self.look.icons {
            ""
        } else if e.dir {
            "/"
        } else if e.link {
            "@"
        } else if e.exec() {
            "*"
        } else {
            ""
        };
        let right = if size && !e.dir { files::human(e.size) } else { String::new() };
        let lead = if selected { sym } else { " " };
        let fixed = lead.width() + mark.width() + icon.width() + right.width() + 1;
        let name = trunc(&format!("{}{suffix}", e.name), w.saturating_sub(fixed));
        let pad = w.saturating_sub(fixed + name.width());
        let base = self.entry_style(e);
        let mut st = base;
        if selected {
            st = match self.look.select {
                Select::Bar => base.patch(Style::new().fg(t.primary).bold()),
                _ => sel_style,
            };
        }
        let fill = if selected && self.look.select != Select::Bar { sel_style } else { Style::new() };
        let lead_st = if selected { Style::new().fg(t.primary).bold().patch(fill) } else { Style::new() };
        Line::from(vec![
            Span::styled(lead.to_string(), lead_st),
            Span::styled(mark.to_string(), Style::new().fg(t.accent).bold().patch(fill)),
            Span::styled(icon, st),
            Span::styled(name, st),
            Span::styled(" ".repeat(pad), fill),
            Span::styled(right, if selected { st } else { Style::new().fg(t.muted) }),
            Span::styled(" ", fill),
        ])
    }

    fn draw(&mut self, f: &mut Frame) {
        let t = self.look.theme;
        if t.bg != Color::Reset {
            f.render_widget(Block::new().style(Style::new().bg(t.bg).fg(t.fg)), f.area());
        }
        self.hits.clear();
        let [head, body, info, foot] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1)]).areas(f.area());

        // arriba: taco y la ruta; a la derecha, lo copiado y lo marcado
        let badge = format!(" {} ", self.look.tx("taco"));
        let mut left = vec![Span::styled(badge, Style::new().fg(t.base).bg(t.primary).bold()), Span::raw("  ")];
        let path = files::tilde(&self.cwd);
        let (dir, last) = match path.rfind('/') {
            Some(i) => (path[..=i].to_string(), path[i + 1..].to_string()),
            None => (String::new(), path.clone()),
        };
        left.push(Span::styled(dir, Style::new().fg(t.muted)));
        left.push(Span::styled(last, Style::new().fg(t.fg).bold()));
        if !self.filter.is_empty() {
            left.push(Span::styled(format!("  /{}", self.filter), Style::new().fg(t.accent)));
        }
        f.render_widget(Paragraph::new(Line::from(left)), head);
        let mut right = Vec::new();
        if let Some((i, n, name)) = &self.job {
            if *n > 0 {
                let (done, rest) = self.look.bar(*i as f64 / *n as f64, 10);
                right.push(Span::styled(done, Style::new().fg(t.primary)));
                right.push(Span::styled(rest, Style::new().fg(t.border)));
                right.push(Span::styled(format!(" {}/{n} {}  ", i + 1, trunc(name, 24)), Style::new().fg(t.accent)));
            } else {
                right.push(Span::styled(format!("{name}  "), Style::new().fg(t.accent)));
            }
        }
        if !self.marked.is_empty() {
            right.push(Span::styled(format!("+{} {}  ", self.marked.len(), self.look.tx(l("marcados", "marked"))), Style::new().fg(t.accent)));
        }
        if self.choose.is_some() {
            right.push(Span::styled(format!("{}  ", self.look.tx(l("enter elige · espacio marca", "enter chooses · space marks"))), Style::new().fg(t.primary)));
        }
        if let Some((p, cut)) = &self.clip {
            let what = if *cut { l("cortados", "cut") } else { l("copiados", "copied") };
            right.push(Span::styled(format!("{} {}  ", p.len(), self.look.tx(what)), Style::new().fg(t.muted)));
        }
        f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), head);

        // las tres columnas (en una terminal angosta, menos)
        let ratios: &[u32] = if body.width >= 90 {
            &[1, 4, 3]
        } else if body.width >= 50 {
            &[0, 1, 1]
        } else {
            &[0, 1, 0]
        };
        let areas = Layout::horizontal(ratios.iter().map(|&r| Constraint::Fill(r as u16))).split(body);
        for (i, a) in areas.iter().enumerate() {
            let block = self.look.block(i == 1);
            self.cols[i] = if ratios[i] == 0 { Rect::default() } else { block.inner(*a) };
            if ratios[i] != 0 {
                f.render_widget(block, *a);
            }
        }

        // la de arriba
        let c = self.cols[0];
        if c.height > 0 {
            let h = c.height as usize;
            let off = self.parent_sel.map(|s| s.saturating_sub(h / 2)).unwrap_or(0).min(self.parent.len().saturating_sub(h));
            let lines: Vec<Line> = self.parent.iter().enumerate().skip(off).take(h).map(|(i, e)| self.entry_line(e, c.width as usize, Some(i) == self.parent_sel, false, false)).collect();
            f.render_widget(Paragraph::new(lines), c);
        }

        // la de ahora
        let c = self.cols[1];
        let h = c.height as usize;
        if h > 0 {
            let margin = (h / 4).min(3);
            if self.cur < self.off + margin {
                self.off = self.cur.saturating_sub(margin);
            } else if self.cur + margin >= self.off + h {
                self.off = (self.cur + margin + 1).saturating_sub(h);
            }
            self.off = self.off.min(self.view.len().saturating_sub(h));
            let lines: Vec<Line> = if self.view.is_empty() {
                let msg = if self.filter.is_empty() { l("vacía", "empty") } else { l("nada coincide", "no matches") };
                vec![Line::from(Span::styled(format!("  {}", self.look.tx(msg)), Style::new().fg(t.muted).italic()))]
            } else {
                self.view.iter().enumerate().skip(self.off).take(h).map(|(i, &ix)| self.entry_line(&self.all[ix], c.width as usize, i == self.cur, true, true)).collect()
            };
            f.render_widget(Paragraph::new(lines), c);
        }

        // la vista previa
        self.img_want = None;
        let c = self.cols[2];
        if c.height > 0 {
            let fresh = self.preview.as_ref().filter(|p| Some(&p.path) == self.pv_want.as_ref());
            let h = c.height as usize;
            let lines: Vec<Line> = match fresh.map(|p| &p.kind) {
                None => vec![Line::from(Span::styled(" …", Style::new().fg(t.muted)))],
                Some(Kind::Dir(v)) if v.is_empty() => vec![Line::from(Span::styled(format!("  {}", self.look.tx(l("vacía", "empty"))), Style::new().fg(t.muted).italic()))],
                Some(Kind::Dir(v)) => {
                    self.pv_scroll = self.pv_scroll.min(v.len().saturating_sub(1));
                    v.iter().skip(self.pv_scroll).take(h).map(|e| self.entry_line(e, c.width as usize, false, false, false)).collect()
                }
                Some(Kind::Text(v)) => {
                    self.pv_scroll = self.pv_scroll.min(v.len().saturating_sub(1));
                    v.iter().skip(self.pv_scroll).take(h).map(|s| Line::from(Span::styled(format!(" {}", trunc(s, c.width.saturating_sub(1) as usize)), Style::new().fg(t.fg)))).collect()
                }
                Some(Kind::Image(p)) => {
                    let p = p.clone();
                    self.img_want = Some((p, Rect { x: c.x + 1, y: c.y, width: c.width.saturating_sub(2), height: c.height }));
                    Vec::new()
                }
                Some(Kind::Info(v)) => {
                    let mut out = vec![Line::raw("")];
                    out.extend(v.iter().map(|s| Line::from(Span::styled(format!("  {}", trunc(s, c.width.saturating_sub(3) as usize)), Style::new().fg(t.muted)))));
                    out
                }
            };
            f.render_widget(Paragraph::new(lines), c);
        }

        // abajo: permisos, tamaño, fecha · orden y cuántos
        let mut v = Vec::new();
        if let Some(e) = self.current() {
            v.push(Span::styled(format!(" {}", files::perms(e)), Style::new().fg(t.muted)));
            if !e.dir {
                v.push(Span::styled(format!("  {}", files::human(e.size)), Style::new().fg(t.fg)));
            }
            v.push(Span::styled(format!("  {}", files::date(e.mtime)), Style::new().fg(t.muted)));
            if e.link {
                if let Ok(to) = std::fs::read_link(&e.path) {
                    v.push(Span::styled(format!("  → {}", files::tilde(&to)), Style::new().fg(t.accent)));
                }
            }
        }
        f.render_widget(Paragraph::new(Line::from(v)), info);
        let arrow = if self.rev { "↑" } else { "↓" };
        let pos = if self.view.is_empty() { "0/0".into() } else { format!("{}/{}", self.cur + 1, self.view.len()) };
        let r = format!("{} {} {arrow}  {pos} ", self.look.tx(l("orden:", "sort:")), self.look.tx(self.sort.label()));
        f.render_widget(Paragraph::new(Span::styled(r, Style::new().fg(t.muted))).alignment(Alignment::Right), info);

        // la última: un aviso reciente, o las teclas
        let line = match &self.status {
            Some((msg, at, bad)) if at.elapsed() < Duration::from_secs(5) => {
                Line::from(Span::styled(format!(" {msg}"), Style::new().fg(if *bad { t.red } else { t.accent })))
            }
            _ => {
                let hints: Vec<(&str, &str)> = match &self.mode {
                    Mode::Goto => vec![
                        ("h", "home"),
                        ("d", l("descargas", "downloads")),
                        ("D", l("documentos", "documents")),
                        ("p", l("imágenes", "pictures")),
                        ("v", l("videos", "videos")),
                        ("m", l("música", "music")),
                        ("c", ".config"),
                        ("t", l("papelera", "trash")),
                        ("u", "usb"),
                        ("r", "/"),
                    ],
                    Mode::Input { ask: Ask::Create, .. } => vec![("enter", l("crear", "create")), ("esc", l("cancelar", "cancel"))],
                    Mode::Input { .. } => vec![("enter", "ok"), ("esc", l("cancelar", "cancel"))],
                    Mode::Confirm { .. } => vec![("y", l("sí", "yes")), ("n", "no")],
                    Mode::Mounts { .. } => vec![("enter", l("montar y abrir", "mount and open")), ("u", l("desmontar", "unmount")), ("e", l("expulsar", "eject")), ("esc", l("cerrar", "close"))],
                    Mode::Help => vec![("esc", l("cerrar", "close"))],
                    Mode::Normal => vec![
                        ("enter", l("abrir", "open")),
                        ("space", l("marcar", "mark")),
                        ("y", l("copiar", "copy")),
                        ("x", l("cortar", "cut")),
                        ("p", l("pegar", "paste")),
                        ("d", l("papelera", "trash")),
                        ("r", l("renombrar", "rename")),
                        ("a", l("nuevo", "new")),
                        ("/", l("filtrar", "filter")),
                        ("z", l("saltar", "jump")),
                        ("g", l("ir a", "go to")),
                        ("M", "usb"),
                        ("?", l("ayuda", "help")),
                        ("q", l("salir", "quit")),
                    ],
                };
                let mut v = Vec::new();
                let mut x = foot.x;
                for (k, what) in hints {
                    let key = format!(" {k} ");
                    let what = format!("{}  ", self.look.tx(what));
                    let w = (key.width() + what.width()) as u16;
                    if x + w > foot.x + foot.width {
                        break;
                    }
                    let code = match k {
                        "enter" => Some(KeyCode::Enter),
                        "esc" => Some(KeyCode::Esc),
                        "space" => Some(KeyCode::Char(' ')),
                        _ if k.chars().count() == 1 => k.chars().next().map(KeyCode::Char),
                        _ => None,
                    };
                    if let Some(code) = code {
                        self.hits.push((Rect { x, y: foot.y, width: w.saturating_sub(1), height: 1 }, Hit::Key(code)));
                    }
                    x += w;
                    v.push(Span::styled(key, Style::new().fg(t.primary).bold()));
                    v.push(Span::styled(what, Style::new().fg(t.muted)));
                }
                Line::from(v)
            }
        };
        f.render_widget(Paragraph::new(line), foot);

        self.draw_popup(f);
    }

    fn popup(&self, f: &mut Frame, w: u16, h: u16, title: &str) -> Rect {
        let a = f.area();
        let w = w.min(a.width.saturating_sub(4));
        let h = h.min(a.height.saturating_sub(2));
        let r = Rect { x: a.x + (a.width - w) / 2, y: a.y + (a.height - h) / 2, width: w, height: h };
        let t = self.look.theme;
        let block = self.look.block(true).title(Span::styled(format!(" {} ", self.look.tx(title)), Style::new().fg(t.primary).bold()));
        let inner = block.inner(r);
        f.render_widget(Clear, r);
        let bg = if t.bg == Color::Reset { Style::new() } else { Style::new().bg(t.bg) };
        f.render_widget(block.style(bg), r);
        inner
    }

    fn draw_popup(&mut self, f: &mut Frame) {
        let t = self.look.theme;
        let input = |buf: &str, cur: usize| {
            let before: String = buf.chars().take(cur).collect();
            let at: String = buf.chars().skip(cur).take(1).collect();
            let after: String = buf.chars().skip(cur + 1).collect();
            Line::from(vec![
                Span::styled(format!(" {before}"), Style::new().fg(t.fg)),
                Span::styled(if at.is_empty() { " ".into() } else { at }, Style::new().fg(t.base).bg(t.primary)),
                Span::styled(after, Style::new().fg(t.fg)),
            ])
        };
        match &self.mode {
            Mode::Input { ask: Ask::Filter, buf, cur } => {
                // el filtro va en la línea de abajo, sin tapar la lista
                let a = f.area();
                let r = Rect { x: a.x, y: a.y + a.height - 1, width: a.width, height: 1 };
                f.render_widget(Clear, r);
                let mut line = input(buf, *cur);
                line.spans.insert(0, Span::styled(" /", Style::new().fg(t.primary).bold()));
                f.render_widget(Paragraph::new(line), r);
            }
            Mode::Input { ask, buf, cur } => {
                let title = match ask {
                    Ask::Rename(_) => l("renombrar", "rename"),
                    Ask::Create => l("nuevo (termina en / para carpeta)", "new (end with / for a folder)"),
                    Ask::OpenWith(_) => l("abrir con (comando)", "open with (command)"),
                    Ask::Filter => "",
                };
                let w = (buf.width() as u16 + 6).max(50);
                let inner = self.popup(f, w, 3, title);
                f.render_widget(Paragraph::new(input(buf, *cur)), inner);
            }
            Mode::Confirm { msg, .. } => {
                let inner = self.popup(f, msg.width() as u16 + 6, 4, l("¿seguro?", "sure?"));
                f.render_widget(
                    Paragraph::new(vec![
                        Line::from(Span::styled(format!(" {msg}"), Style::new().fg(t.fg))),
                        Line::from(vec![
                            Span::styled(" y ", Style::new().fg(t.primary).bold()),
                            Span::styled(l("sí   ", "yes   "), Style::new().fg(t.muted)),
                            Span::styled(" n ", Style::new().fg(t.primary).bold()),
                            Span::styled("no", Style::new().fg(t.muted)),
                        ]),
                    ]),
                    inner,
                );
            }
            Mode::Mounts { drives, sel } => {
                let inner = self.popup(f, 64, drives.len() as u16 + 2, l("memorias y discos", "drives"));
                let (st, sym) = self.look.sel(true);
                let lines: Vec<Line> = drives
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let where_ = d.mount.as_deref().map(files::tilde_str).unwrap_or_else(|| l("sin montar", "not mounted").to_string());
                        let text = format!("{} {:<20} {:>7}  {}", if i == *sel { sym } else { " " }, trunc(&d.label, 20), d.size, where_);
                        let text = format!("{text:<w$}", w = inner.width as usize);
                        if i == *sel {
                            Line::from(Span::styled(text, if self.look.select == Select::Bar { Style::new().fg(t.primary).bold() } else { st }))
                        } else {
                            Line::from(Span::styled(text, Style::new().fg(if d.mount.is_some() { t.fg } else { t.muted })))
                        }
                    })
                    .collect();
                for i in 0..drives.len().min(inner.height as usize) {
                    self.hits.push((Rect { x: inner.x, y: inner.y + i as u16, width: inner.width, height: 1 }, Hit::Row(i)));
                }
                f.render_widget(Paragraph::new(lines), inner);
            }
            Mode::Help => {
                let keys: &[(&str, &str, &str)] = &[
                    ("j k  ↑ ↓", "moverse", "move"),
                    ("h l  ← →", "subir / entrar", "up / enter"),
                    ("enter", "abrir (texto en el editor, lo demás con su app)", "open (text in the editor, the rest with its app)"),
                    ("o", "abrir con un comando", "open with a command"),
                    ("e", "editar", "edit"),
                    ("space", "marcar · ctrl+a todo · esc nada", "mark · ctrl+a all · esc none"),
                    ("y x p", "copiar · cortar · pegar", "copy · cut · paste"),
                    ("d  D", "a la papelera · borrar para siempre", "to the trash · delete forever"),
                    ("r  a", "renombrar · nuevo (con / es carpeta)", "rename · new (ending in / makes a folder)"),
                    ("/", "filtrar esta carpeta", "filter this folder"),
                    ("z  f", "saltar con zoxide · buscar archivo (fzf)", "jump with zoxide · find a file (fzf)"),
                    ("g …", "ir a: h home, d descargas, p imágenes, c .config, t papelera, u usb", "go to: h home, d downloads, p pictures, c .config, t trash, u usb"),
                    ("~  -", "home · la carpeta de antes", "home · previous folder"),
                    ("M", "memorias USB: montar, desmontar, expulsar", "USB drives: mount, unmount, eject"),
                    (".", "ver / esconder ocultos", "show / hide hidden"),
                    ("s  S", "cambiar orden · al revés", "change sort · reverse"),
                    ("J  K", "mover la vista previa", "scroll the preview"),
                    ("c  C", "copiar la ruta · la de la carpeta", "copy the path · the folder's"),
                    ("!", "una terminal aquí", "a shell here"),
                    ("P", "al cel (picha)", "to the phone (picha)"),
                    ("q  Q", "salir (y cd a donde quedó) · salir", "quit (and cd there) · quit"),
                ];
                let inner = self.popup(f, 84, keys.len() as u16 + 2, l("teclas", "keys"));
                let lines: Vec<Line> = keys
                    .iter()
                    .map(|(k, es, en)| {
                        Line::from(vec![
                            Span::styled(format!(" {k:<10}"), Style::new().fg(t.primary).bold()),
                            Span::styled(if spanish() { *es } else { *en }, Style::new().fg(t.fg)),
                        ])
                    })
                    .collect();
                f.render_widget(Paragraph::new(lines), inner);
            }
            _ => {}
        }
    }

    // ── imágenes (kitty) ─────────────────────────────────────────────────────
    fn update_image(&mut self) {
        if !self.images {
            return;
        }
        let want = if matches!(self.mode, Mode::Normal | Mode::Goto | Mode::Input { ask: Ask::Filter, .. }) { self.img_want.clone() } else { None };
        if want == self.img_placed {
            self.img_since = Instant::now();
            return;
        }
        if self.img_placed.is_some() {
            icat_clear();
            self.img_placed = None;
        }
        // esperar a que el cursor se quede quieto un momento (al pasar rápido
        // por muchas imágenes no se pinta cada una)
        if let Some((p, r)) = want {
            if self.img_since.elapsed() >= Duration::from_millis(90) && r.width > 2 && r.height > 1 {
                let _ = Command::new("kitten")
                    .args(["icat", "--stdin=no", "--transfer-mode=file", "--align=left", "--place"])
                    .arg(format!("{}x{}@{}x{}", r.width, r.height, r.x, r.y))
                    .arg(&p)
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                self.img_placed = Some((p, r));
            }
        }
    }

    fn tick(&mut self) {
        while let Ok(p) = self.pv_rx.try_recv() {
            self.preview = Some(p);
        }
        let mut finished = false;
        while let Ok(p) = self.progress.try_recv() {
            match p {
                Progress::Step(i, n, name) => self.job = Some((i, n, name)),
                Progress::Done(msg) => {
                    self.job = None;
                    self.say(msg);
                    finished = true;
                }
                Progress::Failed(e) => {
                    self.job = None;
                    self.warn(e);
                    finished = true;
                }
            }
        }
        // la carpeta cambió por fuera (o terminó algo de aquí)
        let m = std::fs::metadata(&self.cwd).and_then(|m| m.modified()).ok();
        if finished || m != self.dir_mtime {
            if !self.cwd.exists() {
                let mut up = self.cwd.clone();
                while !up.exists() && up.pop() {}
                self.load(&up, None);
            } else {
                self.reload();
            }
        }
        self.ask_preview();
    }
}

fn icat_clear() {
    let _ = Command::new("kitten").args(["icat", "--clear", "--stdin=no"]).stdin(Stdio::null()).stderr(Stdio::null()).status();
}

fn suspend(term: &mut DefaultTerminal, app: &mut App, run: Run) {
    if app.img_placed.take().is_some() {
        icat_clear();
    }
    let _ = execute!(std::io::stdout(), event::DisableMouseCapture);
    ratatui::restore();
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| if have("nvim") { "nvim".into() } else { "nano".into() });
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    let picked = |cmd: &mut Command| -> Option<String> {
        let out = cmd.stdout(Stdio::piped()).current_dir(&app.cwd).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    match run {
        Run::Edit(paths) => {
            let _ = Command::new("sh").arg("-c").arg(format!("{editor} \"$@\"")).arg("sh").args(&paths).current_dir(&app.cwd).status();
        }
        Run::Shell => {
            println!("{}", l("taco: escribe exit para volver", "taco: type exit to come back"));
            let _ = Command::new(&shell).current_dir(&app.cwd).status();
        }
        Run::Zoxide => {
            if !have("zoxide") {
                app.warn(l("Falta zoxide", "zoxide is missing"));
            } else if let Some(d) = picked(Command::new("zoxide").args(["query", "-i"])) {
                app.load(Path::new(&d), None);
            }
        }
        Run::Find => {
            let list = if have("fd") { "fd --type f --hidden --exclude .git" } else { "find . -type f -not -path '*/.git/*'" };
            if !have("fzf") {
                app.warn(l("Falta fzf", "fzf is missing"));
            } else if let Some(p) = picked(Command::new("sh").arg("-c").arg(format!("{list} | fzf --reverse"))) {
                let p = app.cwd.join(p);
                if let (Some(dir), Some(name)) = (p.parent(), p.file_name()) {
                    let name = name.to_string_lossy().into_owned();
                    app.load(dir, Some(name));
                }
            }
        }
    }
    *term = ratatui::init();
    let _ = execute!(std::io::stdout(), event::EnableMouseCapture);
    let _ = term.clear();
    app.reload();
}

/// file:///home/x/Mi%20carpeta → /home/x/Mi carpeta (lo que manda xdg-open)
fn from_uri(s: &str) -> PathBuf {
    let Some(rest) = s.strip_prefix("file://") else { return PathBuf::from(s) };
    let b = rest.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut start = None;
    let mut cwd_file = None;
    let mut choose = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("taco [{}] [--cwd-file {}] [--choose {}]", l("carpeta|archivo", "folder|file"), l("RUTA", "PATH"), l("RUTA", "PATH"));
                println!("  {}", l("? dentro de taco enseña las teclas", "? inside taco shows the keys"));
                return;
            }
            "-V" | "--version" => {
                println!("taco {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--cwd-file" => cwd_file = args.next().map(PathBuf::from),
            "--choose" => choose = args.next().map(PathBuf::from),
            _ if a.starts_with("--choose=") => choose = Some(PathBuf::from(&a["--choose=".len()..])),
            _ if a.starts_with("--cwd-file=") => cwd_file = Some(PathBuf::from(&a["--cwd-file=".len()..])),
            _ => start = Some(from_uri(&a)),
        }
    }
    let start = start.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| files::home()));
    // un archivo: abrir su carpeta con él elegido
    let (dir, select) = if start.is_file() {
        (start.parent().map(Path::to_path_buf).unwrap_or_else(files::home), start.file_name().map(|n| n.to_string_lossy().into_owned()))
    } else {
        (start, None)
    };
    let mut app = App::new(dir, select, cwd_file);
    app.choose = choose;

    let mut term = ratatui::init();
    let _ = execute!(std::io::stdout(), event::EnableMouseCapture);
    let mut last_look = Instant::now();
    let mut last_tick = Instant::now() - Duration::from_secs(1);
    while !app.quit {
        if last_look.elapsed() > Duration::from_secs(1) {
            if app.look.refresh() && app.img_placed.take().is_some() {
                icat_clear();
            }
            last_look = Instant::now();
        }
        if last_tick.elapsed() > Duration::from_millis(400) {
            app.tick();
            last_tick = Instant::now();
        } else {
            while let Ok(p) = app.pv_rx.try_recv() {
                app.preview = Some(p);
            }
            app.ask_preview();
        }
        let _ = term.draw(|f| app.draw(f));
        app.update_image();
        if let Some(run) = app.run.take() {
            suspend(&mut term, &mut app, run);
            continue;
        }
        // más seguido mientras se espera una vista previa o una imagen
        let waiting = app.pv_want.as_ref() != app.preview.as_ref().map(|p| &p.path) || app.img_want != app.img_placed || app.job.is_some();
        let wait = if waiting { 40 } else { 250 };
        if event::poll(Duration::from_millis(wait)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => app.key(k),
                Ok(Event::Mouse(m)) => app.mouse(m),
                Ok(Event::Resize(..)) => {
                    if app.img_placed.take().is_some() {
                        icat_clear();
                    }
                }
                _ => {}
            }
        }
    }
    if app.img_placed.is_some() {
        icat_clear();
    }
    let _ = execute!(std::io::stdout(), event::DisableMouseCapture);
    ratatui::restore();
}
