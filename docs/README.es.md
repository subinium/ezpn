<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn demo">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>Paneles de terminal, al instante.</strong><br>
  Multiplexor de terminal sin configuración con persistencia de sesión y teclas compatibles con tmux.
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <a href="README.ko.md">한국어</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <b>Español</b> | <a href="README.fr.md">Français</a>
</p>

---

## ¿Por qué ezpn?

```bash
$ ezpn -e 'npm run dev' -e 'npm test --watch' -e 'tail -f logs/app.log'
```

Tres paneles. Tres comandos. Una línea. Sin archivos de configuración, sin setup, sin curva de aprendizaje.

Las sesiones persisten en segundo plano — `Ctrl+B d` para separar, `ezpn a` para volver. Tus procesos siguen ejecutándose.

**Para equipos**, coloca `.ezpn.toml` en tu repo y todos obtienen el mismo espacio de trabajo:

```toml
[workspace]
layout = "7:3/1:1"

[[pane]]
name = "editor"
command = "nvim ."

[[pane]]
name = "server"
command = "npm run dev"
restart = "on_failure"

[[pane]]
name = "tests"
command = "npm test -- --watch"

[[pane]]
name = "logs"
command = "tail -f logs/app.log"
```

```bash
$ ezpn   # lee .ezpn.toml, inicia todo
```

Sin tmuxinator. Sin YAML. Solo un archivo TOML en tu repo.

## Instalación

```bash
cargo install ezpn
```

<details>
<summary>Compilar desde fuente</summary>

```bash
git clone https://github.com/subinium/ezpn
cd ezpn && cargo install --path .
```

</details>

## Inicio rápido

```bash
ezpn                  # 2 paneles (o carga .ezpn.toml)
ezpn 2 3              # Cuadrícula 2x3
ezpn -l dev           # Preset de diseño (dev, monitor, quad, stack, trio...)
ezpn -e 'cmd1' -e 'cmd2'   # Comandos por panel
```

### Sesiones

```bash
Ctrl+B d               # Separar (la sesión sigue ejecutándose)
ezpn a                 # Reconectar a la sesión más reciente
ezpn a myproject       # Reconectar por nombre
ezpn ls                # Listar sesiones activas
ezpn kill myproject    # Terminar una sesión
```

### Pestañas

```bash
Ctrl+B c               # Nueva pestaña
Ctrl+B n / p           # Siguiente / anterior pestaña
Ctrl+B 0-9             # Saltar a pestaña por número
```

Todas las teclas de tmux funcionan — `Ctrl+B %` para dividir, `Ctrl+B x` para cerrar, `Ctrl+B [` para modo copia.

## Características

| | |
|---|---|
| **Sin configuración** | Funciona de inmediato. Sin archivos rc. |
| **Presets de diseño** | `dev`, `ide`, `monitor`, `quad`, `stack`, `main`, `trio` |
| **Persistencia de sesión** | Separar/conectar como tmux. Daemon en segundo plano mantiene los procesos. |
| **Pestañas** | Ventanas estilo tmux con barra de pestañas y clic para cambiar. |
| **Prioridad al ratón** | Clic para enfocar, arrastrar para redimensionar, scroll para historial, arrastrar para seleccionar y copiar. |
| **Modo copia** | Teclas Vi, selección visual, búsqueda incremental, portapapeles OSC 52. |
| **Paleta de comandos** | `Ctrl+B :` con comandos compatibles con tmux. |
| **Modo broadcast** | Escribir en todos los paneles simultáneamente. |
| **Configuración de proyecto** | `.ezpn.toml` — diseño, comandos, variables de entorno, auto-reinicio. |
| **Modo sin bordes** | `ezpn -b none` para maximizar el espacio de pantalla. |
| **Teclado Kitty** | `Shift+Enter`, `Ctrl+Arrow` y teclas modificadas funcionan correctamente. |
| **CJK/Unicode** | Cálculo preciso de ancho para coreano, chino, japonés y emoji. |

## Presets de diseño

```bash
ezpn -l dev       # 7:3 — principal + lateral
ezpn -l ide       # 7:3/1:1 — editor + barra lateral + 2 inferiores
ezpn -l monitor   # 1:1:1 — 3 columnas iguales
ezpn -l quad      # Cuadrícula 2x2
ezpn -l stack     # 1/1/1 — 3 filas apiladas
ezpn -l main      # 6:4/1 — par superior ancho + inferior completo
ezpn -l trio      # 1/1:1 — superior completo + 2 inferiores
```

Proporciones personalizadas: `ezpn -l '7:3/5:5'`

## Configuración de proyecto

Coloca `.ezpn.toml` en la raíz del proyecto y ejecuta `ezpn`. Eso es todo.

**Opciones por panel:** `command`, `cwd`, `name`, `env`, `restart` (`never`/`on_failure`/`always`), `shell`

```bash
ezpn init              # Generar plantilla .ezpn.toml
ezpn from Procfile     # Importar desde Procfile
```

<details>
<summary>Configuración global</summary>

`~/.config/ezpn/config.toml`:

```toml
border = rounded        # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b              # tecla prefijo (Ctrl+<key>)
```

</details>

