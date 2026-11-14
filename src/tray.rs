//! System tray icon (StatusNotifierItem): a "VI"/"EN" badge shown by any
//! tray that speaks the standard: Waybar's tray, KDE, GNOME with the
//! AppIndicator extension, AGS, ...
//!
//! Left click switches Vietnamese typing; the menu has the switch, the settings
//! window and Quit. The tray runs on its own thread and asks the main loop to
//! switch by sending SIGUSR2, the same path the settings app uses.

use std::process::Command;

use ksni::{menu::*, Icon, MenuItem, ToolTip};

pub struct UnikeyTray {
    pub vietnamese: bool,
    last_click: Option<std::time::Instant>,
}

/// GTK's default double-click time.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

/// 5x7 glyphs of the four letters we draw ('#' = ink).
fn glyph(c: char) -> &'static [&'static str; 7] {
    match c {
        'V' => &["#...#", "#...#", "#...#", "#...#", ".#.#.", ".#.#.", "..#.."],
        'I' => &["###", ".#.", ".#.", ".#.", ".#.", ".#.", "###"],
        'E' => &["####", "#...", "#...", "###.", "#...", "#...", "####"],
        _ => &["#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#"], // N
    }
}

/// A rounded badge with two letters, `size` x `size`, ARGB32 big-endian.
pub fn badge(text: &str, fill: [u8; 3], ink: [u8; 3], size: usize) -> Vec<u8> {
    let scale = (size / 16).max(1);
    let letters: Vec<_> = text.chars().map(glyph).collect();
    let text_w: usize = letters.iter().map(|g| g[0].len() * scale).sum::<usize>() + scale * (letters.len() - 1);
    let (x0, y0) = ((size - text_w) / 2, (size - 7 * scale) / 2);
    let r = size as f32 / 4.0;
    let mut px = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            // inside the rounded rectangle?
            let (cx, cy) = ((x as f32 + 0.5).clamp(r, size as f32 - r), (y as f32 + 0.5).clamp(r, size as f32 - r));
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            if dx * dx + dy * dy > r * r {
                continue;
            }
            let mut color = fill;
            if y >= y0 && y < y0 + 7 * scale {
                let row = (y - y0) / scale;
                let mut left = x0;
                for g in &letters {
                    let w = g[0].len() * scale;
                    if (left..left + w).contains(&x) && g[row].as_bytes()[(x - left) / scale] == b'#' {
                        color = ink;
                    }
                    left += w + scale; // one column of space between letters
                }
            }
            let i = (y * size + x) * 4;
            px[i..i + 4].copy_from_slice(&[255, color[0], color[1], color[2]]);
        }
    }
    px
}

fn toggle() {
    unsafe { libc::kill(libc::getpid(), libc::SIGUSR2) };
}

fn open_settings() {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join("unikey-settings")));
    let exe = exe.filter(|p| p.exists()).unwrap_or_else(|| "unikey-settings".into());
    // Reap it when it closes, or every settings window leaves a zombie behind.
    if let Ok(mut child) = Command::new(exe).process_group(0).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

impl ksni::Tray for UnikeyTray {
    fn id(&self) -> String {
        "unikey".into()
    }

    fn title(&self) -> String {
        "UniKey".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        // Catppuccin: lavender badge for VI, grey for EN, dark letters.
        let (text, fill) = if self.vietnamese { ("VI", [0xb4, 0xbe, 0xfe]) } else { ("EN", [0x7f, 0x84, 0x9c]) };
        [16, 32, 64]
            .into_iter()
            .map(|s| Icon { width: s as i32, height: s as i32, data: badge(text, fill, [0x11, 0x11, 0x1b], s) })
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "UniKey".into(),
            description: if self.vietnamese { "Tiếng Việt (VI)" } else { "English (EN)" }.into(),
            ..Default::default()
        }
    }

    /// Click switches VI/EN. The tray protocol has no double-click, so two
    /// clicks in quick succession open the settings; the two switches cancel out.
    fn activate(&mut self, _x: i32, _y: i32) {
        toggle();
        let now = std::time::Instant::now();
        if self.last_click.is_some_and(|t| now - t < DOUBLE_CLICK) {
            self.last_click = None;
            open_settings();
        } else {
            self.last_click = Some(now);
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            CheckmarkItem {
                label: "Gõ tiếng Việt".into(),
                checked: self.vietnamese,
                activate: Box::new(|_| toggle()),
                ..Default::default()
            }
            .into(),
            StandardItem { label: "Cài đặt…".into(), activate: Box::new(|_| open_settings()), ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem { label: "Thoát".into(), activate: Box::new(|_| std::process::exit(0)), ..Default::default() }
                .into(),
        ]
    }
}

pub fn spawn(vietnamese: bool) -> ksni::Handle<UnikeyTray> {
    let service = ksni::TrayService::new(UnikeyTray { vietnamese, last_click: None });
    let handle = service.handle();
    service.spawn();
    handle
}

#[cfg(test)]
mod tests {
    use super::badge;

    #[test]
    fn badge_draws_letters_inside_a_rounded_box() {
        let (fill, ink) = ([1, 2, 3], [9, 9, 9]);
        for size in [16, 32] {
            let px = badge("VI", fill, ink, size);
            assert_eq!(px.len(), size * size * 4);
            let at = |x: usize, y: usize| &px[(y * size + x) * 4..][..4];
            assert_eq!(at(0, 0), &[0, 0, 0, 0], "corner is transparent");
            assert_eq!(at(size / 2, 1), &[255, 1, 2, 3], "edge is filled");
            let inked = (0..size * size).filter(|i| px[i * 4 + 1..i * 4 + 4] == ink).count();
            assert!(inked > 10 * (size / 16).pow(2), "letters drawn ({inked} px)");
        }
    }
}
