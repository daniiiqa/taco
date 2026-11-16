//! Descomprimir: taco mira los primeros bytes del archivo (no se fía de la
//! extensión) y elige con qué. bsdtar abre casi todo (zip, tar.*, rar, 7z, iso,
//! deb…); 7z, lo que a bsdtar se le atora; bsdcat, los que son un solo archivo
//! comprimido sin tar adentro (notas.txt.gz).
//!
//! Se descomprime en una carpeta oculta junto al destino y solo al terminar toma
//! su nombre: si algo falla no queda nada a medias. Si lo de adentro ya venía en
//! una sola carpeta, esa toma el nombre elegido, para no dejar foo/foo/.

use crate::files;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// bsdtar y 7z le piden la contraseña directo a la terminal, y eso rompería la
/// pantalla de taco. Con una de mentira fallan sin preguntar; a los comprimidos
/// sin contraseña no les afecta.
const NO_PASSWORD: &str = "taco-sin-clave";

/// Los comprimidos de un solo flujo: pueden traer un tar adentro o un archivo suelto.
fn compressed_magic(h: &[u8]) -> bool {
    const MAGIC: [&[u8]; 7] = [
        b"\x1f\x8b",                 // gzip
        b"\x1f\x9d",                 // compress (.Z)
        b"BZh",                      // bzip2
        b"\xfd7zXZ\x00",             // xz
        b"\x28\xb5\x2f\xfd",         // zstd
        b"\x04\x22\x4d\x18",         // lz4
        b"LZIP",                     // lzip
    ];
    MAGIC.iter().any(|m| h.starts_with(m))
}

fn archive_magic(h: &[u8]) -> bool {
    const MAGIC: [&[u8]; 13] = [
        b"PK\x03\x04",               // zip (y jar, apk, docx…)
        b"PK\x05\x06",               // zip vacío
        b"PK\x07\x08",               // zip partido
        b"Rar!\x1a\x07",             // rar
        b"7z\xbc\xaf\x27\x1c",       // 7z
        b"!<arch>\n",                // ar (deb)
        b"\xed\xab\xee\xdb",         // rpm
        b"MSCF",                     // cab
        b"070701",                   // cpio
        b"070702",
        b"070707",
        b"xar!",                     // xar (pkg de mac)
        b"MSWIM",                    // wim
    ];
    let at = |off: usize, m: &[u8]| h.len() >= off + m.len() && &h[off..off + m.len()] == m;
    MAGIC.iter().any(|m| h.starts_with(m)) || at(257, b"ustar") || at(32769, b"CD001") || at(2, b"-lh")
}

/// Los que no tienen una firma que sirva: lzma no tiene, y la de msi es la misma de un .doc viejo.
fn archive_ext(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "tar" | "lzma" | "tlz" | "msi" | "dmg")
}

fn head(p: &Path) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(f) = fs::File::open(p) {
        let _ = f.take(33 * 1024).read_to_end(&mut buf);
    }
    buf
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// ¿Se puede intentar descomprimir? Solo lee el principio del archivo.
pub fn is_archive(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let h = head(p);
    !h.is_empty() && (compressed_magic(&h) || archive_magic(&h) || archive_ext(&name_of(p)))
}

/// notas.txt.gz → notas.txt
fn strip_compression(name: &str) -> &str {
    if let Some((stem, ext)) = name.rsplit_once('.')
        && !stem.is_empty() && matches!(ext.to_ascii_lowercase().as_str(), "gz" | "bz2" | "xz" | "zst" | "lz4" | "lzma" | "lz" | "z") {
        return stem;
    }
    name
}

/// El nombre que se propone: foo-1.2.tar.gz → foo-1.2, foo-1.2.zip → foo-1.2
pub fn folder_name(file: &str) -> String {
    let inner = strip_compression(file);
    let stem = if inner != file {
        // solo quitar .tar: notas.txt.gz → notas.txt, y foo-1.2.gz no pierde el .2
        match inner.rsplit_once('.') {
            Some((s, e)) if e.eq_ignore_ascii_case("tar") => s,
            _ => inner,
        }
    } else {
        file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file)
    };
    if stem.is_empty() { file.to_string() } else { stem.to_string() }
}

/// Lo que se escribió como destino, como ruta de verdad (~ y relativas).
pub fn resolve(input: &str, cwd: &Path) -> PathBuf {
    let s = input.trim();
    if s == "~" {
        files::home()
    } else if let Some(rest) = s.strip_prefix("~/") {
        files::home().join(rest)
    } else {
        cwd.join(s)
    }
}

