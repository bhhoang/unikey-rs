# unikey

[![Release](https://img.shields.io/github/v/release/bhhoang/unikey-rs)](https://github.com/bhhoang/unikey-rs/releases)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange?logo=rust)](https://www.rust-lang.org)
[![Wayland](https://img.shields.io/badge/Wayland-supported-yellow?logo=wayland)](#where-it-works)
[![X11](https://img.shields.io/badge/X11-supported-lightgrey?logo=xdotorg)](#where-it-works)

Vietnamese typing for Linux, like UniKey on Windows: no underlined text, works
in every app (browsers, Electron, terminals, Neovim...) without per-app setup.
Built on a Rust port of the original UniKey engine.

Supports Telex, VNI, VIQR, MS VI and Simple Telex, plus macros and spell check.

## Contents

- [Where it works](#where-it-works)
- [Install](#install)
  - [Arch Linux](#arch-linux)
  - [Debian / Ubuntu](#debian--ubuntu)
  - [Fedora](#fedora)
  - [openSUSE](#opensuse)
  - [AppImage](#appimage)
- [Start it with your session](#start-it-with-your-session)
- [Use](#use)
- [Uninstall](#uninstall)
- [License](#license)

## Where it works

| Desktop | Support |
|---|---|
| Hyprland, Sway, River, labwc, Wayfire, Niri | Full |
| Any X11 session | Full |
| GNOME / KDE on Wayland | X11 (XWayland) apps only |

## Install

The easiest way is the [AppImage](#appimage). To build from source, install
the build dependencies for your distribution, then build and install
with Cargo:

    cargo install --locked --git https://github.com/bhhoang/unikey-rs

This puts `unikey` and `unikey-settings` in `~/.cargo/bin`. Make sure that
folder is on your `PATH`.

The settings window needs GTK 4.14 and libadwaita 1.5 or newer.

### Arch Linux

    sudo pacman -S --needed rust pkgconf gtk4 libadwaita libxkbcommon dbus

### Debian / Ubuntu

Ubuntu 24.04+ or Debian 13+:

    sudo apt install cargo pkg-config libgtk-4-dev libadwaita-1-dev libxkbcommon-dev libdbus-1-dev

If the build complains that Rust is too old, install a newer one with
[rustup](https://rustup.rs).

### Fedora

Fedora 40+:

    sudo dnf install cargo pkgconf gtk4-devel libadwaita-devel libxkbcommon-devel dbus-devel

### openSUSE

Tumbleweed:

    sudo zypper install cargo pkg-config gtk4-devel libadwaita-devel libxkbcommon-devel dbus-1-devel

### AppImage

No build needed: download `unikey-*-x86_64.AppImage` from the
[latest release](https://github.com/bhhoang/unikey-rs/releases/latest). It runs
on Ubuntu 24.04, Debian 13, Fedora 40 or newer, and any current rolling distro.

    install -Dm755 unikey-*-x86_64.AppImage ~/.local/bin/unikey
    unikey              # start unikey
    unikey settings     # open the settings window

If it fails to start, install FUSE 2 (`libfuse2` / `fuse2` / `fuse-libs`).

## Start it with your session

Remove any other input method (fcitx5, ibus) from your autostart first, then
start `unikey` when you log in:

- **Hyprland** (`hyprland.conf`): `exec-once = unikey`
- **Sway** (`config`): `exec unikey`
- **Other desktops**: add `unikey` to your startup applications.

## Use

- **Ctrl+Shift** or **Alt+Z**: switch between Vietnamese (VI) and English (EN).
  You can change the key in the settings.
- **Tray icon**: shows VI/EN. Click to switch, double-click to open the
  settings, right-click for the menu.
- **`unikey-settings`**: pick the input method and the options.

The settings are saved in `~/.config/unikey/config`.

## Uninstall

    cargo uninstall unikey

Then remove `unikey` from your autostart.

## License

GPL-3.0-or-later.
