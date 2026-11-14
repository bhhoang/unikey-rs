# unikey

UniKey Vietnamese typing for Linux, written in Rust. The conversion engine is a
port of Pham Kim Long's UniKey engine (`ukengine`), brought up to date with the
fixes in fcitx5-unikey (`q`/`qu` validation, `uơ`/`ươ` rules) and checked key for
key against that C++ engine. It also follows UniKey for Windows up to 4.6: Simple
Telex, 8192 macros, optional macro auto-case, per-app VI/EN mode. It
it behaves like UniKey on Windows: no underlined preedit text. When a key
converts, the app sends Backspaces and then the new text.

Input methods: Telex, VNI, VIQR, MsVi, simple Telex, user-defined layouts;
macros; spell check with auto-restore. Ctrl+Shift or Alt+Z (configurable)
toggles Vietnamese on/off, Shift+Space restores the typed keys of the last word.
`unikey-settings` is the settings window.

## How it reaches applications

| Application | Mechanism |
|---|---|
| Wayland-native (GTK, Qt, Chromium/Edge/Electron, WezTerm ...) | Wayland input method (`zwp_input_method_v2`) + virtual keyboard. Backspace key presses, then committed text a few ms later, through one ordered queue. |
| X11 / XWayland | Acts as an X11 client: watches keystrokes (XInput2 raw events) and types corrections back with XTest. Needs no extra privileges; the app briefly shows the raw key before the fix. |

No per-app flags or lists. XWayland only sends raw key events while an X11
window has focus, and the Wayland input method only activates for a focused
text field, so the two paths never convert the same key.

## Where it runs

| Session | What works |
|---|---|
| Hyprland | Everything. Focus comes from Hyprland's IPC, the XWayland layout from its `input:kb_*` options. |
| Other compositors with `zwp_input_method_v2` (Sway, River, labwc, Wayfire, Niri ...) | Everything, including per-app VI/EN: focus comes from `wlr-foreign-toplevel-management`. The XWayland layout is read from `_XKB_RULES_NAMES` (often just `us`). Without that protocol: typing only. |
| X11 session (any desktop or window manager) | Typing in every app, through the X11 path. Started without Wayland, `unikey` runs X11-only and reads the layout set by `setxkbmap`. |
| GNOME / KDE on Wayland | XWayland apps only: these compositors do not offer the input method protocol (GNOME takes IBus, KDE only `zwp_input_method_v1`). |

On X11 a mouse click starts a new word, since the cursor may have moved.

## Build, install, run

    cargo build --release
    install -Dm755 target/release/unikey ~/.local/bin/unikey
    unikey                      # UNIKEY_LOG=1 for a trace on stderr

Start it from your compositor's autostart. Toolkits must be pointed at the
Wayland input method, e.g. `GTK_IM_MODULE=wayland` (and no `XMODIFIERS`/`QT_IM_MODULE` for fcitx).

Settings: `~/.config/unikey/config` (see `src/settings.rs`), or the
`unikey-settings` window.

## Showing VI/EN

- **Tray icon** (StatusNotifierItem): a VI/EN badge in any tray that supports
  it: Waybar's `tray` module, KDE, GNOME with the AppIndicator extension, AGS,
  eww... Left click switches, right click opens a menu (switch, settings, quit).
- **Mode file**: `~/.local/state/unikey/mode` holds `VI` or `EN`, for any bar
  or script. To switch from a script or keybinding: `pkill -USR2 -x unikey`.
- **Waybar text module** (optional): set `bar_signal = 8` in the config, then

      "custom/unikey": {
          "exec": "cat ${XDG_STATE_HOME:-$HOME/.local/state}/unikey/mode",
          "interval": "once",
          "signal": 8,
          "on-click": "pkill -USR2 -x unikey"
      }

## Tests

    cargo test --release

`tests/golden` replays sessions recorded from the C++ engine in fcitx5-unikey
(all input methods, options, macros, user layout, backspace, restore) and checks
identical answers; `tests/golden/fcitx5_telex.tsv` is fcitx5-unikey's own Telex
table (1307 cases). `tests/typing.rs` types real Vietnamese. `examples/inject.rs` types
through a virtual keyboard for end-to-end tests.

## Differences from the original

- Output is Unicode only; the legacy charset converters (TCVN3, VNI, ...) are not ported.
- Macro files without the version header are read as UTF-8 (the original assumed VIQR).
- When a tone is undone by a key that is also a word break (VIQR `?`/`.`, MsVi
  digits), the original reports "delete 0" and corrupts the text; this port does not.

## License

GPL-3.0-or-later, as the code it derives from (see `LICENSE`).