/// Lo que dijo el programa al fallar: su última línea que diga algo.
fn last_words(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    let std = String::from_utf8_lossy(&out.stdout);
    err.lines()
        .chain(std.lines())
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.contains("Error exit delayed") && *l != "ERRORS:" && *l != "ERROR:")
        .unwrap_or("error")
        .to_string()
}

enum Fail {
    /// no está instalado
    Missing,
    Said(String),
}

fn run(cmd: &mut Command) -> Result<(), Fail> {
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::piped()).output().map_err(|_| Fail::Missing)?;
    if out.status.success() { Ok(()) } else { Err(Fail::Said(last_words(&out))) }
}

/// Vaciar lo que dejó a medias el intento anterior.
fn fresh(tmp: &Path) -> Result<(), String> {
    fs::remove_dir_all(tmp).and_then(|_| fs::create_dir(tmp)).map_err(|e| e.to_string())
}

fn unpack(src: &Path, tmp: &Path) -> Result<(), String> {
    let es = crate::spanish();
    let single = compressed_magic(&head(src)) || name_of(src).to_ascii_lowercase().ends_with(".lzma");
    let tar = run(Command::new("bsdtar").arg("--passphrase").arg(NO_PASSWORD).arg("-xf").arg(src).arg("-C").arg(tmp).stdout(Stdio::piped()));
    let tar = match tar {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    fresh(tmp)?;
    // un gz sin tar adentro: no es comprimido de carpetas, es un archivo solo
    if single && matches!(&tar, Fail::Said(s) if s.contains("Unrecognized archive format")) {
        let to = tmp.join(strip_compression(&name_of(src)));
        let file = fs::File::create(&to).map_err(|e| e.to_string())?;
        return match run(Command::new("bsdcat").arg(src).stdout(file)) {
            Ok(()) => Ok(()),
            Err(Fail::Missing) => Err(missing(es, "bsdcat (libarchive)")),
            Err(Fail::Said(s)) => Err(s),
        };
    }
    let seven = run(Command::new("7z")
        .args(["x", "-y", "-bd", "-bso0", "-bsp0", "-spd"])
        .arg(format!("-p{NO_PASSWORD}"))
        .arg(format!("-o{}", tmp.display()))
        .arg("--")
        .arg(src)
        .stdout(Stdio::piped()));
    let seven = match seven {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    let locked = |f: &Fail| matches!(f, Fail::Said(s) if { let s = s.to_lowercase(); s.contains("passphrase") || s.contains("password") || s.contains("encrypted") });
    if locked(&tar) || locked(&seven) {
        return Err(if es { "Tiene contraseña; taco todavía no sabe abrirlos".into() } else { "It's password-protected; taco can't open those yet".into() });
    }
    match (tar, seven) {
        (Fail::Missing, Fail::Missing) => Err(missing(es, "bsdtar / 7z")),
        // lo que dijo bsdtar suele explicar más ("truncated gzip input")
        (Fail::Said(s), _) | (Fail::Missing, Fail::Said(s)) => Err(s),
    }
}

fn missing(es: bool, what: &str) -> String {
    if es { format!("Falta {what}") } else { format!("{what} is missing") }
}

/// La carpeta temporal ya tiene todo: que tome su lugar y su nombre.
fn place(tmp: &Path, dest: &Path, name: &str) -> std::io::Result<PathBuf> {
    let inside: Vec<PathBuf> = fs::read_dir(tmp)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    let to = files::free_name(dest, name);
    match inside.as_slice() {
        [only] if fs::symlink_metadata(only)?.is_dir() => {
            fs::rename(only, &to)?;
            fs::remove_dir(tmp)?;
        }
        _ => fs::rename(tmp, &to)?,
    }
    Ok(to)
}

/// Descomprimir `src` en `dest/name` (crea `dest` si hace falta). Devuelve dónde quedó.
pub fn extract(src: &Path, dest: &Path, name: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let tmp = files::free_name(dest, &format!(".{name}.taco"));
    fs::create_dir(&tmp).map_err(|e| e.to_string())?;
    let r = unpack(src, &tmp).and_then(|_| place(&tmp, dest, name).map_err(|e| e.to_string()));
    if r.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    r
}

// ── comprimir ────────────────────────────────────────────────────────────────

/// Los formatos para comprimir, en el orden en que se van cambiando. zip
/// primero: es el que abre cualquiera (Windows, el cel).
pub const FORMATS: [&str; 6] = [".zip", ".tar.zst", ".tar.gz", ".tar.xz", ".7z", ".tar"];

/// La extensión de formato que trae el nombre (la más larga: .tar.gz antes que .tar).
pub fn format_of(name: &str) -> Option<&'static str> {
    let low = name.to_ascii_lowercase();
    FORMATS.iter().copied().filter(|f| low.len() > f.len() && low.ends_with(f)).max_by_key(|f| f.len())
}

/// Qué le dice a bsdtar cada formato. El nombre de la temporal no termina en
/// la extensión, por eso no sirve `-a`.
fn format_flags(ext: &str) -> &'static [&'static str] {
    match ext {
        ".zip" => &["--format", "zip"],
        ".7z" => &["--format", "7zip"],
        ".tar.zst" => &["--zstd"],
        ".tar.gz" => &["-z"],
        ".tar.xz" => &["-J"],
        _ => &[],
    }
}

