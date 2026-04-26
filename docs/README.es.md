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
  <a href="https://github.com/subinium/ezpn/actions/workflows/gitleaks.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/gitleaks.yml?style=flat-square&label=gitleaks" alt="gitleaks"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/supply-chain.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/supply-chain.yml?style=flat-square&label=audit" alt="audit"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <a href="README.ko.md">한국어</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <b>Español</b> | <a href="README.fr.md">Français</a>
</p>

---

## ¿Por qué ezpn?

```bash
$ ezpn                # divide tu terminal, al instante
$ ezpn 2 3            # cuadrícula 2x3 de shells
$ ezpn -l dev         # preset de diseño
```

Sin archivos de configuración, sin setup, sin curva de aprendizaje. Las sesiones persisten en segundo plano — `Ctrl+B d` para separar, `ezpn a` para volver.

**En un proyecto**, coloca `.ezpn.toml` en tu repo y ejecuta `ezpn` — todos obtienen el mismo espacio de trabajo:

```toml
[session]
name = "myproject"           # fija el nombre de la sesión (las colisiones se vuelven myproject-1, -2...)

[workspace]
layout = "7:3/1:1"
persist_scrollback = true    # el scrollback sobrevive a desconectar/reconectar

[[pane]]
name = "editor"
command = "nvim ."

[[pane]]
name = "server"
command = "npm run dev"
restart = "on_failure"
env = { NODE_ENV = "${env:NODE_ENV}", DB_URL = "${file:.env.local}" }

[[pane]]
name = "tests"
command = "npm test -- --watch"

[[pane]]
name = "logs"
command = "tail -f logs/app.log"
```

```bash
$ ezpn         # lee .ezpn.toml, inicia todo
$ ezpn doctor  # valida la interpolación de env + referencias a secretos antes de ejecutar
```

Sin tmuxinator. Sin YAML. Solo un archivo TOML en tu repo.

## Instalación

```bash
cargo install ezpn
```

