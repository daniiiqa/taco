# taco

*Taco*: lo que sea, bien envuelto. Escribes `taco` en la terminal y ahí están
tus archivos: la carpeta de arriba, la de ahora y una vista previa.

Parte de [discada](https://github.com/daniiiqa/dotfiles), hermano de
[rola](https://github.com/daniiiqa/rola), [vato](https://github.com/daniiiqa/vato)
y [arre](https://github.com/daniiiqa/arre): mismo look en cada estilo del rice
(bordes, cómo se marca lo elegido, iconos o solo texto, mayúsculas, la barra de
progreso y los colores del theme de kitty), en español o inglés según discada.
En discada abre con `SUPER+E` y con "abrir carpeta" desde cualquier app.

```
taco                 la carpeta donde estás
taco ~/Descargas     otra carpeta
taco foto.png        su carpeta, con ese archivo elegido
taco --cwd-file F    al salir con q escribe en F dónde quedó (para hacer cd)
```

| tecla | qué hace |
|---|---|
| `j` `k` / flechas | moverse (`ctrl+d` / `ctrl+u` media página, `G` al final) |
| `h` `l` | subir / entrar |
| `enter` | abrir: texto en `$EDITOR` (nvim), lo demás con su app (`xdg-open`) |
| `o` · `e` | abrir con un comando · editar |
| `espacio` | marcar (`ctrl+a` todo, `esc` nada; clic derecho también marca) |
| `y` `x` `p` | copiar · cortar · pegar (en otro hilo, con barra de progreso) |
| `d` · `D` | a la papelera (la de freedesktop, la misma de GTK) · borrar para siempre |
| `r` · `a` | renombrar · nuevo (si termina en `/` es carpeta; `a/b/c.txt` crea todo) |
| `/` | filtrar la carpeta mientras escribes |
| `z` · `f` | saltar con zoxide · buscar un archivo con fzf |
| `g` + tecla | ir a: `h` home, `d` descargas, `D` documentos, `p` imágenes, `v` videos, `m` música, `c` .config, `t` papelera, `u` memorias, `r` / |
| `~` · `-` | home · la carpeta de antes |
| `M` | memorias USB y discos externos: montar y abrir, desmontar (`u`), expulsar (`e`) |
| `.` · `s` · `S` | ocultos sí/no · cambiar orden (nombre, fecha, tamaño, tipo) · al revés |
| `J` `K` | mover la vista previa |
| `c` · `C` | copiar la ruta · la de la carpeta |
| `!` | una terminal aquí (`exit` para volver) |
| `?` | todas las teclas |
| `q` · `Q` | salir (escribiendo `--cwd-file`) · salir |

Vista previa: texto, carpetas, imágenes (con el protocolo de kitty), video
(`ffmpegthumbnailer`), PDF (`pdftoppm`), SVG (`resvg`), lo que hay dentro de un
comprimido (`bsdtar`) y los datos de una canción (`ffprobe`). Las miniaturas
quedan en `~/.cache/taco`. Si falta alguno de esos programas, esa vista
previa no sale y ya.

Ratón, como en vato: clic elige, doble clic abre, clic derecho marca, la rueda
va donde esté el puntero (la lista o la vista previa), clic en la columna de
arriba sube, y las teclas de la línea de abajo se pueden picar.

Para que la terminal se quede en la carpeta donde saliste (zsh/bash):

```sh
t() { local f=$(mktemp); taco --cwd-file "$f" "$@"; cd "$(cat "$f")"; rm -f "$f"; }
```

## Instalar

```bash
cargo install --git https://github.com/daniiiqa/taco --root ~/.local --locked
```

El look sale de `~/.config/taco/look.json` o, si no existe, del de rola
(`~/.config/rola/look.json`), que cada look de discada ya trae. Las claves:
`borders` (auto, rounded, plain, double, thick, dashed, none), `bar` (line,
knob, blocks, dots, ascii), `labels` (icons pone iconos de Nerd Font; short o
words, solo texto con `/` en las carpetas), `case`, `select` (fill, bar,
inverse), `tint` (color, phosphor, ink) y `transparent`.

---

*Taco*: anything, nicely wrapped. A terminal file manager styled by each
discada look like rola and vato: three columns, previews (images in kitty,
video, PDF, archives), copy/move in the background, freedesktop trash, USB
mounting, zoxide and fzf. English or Spanish follows discada's language
setting. `?` inside taco lists every key.
