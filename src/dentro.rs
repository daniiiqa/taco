//! Entrar a un comprimido como a una carpeta: enter lo descomprime (con lo
//! mismo de X) en ~/.cache/taco/dentro/<pid>/, de solo lectura, y taco lo
//! recorre como cualquier carpeta: vista previa, abrir, copiar hacia fuera.
//! Al salir del comprimido (o de taco) se borra. Lo que dejó un taco que se
//! cayó lo limpia el siguiente al arrancar.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Más que esto descomprimido, se pregunta antes (todo se descomprime).
pub const LIMITE: u64 = 2 * 1024 * 1024 * 1024;

fn base() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::files::home().join(".cache"))
        .join("taco/dentro")
}

/// Lo de este taco.
pub fn root() -> PathBuf {
    base().join(std::process::id().to_string())
}

/// ¿Está `p` dentro de algún comprimido abierto (de cualquier taco)?
pub fn is_inside(p: &Path) -> bool {
    p.starts_with(base())
}

/// Con enter se entra por la extensión, no por la firma: un .docx, .jar o .apk
/// son zip por dentro, pero se abren con su app. (X sí mira la firma.)
pub fn peekable(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    let known = matches!(
        ext,
        "zip" | "tar" | "tgz" | "tbz" | "tbz2" | "txz" | "tzst" | "tlz" | "gz" | "bz2" | "xz" | "zst" | "lz4" | "lzma"
            | "lz" | "z" | "rar" | "7z" | "cab" | "cpio" | "iso" | "deb" | "rpm" | "lzh" | "lha" | "xar"
    );
    known && crate::archive::is_archive(p)
}

/// Cuánto pesa descomprimido, según la lista del comprimido (`bsdtar -tv`).
/// None si no se sabe (un .gz suelto no trae lista).
pub fn unpacked_size(src: &Path) -> Option<u64> {
    let out = Command::new("bsdtar").arg("-tvf").arg(src).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(sum_listing(&String::from_utf8_lossy(&out.stdout)))
}

/// `-rw-r--r--  0 dany dany  1234 Nov 10 12:00 a.txt`: el quinto campo.
fn sum_listing(listing: &str) -> u64 {
    listing.lines().filter_map(|l| l.split_whitespace().nth(4)?.parse::<u64>().ok()).sum()
}

/// Una carpeta por comprimido (y por versión: si cambia, otra).
fn slot(src: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    src.hash(&mut h);
    if let Ok(m) = fs::metadata(src) {
        m.len().hash(&mut h);
        m.modified().ok().hash(&mut h);
    }
    root().join(format!("{:016x}", h.finish()))
}

/// Descomprime `src` (si no estaba ya) y devuelve la carpeta para recorrer.
pub fn open(src: &Path) -> Result<PathBuf, String> {
    let slot = slot(src);
    // ya abierto antes (y no cambió): lo mismo
    if let Some(Ok(e)) = fs::read_dir(&slot).ok().and_then(|mut d| d.next()) {
        return Ok(e.path());
    }
    let name = crate::archive::folder_name(&src.file_name().unwrap_or_default().to_string_lossy());
    let dir = crate::archive::extract(src, &slot, &name)?;
    read_only(&dir);
    Ok(dir)
}

/// Que nada de adentro se pueda cambiar: es una copia que se va a borrar.
fn read_only(p: &Path) {
    let Ok(meta) = fs::symlink_metadata(p) else { return };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        for e in fs::read_dir(p).into_iter().flatten().flatten() {
            read_only(&e.path());
        }
    }
    let mode = meta.permissions().mode() & !0o222;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(mode));
}

/// Borrar una carpeta de solo lectura: primero devolverle el permiso de escribir.
pub fn remove(p: &Path) {
    fn writable(p: &Path) {
        let Ok(meta) = fs::symlink_metadata(p) else { return };
        if meta.file_type().is_symlink() {
            return;
        }
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(meta.permissions().mode() | 0o200));
        if meta.is_dir() {
            for e in fs::read_dir(p).into_iter().flatten().flatten() {
                writable(&e.path());
            }
        }
    }
    writable(p);
    let _ = fs::remove_dir_all(p);
}

/// Lo que dejaron tacos que ya no están (se cayeron o los mataron).
pub fn clean_stale() {
    for e in fs::read_dir(base()).into_iter().flatten().flatten() {
        let alive = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()).is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists());
        if !alive {
            remove(&e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suma_la_lista() {
        let l = "drwxr-xr-x  0 dany dany       0 Nov 10 12:00 foo/\n-rw-r--r--  0 dany dany    1234 Nov 10 12:00 foo/a.txt\n-rw-r--r--  0 dany dany 1000000 Nov 10 12:00 b.bin\n";
        assert_eq!(sum_listing(l), 1_001_234);
        assert_eq!(sum_listing(""), 0);
    }

    #[test]
    fn solo_lectura_y_se_borra() {
        let d = std::env::temp_dir().join(format!("taco-dentro-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/b/c.txt"), "x").unwrap();
        read_only(&d.join("a"));
        assert!(fs::write(d.join("a/b/c.txt"), "y").is_err(), "no se puede cambiar");
        assert!(fs::write(d.join("a/nuevo"), "y").is_err(), "ni agregar");
        remove(&d.join("a"));
        assert!(!d.join("a").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn abre_reusa_y_se_va() {
        let d = std::env::temp_dir().join(format!("taco-abre-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src/foo")).unwrap();
        fs::write(d.join("src/foo/a.txt"), "hola").unwrap();
        // la caché de la prueba, no la de verdad (solo esta prueba la lee)
        unsafe { std::env::set_var("XDG_CACHE_HOME", d.join("cache")) };
        let ok = Command::new("sh").arg("-c").arg("bsdtar -a -cf t.zip foo").current_dir(d.join("src")).status().unwrap().success();
        assert!(ok);
        let src = d.join("src/t.zip");
        assert!(peekable(&src));
        let dir = open(&src).unwrap();
        assert!(dir.starts_with(root()) && is_inside(&dir));
        // una sola carpeta adentro: se entra directo a ella
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "hola");
        assert!(fs::write(dir.join("a.txt"), "x").is_err(), "solo lectura");
        assert_eq!(open(&src).unwrap(), dir, "la segunda vez, la misma copia");
        remove(&root());
        assert!(!root().exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn entra_por_extension_no_por_firma() {
        let d = std::env::temp_dir().join(format!("taco-peek-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("b.txt"), "hola").unwrap();
        let ok = |c: &str| Command::new("sh").arg("-c").arg(c).current_dir(&d).status().unwrap().success();
        assert!(ok("bsdtar -a -cf t.zip b.txt && cp t.zip doc.docx"));
        assert!(peekable(&d.join("t.zip")));
        assert!(!peekable(&d.join("doc.docx")), "un docx se abre con su app");
        assert!(!peekable(&d.join("b.txt")));
        let _ = fs::remove_dir_all(&d);
    }
}
