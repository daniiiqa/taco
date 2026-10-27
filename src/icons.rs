//! Iconos de Nerd Font por tipo de archivo (cuando el look usa iconos).

use crate::files::Entry;

pub fn icon(e: &Entry) -> &'static str {
    if e.dir {
        return match e.name.as_str() {
            "Downloads" | "Descargas" => "\u{f024d}",
            "Pictures" | "Imágenes" | "wallpapers" => "\u{f024f}",
            ".config" => "\u{f107b}",
            ".git" => "\u{f02a2}",
            _ if e.path == crate::files::home() => "\u{f10b5}",
            _ => "\u{f024b}",
        };
    }
    if e.link && e.broken {
        return "\u{f0339}";
    }
    match e.name.as_str() {
        ".gitignore" | ".gitmodules" | ".gitattributes" => return "\u{f02a2}",
        "Cargo.toml" | "Cargo.lock" => return "\u{e7a8}",
        "Makefile" | "CMakeLists.txt" | "PKGBUILD" => return "\u{e615}",
        _ => {}
    }
    match e.ext().as_str() {
        "rs" => "\u{e7a8}",
        "py" => "\u{e73c}",
        "js" | "mjs" | "cjs" | "jsx" => "\u{e74e}",
        "ts" | "tsx" => "\u{e628}",
        "lua" => "\u{e620}",
        "sh" | "bash" | "zsh" | "fish" => "\u{f489}",
        "md" | "markdown" => "\u{e73e}",
        "json" | "jsonc" => "\u{e60b}",
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "rasi" => "\u{e615}",
        "html" | "htm" => "\u{e736}",
        "css" | "scss" => "\u{e749}",
        "c" | "h" => "\u{e61e}",
        "cpp" | "cc" | "hpp" | "cxx" => "\u{e61d}",
        "go" => "\u{e626}",
        "java" | "kt" => "\u{e738}",
        "gd" | "godot" | "tscn" => "\u{e65f}",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" | "avif" | "tiff" | "heic" => "\u{f021f}",
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" => "\u{f022b}",
        "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" => "\u{f0223}",
        "pdf" => "\u{f0226}",
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" | "bz2" | "tgz" => "\u{f05c4}",
        "txt" | "log" => "\u{f0219}",
        "frag" | "vert" | "glsl" => "\u{f022e}",
        _ if e.exec() => "\u{f489}",
        _ => "\u{f0214}",
    }
}