/// mods.zip → mods.tar.zst: cambiar al siguiente formato sin tocar el nombre.
pub fn next_format(name: &str) -> String {
    let (stem, i) = match format_of(name) {
        Some(f) => (&name[..name.len() - f.len()], FORMATS.iter().position(|x| *x == f).unwrap() + 1),
        None => (name, 0),
    };
    format!("{stem}{}", FORMATS[i % FORMATS.len()])
}

/// El nombre que se propone: la carpeta o el archivo (sin su extensión) + .zip,
/// y si ya existe, mods_1.zip, mods_2.zip…
pub fn pack_name(first: &Path, many: bool, cwd: &Path, dest: &Path) -> String {
    let base = if many {
        name_of(cwd)
    } else {
        let n = name_of(first);
        match n.rsplit_once('.') {
            Some((s, _)) if !s.is_empty() && !first.is_dir() => s.to_string(),
            _ => n,
        }
    };
    let base = if base.is_empty() { "taco".to_string() } else { base };
    (0..)
        .map(|i| if i == 0 { format!("{base}.zip") } else { format!("{base}_{i}.zip") })
        .find(|n| fs::symlink_metadata(dest.join(n)).is_err())
        .unwrap()
}

/// Comprimir `srcs` (cada uno con su nombre, sin la ruta de arriba) en
/// `dest/name`, con el formato que diga la extensión. Se escribe en una
/// temporal oculta y solo al terminar toma el nombre: si algo falla no queda
/// nada a medias.
pub fn compress(srcs: &[PathBuf], dest: &Path, name: &str) -> Result<PathBuf, String> {
    let es = crate::spanish();
    let ext = format_of(name).ok_or_else(|| if es { "No sé de qué formato es ese nombre".to_string() } else { "Unknown format for that name".to_string() })?;
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let to = dest.join(name);
    if fs::symlink_metadata(&to).is_ok() {
        return Err(if es { "Ya existe".into() } else { "Already exists".into() });
    }
    let tmp = files::free_name(dest, &format!(".{name}.taco"));
    let mut cmd = Command::new("bsdtar");
    cmd.arg("-cf").arg(&tmp).args(format_flags(ext));
    for s in srcs {
        let parent = s.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
        let parent = fs::canonicalize(&parent).unwrap_or(parent);
        let n = name_of(s);
        // que un nombre con - adelante no se lea como opción
        let n = if n.starts_with('-') { format!("./{n}") } else { n };
        cmd.arg("-C").arg(parent).arg(n);
    }
    let r = match run(cmd.stdout(Stdio::piped())) {
        Ok(()) => fs::rename(&tmp, &to).map(|_| to).map_err(|e| e.to_string()),
        Err(Fail::Missing) => Err(missing(es, "bsdtar (libarchive)")),
        Err(Fail::Said(s)) => Err(s),
    };
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una carpeta de pruebas con `foo/a.txt` y `b.txt` listos para comprimir.
    struct Lab(PathBuf);
    impl Lab {
        fn new(tag: &str) -> Lab {
            let d = std::env::temp_dir().join(format!("taco-test-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(d.join("src/foo")).unwrap();
            fs::create_dir_all(d.join("out")).unwrap();
            fs::write(d.join("src/foo/a.txt"), "hola\n").unwrap();
            fs::write(d.join("src/b.txt"), "adios\n").unwrap();
            Lab(d)
        }
        fn sh(&self, cmd: &str) {
            let ok = Command::new("sh").arg("-c").arg(cmd).current_dir(self.0.join("src")).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().success();
            assert!(ok, "{cmd}");
        }
        fn src(&self, name: &str) -> PathBuf {
            self.0.join("src").join(name)
        }
        fn out(&self) -> PathBuf {
            self.0.join("out")
        }
        /// Sin restos: lo único en out/ es lo que se esperaba.
        fn out_names(&self) -> Vec<String> {
            let mut v: Vec<String> = fs::read_dir(self.out()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn names() {
        assert_eq!(folder_name("foo-1.2.tar.gz"), "foo-1.2");
        assert_eq!(folder_name("foo-1.2.zip"), "foo-1.2");
        assert_eq!(folder_name("foo.TGZ"), "foo");
        assert_eq!(folder_name("notas.txt.gz"), "notas.txt");
        assert_eq!(folder_name("foo-1.2.gz"), "foo-1.2");
        assert_eq!(folder_name(".zip"), ".zip");
        assert_eq!(folder_name("sin"), "sin");
        assert_eq!(strip_compression("a.tar.zst"), "a.tar");
    }

    #[test]
    fn paths() {
        let cwd = Path::new("/x/y");
        assert_eq!(resolve("~", cwd), files::home());
        assert_eq!(resolve("~/a", cwd), files::home().join("a"));
        assert_eq!(resolve("a/b", cwd), Path::new("/x/y/a/b"));
        assert_eq!(resolve("/z", cwd), Path::new("/z"));
    }

    #[test]
    fn varios_archivos_quedan_en_la_carpeta_elegida() {
        let l = Lab::new("flat");
        l.sh("bsdtar -a -cf t.zip foo b.txt");
        assert!(is_archive(&l.src("t.zip")));
        let to = extract(&l.src("t.zip"), &l.out(), "elegida").unwrap();
        assert_eq!(to, l.out().join("elegida"));
        assert_eq!(fs::read_to_string(to.join("foo/a.txt")).unwrap(), "hola\n");
        assert!(to.join("b.txt").exists());
        assert_eq!(l.out_names(), ["elegida"]);
    }

    #[test]
    fn una_sola_carpeta_adentro_no_se_anida() {
        let l = Lab::new("single");
        l.sh("bsdtar -czf t.tar.gz foo");
        let to = extract(&l.src("t.tar.gz"), &l.out(), "elegida").unwrap();
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "hola\n");
        assert_eq!(l.out_names(), ["elegida"]);
    }

    #[test]
    fn gz_de_un_solo_archivo() {
        let l = Lab::new("gz");
        l.sh("gzip -k b.txt");
        assert!(is_archive(&l.src("b.txt.gz")));
        let to = extract(&l.src("b.txt.gz"), &l.out(), "notas").unwrap();
        assert_eq!(fs::read_to_string(to.join("b.txt")).unwrap(), "adios\n");
    }

    #[test]
    fn la_extension_no_manda() {
        let l = Lab::new("magic");
        l.sh("bsdtar -a -cf t.zip b.txt && mv t.zip engañoso.dat");
        assert!(is_archive(&l.src("engañoso.dat")));
        assert!(extract(&l.src("engañoso.dat"), &l.out(), "x").unwrap().join("b.txt").exists());
        assert!(!is_archive(&l.src("b.txt")));
    }

    #[test]
    fn siete_z_y_destino_nuevo() {
        let l = Lab::new("7z");
        l.sh("7z a t.7z foo b.txt");
        let to = extract(&l.src("t.7z"), &l.out().join("no/existia"), "x").unwrap();
        assert!(to.join("foo/a.txt").exists());
    }

    #[test]
    fn con_contrasena_avisa_y_no_deja_nada() {
        let l = Lab::new("enc");
        l.sh("7z a -pSECRETO t.zip b.txt && 7z a -pSECRETO -mhe=on t.7z b.txt");
        for f in ["t.zip", "t.7z"] {
            let e = extract(&l.src(f), &l.out(), "x").unwrap_err();
            assert!(e.contains("ontraseña") || e.contains("password"), "{f}: {e}");
        }
        assert!(l.out_names().is_empty());
    }

    #[test]
    fn roto_avisa_y_no_deja_nada() {
        let l = Lab::new("broken");
        l.sh("bsdtar -czf t.tgz foo && head -c 40 t.tgz > roto.tgz");
        assert!(extract(&l.src("roto.tgz"), &l.out(), "x").is_err());
        assert!(l.out_names().is_empty());
    }

    #[test]
    fn no_pisa_lo_que_ya_existe() {
        let l = Lab::new("exists");
        l.sh("bsdtar -a -cf t.zip foo b.txt");
        fs::create_dir(l.out().join("x")).unwrap();
        fs::write(l.out().join("x/mio"), "").unwrap();
        let to = extract(&l.src("t.zip"), &l.out(), "x").unwrap();
        assert_eq!(to, l.out().join("x_1"));
        assert!(l.out().join("x/mio").exists());
    }

    #[test]
    fn no_se_sale_de_la_carpeta() {
        let l = Lab::new("escape");
        // un tar con ../../malo.txt: -P lo guarda tal cual, y luego se borra el de verdad
        l.sh("touch ../malo.txt && bsdtar -cPf t.tar ../malo.txt b.txt && rm ../malo.txt && bsdtar -tf t.tar | grep -q '^../malo.txt'");
        let _ = extract(&l.src("t.tar"), &l.out(), "x");
        assert!(!l.out().join("malo.txt").exists());
        assert!(!l.0.join("malo.txt").exists());
    }

    #[test]
    fn un_enlace_no_lleva_afuera() {
        let l = Lab::new("link");
        // lnk → .. y luego lnk/malo.txt: escribir a través del enlace dejaría
        // malo.txt junto a la carpeta temporal, fuera de lo descomprimido
        l.sh("mkdir t u && ln -s .. t/lnk && bsdtar -cf t.tar -C t lnk && mkdir u/lnk && echo x > u/lnk/malo.txt && bsdtar -rf t.tar -C u lnk/malo.txt && bsdtar -tf t.tar | grep -q '^lnk/malo.txt'");
        let _ = extract(&l.src("t.tar"), &l.out(), "x");
        assert!(!l.out().join("malo.txt").exists());
        assert!(!l.0.join("malo.txt").exists());
    }
    #[test]
    fn formatos_y_nombres() {
        assert_eq!(format_of("mods.zip"), Some(".zip"));
        assert_eq!(format_of("a.TAR.GZ"), Some(".tar.gz"));
        assert_eq!(format_of("a.tar"), Some(".tar"));
        assert_eq!(format_of(".zip"), None);
        assert_eq!(format_of("a.rar"), None);
        assert_eq!(next_format("mods.zip"), "mods.tar.zst");
        assert_eq!(next_format("mods.tar"), "mods.zip");
        assert_eq!(next_format("mods"), "mods.zip");
        let l = Lab::new("packname");
        let src = l.0.join("src");
        assert_eq!(pack_name(&src.join("foo"), false, &src, &src), "foo.zip");
        assert_eq!(pack_name(&src.join("b.txt"), false, &src, &src), "b.zip");
        assert_eq!(pack_name(&src.join("b.txt"), true, &src, &src), "src.zip");
        fs::write(src.join("foo.zip"), "").unwrap();
        assert_eq!(pack_name(&src.join("foo"), false, &src, &src), "foo_1.zip");
    }

    #[test]
    fn comprimir_y_descomprimir_de_vuelta() {
        for name in ["t.zip", "t.tar.zst", "t.tar.gz", "t.tar.xz", "t.7z", "t.tar"] {
            let l = Lab::new(&format!("pack{}", name.replace('.', "")));
            let srcs = [l.src("foo"), l.src("b.txt")];
            let to = compress(&srcs, &l.out(), name).unwrap();
            assert_eq!(to, l.out().join(name));
            assert!(is_archive(&to), "{name}");
            assert_eq!(l.out_names(), [name]);
            let back = extract(&to, &l.out(), "back").unwrap();
            assert_eq!(fs::read_to_string(back.join("foo/a.txt")).unwrap(), "hola\n", "{name}");
            assert_eq!(fs::read_to_string(back.join("b.txt")).unwrap(), "adios\n", "{name}");
        }
    }

    #[test]
    fn comprimir_no_pisa_ni_deja_restos() {
        let l = Lab::new("packfail");
        fs::write(l.out().join("t.zip"), "mio").unwrap();
        assert!(compress(&[l.src("foo")], &l.out(), "t.zip").is_err());
        assert_eq!(fs::read_to_string(l.out().join("t.zip")).unwrap(), "mio");
        assert!(compress(&[l.src("no-existe")], &l.out(), "x.zip").is_err());
        assert!(compress(&[l.src("foo")], &l.out(), "x.rar").is_err());
        assert_eq!(l.out_names(), ["t.zip"]);
    }

    #[test]
    fn nombre_con_guion() {
        let l = Lab::new("packdash");
        fs::write(l.src("-raro.txt"), "x").unwrap();
        let to = compress(&[l.src("-raro.txt")], &l.out(), "r.tar").unwrap();
        assert!(extract(&to, &l.out(), "back").unwrap().join("-raro.txt").exists());
    }
}