## Atajos de teclado

**Atajos directos:**

| Tecla | Acción |
|---|---|
| `Ctrl+D` | Dividir horizontal |
| `Ctrl+E` | Dividir vertical |
| `Ctrl+N` | Siguiente panel |
| `F2` | Igualar tamaños |

**Modo prefijo** (`Ctrl+B`, luego):

| Tecla | Acción |
|---|---|
| `%` / `"` | Dividir H / V |
| `o` / Arrow | Navegar paneles |
| `x` | Cerrar panel |
| `z` | Alternar zoom |
| `R` | Modo redimensionar |
| `[` | Modo copia |
| `B` | Broadcast |
| `:` | Paleta de comandos |
| `d` | Separar |
| `?` | Ayuda |

<details>
<summary>Referencia completa de atajos</summary>

**Pestañas:**

| Tecla | Acción |
|---|---|
| `Ctrl+B c` | Nueva pestaña |
| `Ctrl+B n` / `p` | Siguiente / anterior pestaña |
| `Ctrl+B 0-9` | Saltar a pestaña por número |
| `Ctrl+B ,` | Renombrar pestaña |
| `Ctrl+B &` | Cerrar pestaña |

**Paneles:**

| Tecla | Acción |
|---|---|
| `Ctrl+B {` / `}` | Intercambiar con anterior / siguiente |
| `Ctrl+B E` / `Space` | Igualar |
| `Ctrl+B s` | Alternar barra de estado |
| `Ctrl+B q` | Números de panel + salto rápido |

**Modo copia** (`Ctrl+B [`):

| Tecla | Acción |
|---|---|
| `h` `j` `k` `l` | Mover cursor |
| `w` / `b` | Siguiente / anterior palabra |
| `0` / `$` / `^` | Inicio / fin / primer carácter no blanco |
| `g` / `G` | Inicio / final del scrollback |
| `Ctrl+U` / `Ctrl+D` | Media página arriba / abajo |
| `v` | Selección de caracteres |
| `V` | Selección de líneas |
| `y` / `Enter` | Copiar y salir |
| `/` / `?` | Buscar adelante / atrás |
| `n` / `N` | Siguiente / anterior coincidencia |
| `q` / `Esc` | Salir |

**Ratón:**

| Acción | Efecto |
|---|---|
| Clic en panel | Enfocar |
| Doble clic | Alternar zoom |
| Clic en pestaña | Cambiar pestaña |
| Clic en `[x]` | Cerrar panel |
| Arrastrar borde | Redimensionar |
| Arrastrar texto | Seleccionar + copiar |
| Rueda de scroll | Historial de scrollback |

**Nota macOS:** Alt+Arrow para navegación direccional requiere configurar Option como Meta (iTerm2: Preferences > Profiles > Keys > `Esc+`).

</details>

<details>
<summary>Comandos de la paleta</summary>

`Ctrl+B :` abre el prompt de comandos. Todos los alias de tmux son compatibles.

```
split / split-window         Dividir horizontalmente
split -v                     Dividir verticalmente
new-tab / new-window         Nueva pestaña
next-tab / prev-tab          Cambiar pestañas
close-pane / kill-pane       Cerrar panel
close-tab / kill-window      Cerrar pestaña
rename-tab <name>            Renombrar pestaña
layout <spec>                Cambiar diseño
equalize / even              Igualar tamaños
zoom                         Alternar zoom
broadcast                    Alternar broadcast
```

</details>

## ezpn vs. tmux vs. Zellij

| | tmux | Zellij | **ezpn** |
|---|---|---|---|
| Configuración | Requiere `.tmux.conf` | Config KDL | **Sin configuración** |
| Primer uso | Pantalla vacía | Modo tutorial | **`ezpn`** |
| Sesiones | `tmux a` | `zellij a` | **`ezpn a`** |
| Config de proyecto | tmuxinator (gem) | — | **`.ezpn.toml` integrado** |
| Broadcast | `:setw synchronize-panes` | — | **`Ctrl+B B`** |
| Auto-reinicio | — | — | **`restart = "always"`** |
| Teclado Kitty | No | Sí | **Sí** |
| Plugins | — | WASM | — |
| Ecosistema | Masivo (30 años) | Creciendo | Nuevo |

**ezpn** — división de terminal sin configuración.
**tmux** — cuando necesitas scripting profundo y ecosistema de plugins.
**Zellij** — cuando quieres UI moderna con plugins WASM.

## Referencia CLI

```
ezpn [ROWS COLS]         Iniciar con diseño de cuadrícula
ezpn -l <PRESET>         Iniciar con preset de diseño
ezpn -e <CMD> [-e ...]   Comandos por panel
ezpn -S <NAME>           Sesión con nombre
ezpn -b <STYLE>          Estilo de borde (single/rounded/heavy/double/none)
ezpn a [NAME]            Conectar a sesión
ezpn ls                  Listar sesiones
ezpn kill [NAME]         Terminar sesión
ezpn rename OLD NEW      Renombrar sesión
ezpn init                Generar plantilla .ezpn.toml
ezpn from <FILE>         Importar desde Procfile
```

## Licencia

[MIT](../LICENSE)
