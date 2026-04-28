<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn démo">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>Panneaux de terminal, instantanément.</strong><br>
  Multiplexeur de terminal sans configuration avec persistance de session et touches compatibles tmux.
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <a href="README.ko.md">한국어</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <b>Français</b>
</p>

---

## Pourquoi ezpn ?

```bash
$ ezpn                # divisez votre terminal, instantanément
$ ezpn 2 3            # grille 2x3 de shells
$ ezpn -l dev         # preset de disposition
```

Pas de fichiers de configuration, pas de setup, pas de courbe d'apprentissage. Les sessions persistent en arrière-plan — `Ctrl+B d` pour détacher, `ezpn a` pour revenir.

**Dans un projet**, placez `.ezpn.toml` dans votre dépôt et lancez `ezpn` — tout le monde obtient le même espace de travail :

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
$ ezpn   # lit .ezpn.toml, lance tout
```

Pas de tmuxinator. Pas de YAML. Juste un fichier TOML dans votre dépôt.

## Installation

```bash
cargo install ezpn
```

<details>
<summary>Compiler depuis les sources</summary>

```bash
git clone https://github.com/subinium/ezpn
cd ezpn && cargo install --path .
```

</details>

## Démarrage rapide

```bash
ezpn                  # 2 panneaux (ou charge .ezpn.toml)
ezpn 2 3              # Grille 2x3
ezpn -l dev           # Preset de disposition (dev, monitor, quad, stack, trio...)
ezpn -e 'cmd1' -e 'cmd2'   # Commandes par panneau
```

### Sessions

```bash
Ctrl+B d               # Détacher (la session continue)
ezpn a                 # Reconnecter à la session la plus récente
ezpn a myproject       # Reconnecter par nom
ezpn ls                # Lister les sessions actives
ezpn kill myproject    # Terminer une session
```

### Onglets

```bash
Ctrl+B c               # Nouvel onglet
Ctrl+B n / p           # Onglet suivant / précédent
Ctrl+B 0-9             # Aller à l'onglet par numéro
```

Toutes les touches tmux fonctionnent — `Ctrl+B %` pour diviser, `Ctrl+B x` pour fermer, `Ctrl+B [` pour le mode copie.

## Fonctionnalités

| | |
|---|---|
| **Zéro configuration** | Fonctionne immédiatement. Aucun fichier rc nécessaire. |
| **Presets de disposition** | `dev`, `ide`, `monitor`, `quad`, `stack`, `main`, `trio` |
| **Persistance de session** | Détacher/attacher comme tmux. Daemon en arrière-plan. |
| **Onglets** | Fenêtres style tmux avec barre d'onglets et clic souris. |
| **Souris d'abord** | Clic pour cibler, glisser pour redimensionner, molette pour l'historique, glisser pour sélectionner et copier. |
| **Mode copie** | Touches Vi, sélection visuelle, recherche incrémentale, presse-papiers OSC 52. |
| **Palette de commandes** | `Ctrl+B :` avec commandes compatibles tmux. |
| **Mode broadcast** | Saisir dans tous les panneaux simultanément. |
| **Configuration projet** | `.ezpn.toml` — disposition, commandes, variables d'env, redémarrage auto. |
| **Mode sans bordure** | `ezpn -b none` pour maximiser l'espace d'écran. |
| **Clavier Kitty** | `Shift+Enter`, `Ctrl+Arrow` et touches modifiées fonctionnent correctement. |
| **CJK/Unicode** | Calcul précis de largeur pour coréen, chinois, japonais et emoji. |

## Presets de disposition

```bash
ezpn -l dev       # 7:3 — principal + latéral
ezpn -l ide       # 7:3/1:1 — éditeur + barre latérale + 2 en bas
ezpn -l monitor   # 1:1:1 — 3 colonnes égales
ezpn -l quad      # Grille 2x2
ezpn -l stack     # 1/1/1 — 3 rangées empilées
ezpn -l main      # 6:4/1 — paire supérieure large + bas complet
ezpn -l trio      # 1/1:1 — haut complet + 2 en bas
```

Proportions personnalisées : `ezpn -l '7:3/5:5'`

## Configuration projet

Placez `.ezpn.toml` à la racine du projet et lancez `ezpn`. C'est tout.

**Options par panneau :** `command`, `cwd`, `name`, `env`, `restart` (`never`/`on_failure`/`always`), `shell`

```bash
ezpn init              # Générer un modèle .ezpn.toml
ezpn from Procfile     # Importer depuis Procfile
```

<details>
<summary>Configuration globale</summary>

`~/.config/ezpn/config.toml` :

```toml
border = rounded        # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b              # touche préfixe (Ctrl+<key>)
```

</details>

## Raccourcis clavier

**Raccourcis directs :**

| Touche | Action |
|---|---|
| `Ctrl+D` | Diviser horizontalement |
| `Ctrl+E` | Diviser verticalement |
| `Ctrl+N` | Panneau suivant |
| `F2` | Égaliser les tailles |

**Mode préfixe** (`Ctrl+B`, puis) :

| Touche | Action |
|---|---|
| `%` / `"` | Diviser H / V |
| `o` / Arrow | Naviguer les panneaux |
| `x` | Fermer le panneau |
| `z` | Basculer le zoom |
| `R` | Mode redimensionnement |
| `[` | Mode copie |
| `B` | Broadcast |
| `:` | Palette de commandes |
| `d` | Détacher |
| `?` | Aide |

<details>
<summary>Référence complète des raccourcis</summary>

**Onglets :**

| Touche | Action |
|---|---|
| `Ctrl+B c` | Nouvel onglet |
| `Ctrl+B n` / `p` | Onglet suivant / précédent |
| `Ctrl+B 0-9` | Aller à l'onglet par numéro |
| `Ctrl+B ,` | Renommer l'onglet |
| `Ctrl+B &` | Fermer l'onglet |

**Panneaux :**

| Touche | Action |
|---|---|
| `Ctrl+B {` / `}` | Échanger avec précédent / suivant |
| `Ctrl+B E` / `Space` | Égaliser |
| `Ctrl+B s` | Basculer la barre d'état |
| `Ctrl+B q` | Numéros de panneau + saut rapide |

**Mode copie** (`Ctrl+B [`) :

| Touche | Action |
|---|---|
| `h` `j` `k` `l` | Déplacer le curseur |
| `w` / `b` | Mot suivant / précédent |
| `0` / `$` / `^` | Début / fin / premier non-blanc |
| `g` / `G` | Haut / bas du scrollback |
| `Ctrl+U` / `Ctrl+D` | Demi-page haut / bas |
| `v` | Sélection de caractères |
| `V` | Sélection de lignes |
| `y` / `Enter` | Copier et quitter |
| `/` / `?` | Chercher avant / arrière |
| `n` / `N` | Correspondance suivante / précédente |
| `q` / `Esc` | Quitter |

**Souris :**

| Action | Effet |
|---|---|
| Clic sur panneau | Cibler |
| Double-clic | Basculer le zoom |
| Clic sur onglet | Changer d'onglet |
| Clic sur `[x]` | Fermer le panneau |
| Glisser la bordure | Redimensionner |
| Glisser le texte | Sélectionner + copier |
| Molette | Historique de scrollback |

**Note macOS :** Alt+Arrow pour la navigation directionnelle nécessite de configurer Option comme Meta (iTerm2 : Preferences > Profiles > Keys > `Esc+`).

</details>

<details>
<summary>Commandes de la palette</summary>

`Ctrl+B :` ouvre l'invite de commande. Tous les alias tmux sont supportés.

```
split / split-window         Diviser horizontalement
split -v                     Diviser verticalement
new-tab / new-window         Nouvel onglet
next-tab / prev-tab          Changer d'onglet
close-pane / kill-pane       Fermer le panneau
close-tab / kill-window      Fermer l'onglet
rename-tab <name>            Renommer l'onglet
layout <spec>                Changer la disposition
equalize / even              Égaliser les tailles
zoom                         Basculer le zoom
broadcast                    Basculer le broadcast
```

</details>

## Pourquoi ezpn (vs. tmux)

Trois affirmations mesurables. Vérifiez-les sur votre propre charge
de travail avant de leur faire confiance.

| Axe | tmux 3.4 | **ezpn 0.12** | Comment c'est mesuré |
| --- | --- | --- | --- |
| RSS au repos (16 panneaux, 50 MB de scrollback total, Linux 6.6) | ~180 MB | **~28 MB** | `ps -o rss= -p $(pgrep -d, tmux\|ezpn)` après 16 splits et 1 minute d'inactivité. |
| Fiabilité de `send-keys` | fire-and-forget ; pas de signal de fin | **`--await-prompt` bloque jusqu'à OSC 133 D** | `ezpn-ctl send-keys --await-prompt --timeout 60s -- 'cargo test\n'` — voir [scripting.md](scripting.md). |
| DECSET 2026 (sortie synchronisée) | transmis à l'émulateur hôte | **intercepté + bufferisé** ; une seule frame atomique aux clients | `printf '\e[?2026h…\e[?2026l'` avec deux clients connectés — les deux voient le même redessin atomique. |

Au-delà des chiffres :

- **Defaults zéro-config.** Toute touche tmux fonctionne sur une installation neuve. Pas de `.tmux.conf`, pas de gestionnaire de plugins.
- **TOML, pas un satellite YAML.** `.ezpn.toml` vit dans votre repo ; tout le monde partage le même workspace sans `gem install tmuxinator`.
- **Garde anti-injection OSC 52.** `cat hostile.log` ne peut pas écraser silencieusement votre presse-papiers ([clipboard.md](clipboard.md), [security.md](security.md)).
- **Protocole gelé.** [`docs/protocol/v1.md`](protocol/v1.md) engage SemVer sur la surface IPC — vos scripts ne cassent pas entre des minor bumps.

Compromis à peser avant de migrer :

- Pas de système de plugins. L'écosystème de tmux a 10+ ans ; celui d'ezpn est vide.
- Pas de `pipe-pane`, `command-alias`, `if-shell`. Utilisez `[[hooks]]` et le bus d'événements.
- Linux et macOS uniquement. Pas de Windows.

Guide de migration complet : [docs/migration-from-tmux.md](migration-from-tmux.md).

## ezpn vs. tmux vs. Zellij

| | tmux | Zellij | **ezpn** |
|---|---|---|---|
| Configuration | `.tmux.conf` requis | Config KDL | **Zéro configuration** |
| Premier usage | Écran vide | Mode tutoriel | **`ezpn`** |
| Sessions | `tmux a` | `zellij a` | **`ezpn a`** |
| Config projet | tmuxinator (gem) | — | **`.ezpn.toml` intégré** |
| Broadcast | `:setw synchronize-panes` | — | **`Ctrl+B B`** |
| Auto-redémarrage | — | — | **`restart = "always"`** |
| Clavier Kitty | Non | Oui | **Oui** |
| Plugins | — | WASM | — |
| Écosystème | Massif (30 ans) | En croissance | Nouveau |

**ezpn** — division de terminal sans configuration.
**tmux** — quand vous avez besoin de scripting avancé et d'un écosystème de plugins.
**Zellij** — quand vous voulez une UI moderne avec des plugins WASM.

## Référence CLI

```
ezpn [ROWS COLS]         Démarrer avec une grille
ezpn -l <PRESET>         Démarrer avec un preset
ezpn -e <CMD> [-e ...]   Commandes par panneau
ezpn -S <NAME>           Session nommée
ezpn -b <STYLE>          Style de bordure (single/rounded/heavy/double/none)
ezpn a [NAME]            Connecter à une session
ezpn ls                  Lister les sessions
ezpn kill [NAME]         Terminer une session
ezpn rename OLD NEW      Renommer une session
ezpn init                Générer un modèle .ezpn.toml
ezpn from <FILE>         Importer depuis Procfile
```

## Documentation

- [Démarrage](getting-started.md) — visite de 5 minutes
- [Migrer depuis tmux](migration-from-tmux.md) — touche par touche, commande par commande
- [Configuration](configuration.md) — référence complète de `config.toml` + `.ezpn.toml`
- [Scripting](scripting.md) — `ezpn-ctl`, événements, `ls --json`
- [Presse-papiers](clipboard.md) — OSC 52, chaîne de fallback, piège SSH
- [Protocole terminal](terminal-protocol.md) — ce qu'ezpn transmet / intercepte / modifie
- [Sécurité](security.md) — modèle de menaces et defaults
- [Protocole IPC v1](protocol/v1.md) — gelé en v1.0

## Licence

[MIT](../LICENSE)
