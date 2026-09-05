<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn demo">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>Paneles de terminal, al instante.</strong><br>
  Multiplexor de terminal para macOS y Linux, cómodo con el ratón, con sesiones persistentes y teclas de prefijo familiares.
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

## Empieza a trabajar

```sh
cargo install ezpn --locked
ezpn                 # two shells
ezpn 2 3             # a 2-by-3 grid
ezpn -S work         # create or reattach to a named session
```

Para compilar se requiere Rust 1.88 o posterior. [GitHub Releases](https://github.com/subinium/ezpn/releases)
ofrece binarios para macOS y Linux; verifica las sumas de comprobación cuando estén disponibles.
ezpn es un multiplexor de terminal ejecutable, no una biblioteca GUI integrable en Rust.

## Sesiones y SSH

```sh
ezpn a work
ezpn a work --shared
ezpn a work --readonly
ezpn ls
ezpn kill work
```

Pulsa `Ctrl+B` y después `d` para desconectar solo tu cliente. Los procesos de shell siguen vivos,
incluidos los trabajos de las pestañas inactivas. Al volver a conectarte, recuperas esos mismos procesos.
Los clientes de solo lectura no pueden introducir texto ni redimensionar el espacio de trabajo de los clientes con permiso de escritura.

Instala ezpn en el host remoto y asegúrate de que esté en su PATH:

```sh
ssh -t host 'ezpn -S work'
ssh -t host 'ezpn a work'
ssh -J bastion -t host 'ezpn a work'
```

SSH debe asignar una PTY. Desconectar el cliente SSH no termina el demonio remoto.
El cifrado, la autenticación, la verificación de claves de host y el reenvío son responsabilidad de OpenSSH.
No expongas los sockets Unix locales de ezpn a una red sin autenticación.

## Ratón y teclado

| Interacción | Resultado |
| --- | --- |
| Clic en el contenido de un panel | Enfocar el panel |
| Arrastrar un separador | Redimensionar la división |
| Botones de división de la barra de título | Dividir el panel seleccionado |
| Botón de cierre de la barra de título | Pedir confirmación antes de cerrar |
| Clic en una pestaña | Cambiar de pestaña |
| Desplazar la rueda | Recorrer el historial o reenviar a una aplicación que use el ratón |
| Arrastrar texto | Seleccionar y copiar |
| Shift + arrastrar | Seleccionar texto de ezpn en lugar de enviar eventos de ratón a la aplicación |
| Doble clic en contenido de una aplicación que no use el ratón | Alternar el zoom |
| F1 / F2 | Ajustes / igualar tamaños |
| Alt + flechas | Navegar entre paneles; configura Option como Meta en macOS |

Los clics, movimientos, eventos de rueda y liberaciones de botones se envían con la codificación de ratón
que solicita la aplicación. Teclas como `Ctrl+D`, `Ctrl+E` y `Ctrl+W` se pasan al shell,
salvo que se reasignen explícitamente. Ya no dividen paneles ni solicitan el cierre.

Pulsa `Ctrl+B` y, a continuación:

| Tecla | Acción |
| --- | --- |
| `%` / `"` | Dividir en columnas / filas |
| `o` / flechas | Navegar entre paneles |
| `x` | Confirmar el cierre del panel |
| `z` | Alternar el zoom |
| `R` | Modo de redimensionado |
| `Space` / `E` | Igualar tamaños |
| `c` / `n` / `p` | Nueva pestaña / siguiente / anterior |
| `0`–`9` | Seleccionar pestaña por índice, empezando en cero |
| `,` / `&` | Renombrar / confirmar el cierre de la pestaña |
| `[` | Modo copia |
| `:` | Paleta de comandos |
| `r` | Recargar la configuración global |
| `B` | Alternar la entrada simultánea en varios paneles |
| `d` | Desconectar este cliente |
| `?` | Ayuda |
| `Ctrl+B` | Enviar la tecla de prefijo a la aplicación |

El modo copia admite navegación vi, selección con `v`/`V`, copia con `y` o Enter,
búsqueda con `/`/`?`, coincidencia siguiente/anterior con `n`/`N` y salida con `q`/Escape.
Admitir algunos atajos habituales de tmux no implica compatibilidad completa con sus comandos.

## Cambiar la distribución sin perder el trabajo

```sh
ezpn -l dev       # 7:3
ezpn -l ide       # 7:3/1:1
ezpn -l quad      # 2-by-2
ezpn -l '7:3/5:5'
ezpn -b none
```

En la paleta de comandos, `select-layout` reorganiza los procesos existentes.
Rechaza distribuciones con un número distinto de paneles; divide o cierra los paneles explícitamente.
Un fallo al dividir o cargar una instantánea no destruye el espacio de trabajo actual.

## Espacios de trabajo de proyectos de confianza

Revisa los comandos del repositorio antes de autorizar su ejecución al iniciar:

```toml
# .ezpn.toml
[workspace]
layout = "7:3"

[[pane]]
name = "shell"
cwd = "."

[[pane]]
name = "worker"
command = "printf 'ready\\n'; exec sh"
restart = "on_failure"
```

```sh
ezpn init
ezpn doctor
ezpn --trust-project
```

`--trust-project` autoriza la ejecución automática de `.ezpn.toml` / Procfile.
Para iniciar shells normales sin cargar comandos del repositorio, indica una cuadrícula explícita, como `ezpn 1 2`.
`doctor` comprueba la sintaxis en modo de solo lectura, sin ejecutar comandos ni resolver secretos.

La interpolación de variables del proyecto admite referencias al entorno, archivos y secretos.
Los valores externos no se imprimen en los diagnósticos. Los paneles cuya configuración lee valores externos
se excluyen de los metadatos de ejecución y del historial de las instantáneas: al restaurarlos, se abren shells limpios.
Esta política prioriza deliberadamente la privacidad frente a guardar credenciales resueltas sin advertirlo.
Consulta [configuración](../docs/configuration.md) y [seguridad](../docs/security.md).

## Configuración y recuperación

```toml
# ~/.config/ezpn/config.toml
[global]
border = "rounded"
scrollback = 10000
persist_scrollback = false

[keys]
prefix = "b"

[theme]
name = "ezpn-dark"
```

Temas: `ezpn-dark`, `ezpn-light`, `nord`, `gruvbox-dark`, `solarized-dark`.
Los mapas de teclas del usuario se definen en `[keymap.normal]`, `[keymap.prefix]` y `[keymap.copy_mode]`.
`Ctrl+B r` recarga los campos admitidos a partir de una única lectura validada del archivo.
Si falla el guardado en el panel de ajustes, se informa del error en vez de afirmar que se guardó correctamente.

Una instantánea en disco no es lo mismo que una sesión activa con el cliente desconectado.
`ezpn --restore FILE` **inicia procesos nuevos**. El historial guardado de forma opcional se restaura como texto,
no como un editor en ejecución, memoria de procesos, gráficos del terminal ni el estado exacto de la pantalla alternativa.
Las instantáneas tienen límites de tamaño y descompresión, y permisos de acceso restringidos.

## Compatibilidad y pruebas

- El soporte se limita a macOS y Linux con un terminal ANSI UTF-8 y PTY Unix.
  Windows nativo no está soportado.
- La negociación del teclado de la aplicación hija es distinta de las capacidades del host.
  Las aplicaciones tradicionales reciben secuencias tradicionales; las extensiones Kitty admitidas son opcionales.
- Las escrituras de aplicaciones en el portapapeles están sujetas a la política OSC 52 configurada.
  Por SSH, las copias del usuario prefieren el terminal conectado antes que el portapapeles del escritorio remoto.
- El renderizado está acotado y recorta las vistas pequeñas. Consulta [compatibilidad de terminales](../docs/terminal-protocol.md)
  para conocer los límites del analizador, las secuencias admitidas y las combinaciones de emuladores GUI sin probar.
- `--features render-diff` activa una ruta opcional y acotada de diferencias ANSI.
  Los fotogramas no admitidos vuelven a la salida original. No garantiza una mejora de velocidad universal.
- Las pruebas con PTY reales cubren conexión/desconexión, redimensionado, clientes compartidos/de solo lectura y transportes interrumpidos.
  Una prueba SSH aislada mediante la interfaz de bucle local distingue SSH real de una simulación.
- Las pruebas prolongadas de estabilidad y las comparaciones de rendimiento con tmux/Zellij son evidencias separadas.
  No se afirma que ezpn sea siempre más rápido ni consuma menos memoria que esos proyectos.

La [auditoría de la versión](../docs/audits/v0.14.0.md) recoge los resultados y las limitaciones pendientes.
El [script de comprobación previa](../scripts/preflight.py) registra PASS/FAIL/SKIP y los códigos de salida reales;
las pruebas fallidas no se ocultan con marcadores de posición ignorados.

## Documentación

[Primeros pasos](../docs/getting-started.md) · [Configuración](../docs/configuration.md) ·
[SSH y protocolos de terminal](../docs/terminal-protocol.md) · [Portapapeles](../docs/clipboard.md) ·
[Seguridad](../docs/security.md) · [Límites de scripting](../docs/scripting.md) ·
[Contribuir](../CONTRIBUTING.md) · [Registro de cambios](../CHANGELOG.md)

## Licencia

[MIT](../LICENSE)
