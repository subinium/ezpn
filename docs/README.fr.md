<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn en action">
</p>

# ezpn

Divisez votre terminal en une commande. Cliquez, glissez, c'est fait.

[![License](https://img.shields.io/badge/license-MIT-blue)](../LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.2.0-orange)](https://crates.io/crates/ezpn)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)]()

[English](../README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md) | [Español](README.es.md) | **Français**

## Installation

```bash
cargo install ezpn
```

## Utilisation

```bash
ezpn              # 2 panneaux cote a cote
ezpn 4            # 4 panneaux horizontaux
ezpn 3 -d v       # 3 panneaux verticaux
ezpn 2 3          # grille 2x3
ezpn --layout '7:3/1:1'   # disposition avec ratios
ezpn -e 'make watch' -e 'npm dev'   # commande par panneau
```

## Controles

**Souris :** Cliquez pour selectionner / `x` pour fermer / Glissez le bord pour redimensionner / Molette

**Clavier :**

| Touche | Action |
|---|---|
| `Ctrl+D` | Diviser gauche \| droite |
| `Ctrl+E` | Diviser haut / bas |
| `Ctrl+N` | Panneau suivant |
| `Ctrl+G` | Panneau de reglages |
| `Ctrl+W` | Quitter |

**Touches compatibles tmux (`Ctrl+B` puis) :**

| Touche | Action |
|---|---|
| `%` | Diviser gauche \| droite |
| `"` | Diviser haut / bas |
| `o` | Panneau suivant |
| `Arrow` | Navigation directionnelle |
| `x` | Fermer le panneau |
| `[` | Mode defilement (j/k/g/G, q pour quitter) |
| `d` | Quitter (avec confirmation) |

## Fonctionnalites

- **Dispositions flexibles** — Grille, ratios, division libre, redimensionnement par glissement
- **Commande par panneau** — `-e` pour lancer des commandes differentes
- **Boutons de barre de titre** — `[━] [┃] [×]` cliquez pour diviser/fermer
- **Touches tmux** — `Ctrl+B` suivi des touches tmux standard
- **Controle IPC** — `ezpn-ctl` pour l'automatisation
- **Instantanes d'espace de travail** — `ezpn-ctl save/load`

## Comparaison

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| Config | `.tmux.conf` | Fichiers KDL | Flags CLI |
| Diviser | `Ctrl+B %` | Changement de mode | `Ctrl+D` / clic |
| Detacher | Oui | Oui | Non |

Utilisez tmux/Zellij pour la persistance de session. Utilisez ezpn pour diviser rapidement votre terminal.

## Licence

[MIT](../LICENSE)
