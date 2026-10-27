//! La vista previa de la derecha. Se arma en otro hilo (las miniaturas de
//! video y PDF, o leer un disco lento, no congelan la lista); si se pide otra
//! antes de terminar, la vieja se descarta.
//!
//! Texto: las primeras líneas. Carpeta: lo que tiene. Imagen: con el protocolo
//! de gráficos de kitty (lo pinta main.rs con `kitten icat`); video, PDF y SVG
//! pasan antes por una miniatura en ~/.cache/taco. Comprimidos: la lista de
//! adentro (bsdtar). Audio: título, artista, álbum y duración (ffprobe).

use crate::files::{self, Entry, Sort};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

pub enum Kind {
    Dir(Vec<Entry>),
    Text(Vec<String>),
    /// el archivo que se le pasa a icat (el original o su miniatura)
    Image(PathBuf),
    Info(Vec<String>),
}

pub struct Preview {
    pub path: PathBuf,
    pub kind: Kind,
}

pub struct Request {
    pub entry: Entry,
    pub hidden: bool,
    pub sort: Sort,
    pub rev: bool,
    /// ¿la terminal pinta imágenes? (kitty)
    pub images: bool,
}

const MAX_LINES: usize = 300;

pub fn worker() -> (Sender<Request>, Receiver<Preview>) {
    let (rtx, rrx) = channel::<Request>();
    let (ptx, prx) = channel();
    std::thread::spawn(move || {
        while let Ok(mut req) = rrx.recv() {
            // solo la última que se pidió
            while let Ok(newer) = rrx.try_recv() {
                req = newer;
            }
            let kind = build(&req);
            if ptx.send(Preview { path: req.entry.path.clone(), kind }).is_err() {
                break;
            }
        }
    });
    (rtx, prx)
}

pub fn is_text(text_ext: &str) -> bool {
    matches!(
        text_ext,
        "txt" | "md" | "rs" | "py" | "js" | "ts" | "lua" | "sh" | "toml" | "json" | "jsonc" | "yaml" | "yml" | "conf"
            | "ini" | "cfg" | "css" | "html" | "c" | "h" | "cpp" | "hpp" | "go" | "java" | "rasi" | "log" | "csv"
            | "xml" | "frag" | "vert" | "glsl" | "gd" | "tscn" | "desktop" | "service"
    )
}

fn image_ext(e: &str) -> bool {
    matches!(e, "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff" | "tif" | "avif" | "heic" | "jxl")
}
fn video_ext(e: &str) -> bool {
    matches!(e, "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "flv" | "wmv")
}
fn audio_ext(e: &str) -> bool {
    matches!(e, "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" | "wma")
}
fn archive_ext(name: &str, e: &str) -> bool {
    matches!(e, "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "jar" | "deb" | "rpm" | "iso")
        || name.ends_with(".pkg.tar.zst")
}

fn build(r: &Request) -> Kind {
    let e = &r.entry;
    if e.dir {
        return match files::list(&e.path, r.hidden, r.sort, r.rev) {
            Ok(mut v) => {
                v.truncate(1000);
                Kind::Dir(v)
            }
            Err(err) => Kind::Info(vec![err.to_string()]),
        };
    }
    if e.broken {
        let to = std::fs::read_link(&e.path).map(|p| p.display().to_string()).unwrap_or_default();
        return Kind::Info(vec![crate::l("Enlace roto", "Broken link").into(), format!("→ {to}")]);
    }
    let ext = e.ext();
    if r.images {
        if image_ext(&ext) {
            return Kind::Image(e.path.clone());
        }
        let thumb = if video_ext(&ext) {
            thumbnail(e, |out| {
                let mut c = Command::new("ffmpegthumbnailer");
                c.arg("-i").arg(&e.path).arg("-o").arg(out).args(["-s", "768", "-q", "6"]);
                c
            })
        } else if ext == "pdf" {
            // pdftoppm le pone .png solo
            thumbnail(e, |out| {
                let mut c = Command::new("pdftoppm");
                c.args(["-png", "-singlefile", "-scale-to", "768"]).arg(&e.path).arg(out.with_extension(""));
                c
            })
        } else if ext == "svg" {
            thumbnail(e, |out| {
                let mut c = Command::new("resvg");
                c.args(["-w", "768"]).arg(&e.path).arg(out);
                c
            })
        } else {
            None
        };
        if let Some(t) = thumb {
            return Kind::Image(t);
        }
    }
    if audio_ext(&ext) {
        if let Some(info) = audio_info(&e.path) {
            return Kind::Info(info);
        }
    }
    if archive_ext(&e.name, &ext) {
        if let Some(lines) = run_lines(Command::new("bsdtar").arg("-tf").arg(&e.path)) {
            return Kind::Text(lines);
        }
    }
    text(&e.path).map(Kind::Text).unwrap_or_else(|| {
        let what = run_lines(Command::new("file").arg("-b").arg(&e.path)).and_then(|v| v.into_iter().next());
        Kind::Info(vec![what.unwrap_or_else(|| crate::l("binario", "binary").into()), files::human(e.size)])
    })
}

/// Las primeras líneas si parece texto (sin bytes nulos y UTF-8 casi siempre).
pub fn text(p: &Path) -> Option<Vec<String>> {
    let mut buf = vec![0u8; 64 * 1024];
    let n = std::fs::File::open(p).and_then(|mut f| f.read(&mut buf)).ok()?;
    buf.truncate(n);
    if buf.contains(&0) {
        return None;
    }
    let s = String::from_utf8_lossy(&buf);
    if s.chars().filter(|&c| c == '\u{fffd}').count() > n / 50 + 1 {
        return None;
    }
    Some(s.lines().take(MAX_LINES).map(clean).collect())
}

/// tabs a espacios y sin caracteres de control (romperían la pantalla)
fn clean(l: &str) -> String {
    l.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect()
}

fn run_lines(cmd: &mut Command) -> Option<Vec<String>> {
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).lines().take(MAX_LINES).map(clean).collect())
}

fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|_| files::home().join(".cache")).join("taco")
}

/// La miniatura en caché (por ruta, fecha y tamaño); la hace `make` si falta.
fn thumbnail(e: &Entry, make: impl Fn(&Path) -> Command) -> Option<PathBuf> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&e.path, e.mtime, e.size).hash(&mut h);
    let out = cache_dir().join(format!("{:016x}.png", h.finish()));
    if out.exists() {
        return Some(out);
    }
    std::fs::create_dir_all(cache_dir()).ok()?;
    let ok = make(&out).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    (ok && out.exists()).then_some(out)
}

fn audio_info(p: &Path) -> Option<Vec<String>> {
    let lines = run_lines(
        Command::new("ffprobe")
            .args(["-v", "quiet", "-show_entries", "format=duration:format_tags=title,artist,album,date", "-of", "default=nw=1"])
            .arg(p),
    )?;
    let mut out = Vec::new();
    for l in lines {
        let Some((k, v)) = l.split_once('=') else { continue };
        let k = k.trim_start_matches("TAG:").to_lowercase();
        let label = match k.as_str() {
            "title" => crate::l("título", "title"),
            "artist" => crate::l("artista", "artist"),
            "album" => crate::l("álbum", "album"),
            "date" => crate::l("año", "year"),
            "duration" => {
                let s = v.parse::<f64>().unwrap_or(0.0) as u64;
                out.push(format!("{}: {}:{:02}", crate::l("duración", "length"), s / 60, s % 60));
                continue;
            }
            _ => continue,
        };
        out.push(format!("{label}: {v}"));
    }
    (!out.is_empty()).then_some(out)
}