O descarga un binario precompilado desde el [último release](https://github.com/subinium/ezpn/releases/latest) — `ezpn-x86_64-unknown-linux-gnu.tar.gz`, `ezpn-x86_64-apple-darwin.tar.gz` o `ezpn-aarch64-apple-darwin.tar.gz`.

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
Ctrl+B d               # Desconectar (la sesión sigue ejecutándose)
ezpn a                 # Reconectar a la sesión más reciente
ezpn a myproject       # Reconectar por nombre
ezpn ls                # Listar sesiones activas
ezpn kill myproject    # Terminar una sesión
ezpn --new             # Forzar una nueva sesión aunque ya exista una para $PWD
```

Los nombres de sesión por defecto son `basename($PWD)`. Las colisiones se resuelven de forma determinista — `repo` → `repo-1` → `repo-2` (los sockets muertos se limpian durante el escaneo). Fija un nombre en `.ezpn.toml` mediante `[session].name = "..."`.

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
| **Persistencia de sesión** | Desconectar/conectar como tmux. Daemon en segundo plano mantiene los procesos. Reconexión en frío inferior a 50 ms. |
| **Persistencia de scrollback** | `persist_scrollback` opcional sobrevive a desconectar/reconectar (gzip+bincode en snapshots v3). |
| **Pestañas** | Ventanas estilo tmux con barra de pestañas y clic para cambiar. |
| **Prioridad al ratón** | Clic para enfocar, arrastrar para redimensionar, scroll para historial, arrastrar para seleccionar y copiar. |
| **Modo copia** | Teclas Vi, selección visual, búsqueda incremental por ancho de visualización, portapapeles OSC 52. |
| **Paleta de comandos** | `Ctrl+B :` con comandos compatibles con tmux. |
| **Modo broadcast** | Escribir en todos los paneles simultáneamente. |
| **Configuración de proyecto** | `.ezpn.toml` por proyecto — diseño, comandos, variables de entorno, auto-reinicio. |
| **Interpolación de env** | `${HOME}`, `${env:VAR}`, `${file:.env.local}`, `${secret:keychain:KEY}` en el env de los paneles. |
| **Temas** | Paleta TOML + 4 incorporados (`tokyo-night`, `gruvbox-dark`, `solarized-dark`/`-light`). |
| **Recarga en caliente** | `Ctrl+B r` recarga `~/.config/ezpn/config.toml` sin desconectar. |
| **Modo sin bordes** | `ezpn -b none` para maximizar el espacio de pantalla. |
| **Teclado Kitty** | `Shift+Enter`, `Ctrl+Arrow`, Alt+Char (CSI u / RFC 3665) — las teclas modificadas funcionan correctamente. |
| **CJK/Unicode** | Cálculo preciso de ancho para coreano, chino, japonés y emoji. |
| **Aislamiento de fallos** | Un panel que entra en pánico no puede tumbar al daemon (manejo seguro de señales SIGTERM/SIGCHLD). |
| **Entrada programable** | `ezpn-ctl send-keys --pane N -- 'cmd' Enter` — para editores, agentes de IA y scripts de CI. |
| **Stream de eventos** | Suscripciones `S_EVENT` de larga vida sobre el protocolo binario (integración estilo `-CC`). |
| **Hooks** | Configuración declarativa `[[hooks]]`: ejecuta shell ante eventos del daemon, con pool de workers y timeout por hook. |
| **Búsqueda regex** | `[copy_mode] search = "regex"` activa búsqueda con patrones POSIX y smart-case en modo copia. |
| **Historial por panel** | `ezpn-ctl clear-history --pane N` / `set-scrollback --pane N --lines L` para control en ejecución. |

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
ezpn doctor            # Validar config + interpolación de env, sale con código distinto de cero si faltan referencias
```

### Hooks

Ejecuta un comando shell ante eventos del daemon. Pool de workers de 4 hilos con `timeout_ms` por hook; cada proceso hijo se lanza en su propio grupo, así la escalada SIGTERM → SIGKILL alcanza todo el árbol.

```toml
# ~/.config/ezpn/config.toml o .ezpn.toml

[[hooks]]
event = "client-attached"
command = "notify-send 'pane {client_id} attached'"
shell = true
timeout_ms = 2000

[[hooks]]
event = "tab-created"
command = ["/usr/local/bin/ezpn-tab-init", "{name}", "{tab_index}"]
```

v0.11 cablea `client-attached`, `client-detached`, `tab-created`, `tab-closed`, `session-renamed`. La expansión de variables (`{session}`, `{client_id}`, `{pane_id}`, …) sustituye los valores por evento dentro de `command` antes del exec.

### Interpolación de env

Los valores de env por panel admiten cuatro formas de referencia:

```toml
[[pane]]
command = "npm run dev"
env = {
  HOME       = "${HOME}",                    # env del proceso
  NODE_ENV   = "${env:NODE_ENV}",            # env explícito
  DB_URL     = "${file:.env.local}",         # búsqueda en archivo estilo dotenv
  GH_TOKEN   = "${secret:keychain:GH_TOKEN}",# Llavero de macOS (Linux: secret-tool)
}
```

`.env.local` junto a `.ezpn.toml` se fusiona automáticamente y sobrescribe a `[env]`. `${secret:keychain:KEY}` retrocede a `${env:KEY}` con una advertencia cuando el llavero del sistema no está disponible. La recursión está limitada a una profundidad de 8 para detectar ciclos.

### Temas

```toml
# .ezpn.toml o ~/.config/ezpn/config.toml
theme = "tokyo-night"   # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
```

Los temas de usuario se cargan desde `~/.config/ezpn/themes/<name>.toml`. ezpn detecta automáticamente `$COLORTERM` / `$TERM` y degrada a 256 o 16 colores cuando truecolor no está soportado.

<details>
<summary>Configuración global (~/.config/ezpn/config.toml)</summary>

```toml
border = rounded            # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b                  # tecla prefijo (Ctrl+<key>)
theme = default             # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
persist_scrollback = false  # guarda el scrollback en los snapshots automáticos (desactivado por defecto)
```

Los cambios en el panel de ajustes (`Ctrl+B Shift+,`) se persisten de forma atómica. Recarga desde disco con `Ctrl+B r`.

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
| `r` | Recargar configuración |
| `d` | Desconectar sesión |
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

**ezpn** — división de terminal sin configuración + superficie de scripting `ezpn-ctl send-keys` / stream de eventos / hooks.
**tmux** — cuando necesitas un ecosistema de plugins profundo (TPM, etc.).
**Zellij** — cuando quieres plugins WASM.

## Referencia CLI

```
ezpn [ROWS COLS]         Iniciar con diseño de cuadrícula
ezpn -l <PRESET>         Iniciar con preset de diseño
ezpn -e <CMD> [-e ...]   Comandos por panel
ezpn -S <NAME>           Sesión con nombre
ezpn -b <STYLE>          Estilo de borde (single/rounded/heavy/double/none)
ezpn --new               Forzar una nueva sesión (omite la auto-conexión a la existente)
ezpn a [NAME]            Conectar a sesión
ezpn ls                  Listar sesiones
ezpn kill [NAME]         Terminar sesión
ezpn rename OLD NEW      Renombrar sesión
ezpn init                Generar plantilla .ezpn.toml
ezpn from <FILE>         Importar desde Procfile
ezpn doctor              Validar .ezpn.toml + interpolación de env
```

### `ezpn-ctl` (scripting)

```
ezpn-ctl list                                Listar paneles
ezpn-ctl split [horizontal|vertical] [PANE]  Dividir un panel
ezpn-ctl close PANE                          Cerrar un panel
ezpn-ctl focus PANE                          Enfocar un panel
ezpn-ctl save <PATH>                         Guardar instantánea del workspace
ezpn-ctl load <PATH>                         Restaurar workspace
ezpn-ctl exec PANE <CMD>                     Reemplazar un panel con un comando nuevo

ezpn-ctl send-keys [--pane N | --target current] [--literal] -- <key>...
                                             Enviar tokens de chord o bytes crudos al PTY del panel.
                                             Ejemplos:
                                               ezpn-ctl send-keys --pane 0 -- 'echo hi' Enter
                                               ezpn-ctl send-keys --target current -- C-c
                                               ezpn-ctl send-keys --pane 0 --literal -- $'#!/bin/sh\nexit 0\n'

ezpn-ctl clear-history --pane N              Descarta el scrollback por encima de la pantalla visible
ezpn-ctl set-scrollback --pane N --lines L   Cambia el tamaño del anillo de scrollback (limitado por scrollback_max_lines)
```

## Licencia

[MIT](../LICENSE)
