<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn en accion">
</p>

# ezpn

Divide tu terminal con un solo comando. Clic, arrastra, listo.

[![License](https://img.shields.io/badge/license-MIT-blue)](../LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.2.0-orange)](https://crates.io/crates/ezpn)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)]()

[English](../README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md) | **Español** | [Français](README.fr.md)

## Instalacion

```bash
cargo install ezpn
```

## Uso

```bash
ezpn              # 2 paneles lado a lado
ezpn 4            # 4 paneles horizontales
ezpn 3 -d v       # 3 paneles verticales
ezpn 2 3          # cuadricula 2x3
ezpn --layout '7:3/1:1'   # diseño con proporciones
ezpn -e 'make watch' -e 'npm dev'   # comando por panel
```

## Controles

**Raton:** Clic para seleccionar / `x` para cerrar / Arrastrar borde para redimensionar / Scroll

**Teclado:**

| Tecla | Accion |
|---|---|
| `Ctrl+D` | Dividir izquierda \| derecha |
| `Ctrl+E` | Dividir arriba / abajo |
| `Ctrl+N` | Panel siguiente |
| `Ctrl+G` | Panel de ajustes |
| `Ctrl+W` | Salir |

**Teclas compatibles con tmux (`Ctrl+B` seguido de):**

| Tecla | Accion |
|---|---|
| `%` | Dividir izquierda \| derecha |
| `"` | Dividir arriba / abajo |
| `o` | Panel siguiente |
| `Arrow` | Navegacion direccional |
| `x` | Cerrar panel |
| `[` | Modo scroll (j/k/g/G, q para salir) |
| `d` | Salir (con confirmacion) |

## Caracteristicas

- **Diseños flexibles** — Cuadricula, proporciones, division libre, redimensionar arrastrando
- **Comando por panel** — `-e` para lanzar comandos diferentes
- **Botones en barra de titulo** — `[━] [┃] [×]` clic para dividir/cerrar
- **Teclas tmux** — `Ctrl+B` seguido de teclas tmux estandar
- **Control IPC** — `ezpn-ctl` para automatizacion
- **Snapshots de espacio de trabajo** — `ezpn-ctl save/load`

## Comparacion

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| Config | `.tmux.conf` | Archivos KDL | Flags CLI |
| Dividir | `Ctrl+B %` | Cambio de modo | `Ctrl+D` / clic |
| Detach | Si | Si | No |

Usa tmux/Zellij si necesitas persistencia de sesion. Usa ezpn si solo quieres dividir terminales.

## Licencia

[MIT](../LICENSE)
