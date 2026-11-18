//! UniKey settings: a small GTK4/libadwaita window in the spirit of UniKey's
//! dialog on Windows. Changes apply at once: the settings file is written and
//! the running input method is told to reload (SIGUSR1); the big switch toggles
//! Vietnamese typing (SIGUSR2), the same thing the VI/EN shortcut does.

use std::{cell::RefCell, fs, rc::Rc, time::Duration};

use adw::prelude::*;
use gtk::glib;
use unikey::{
    InputMethod,
    settings::{self, INPUT_METHODS, Settings, TOGGLE_KEYS, ToggleKey},
};

const APP_ID: &str = "dev.unikey.Settings";
const MACRO_HEADER: &str = ";DO NOT DELETE THIS LINE*** version=1 ***";

/// Display names, in the order of `settings::INPUT_METHODS`.
/// Display names, in the order of `settings::TOGGLE_KEYS`.
const TOGGLE_NAMES: [&str; 3] = ["Ctrl + Shift", "Alt + Z", "Không dùng"];

fn toggle_hint(key: ToggleKey) -> &'static str {
    match key {
        ToggleKey::CtrlShift => "Nhấn Ctrl+Shift để chuyển nhanh",
        ToggleKey::AltZ => "Nhấn Alt+Z để chuyển nhanh",
        ToggleKey::None => "Bấm vào chỉ báo VI/EN trên Waybar để chuyển",
    }
}

const IM_NAMES: [&str; 7] =
    ["Telex", "VNI", "VIQR", "Microsoft VI", "Telex đơn giản", "Telex đơn giản 2", "Tự định nghĩa"];

const CSS: &str = r#"
:root {
    --accent-bg-color: #b4befe;
    --accent-fg-color: #11111b;
    --accent-color: #b4befe;
    --window-bg-color: #181825;
    --window-fg-color: #cdd6f4;
    --view-bg-color: transparent;
    --headerbar-bg-color: transparent;
    --headerbar-shade-color: transparent;
    --card-bg-color: rgba(255, 255, 255, 0.05);
    --popover-bg-color: #1e1e2e;
    --dialog-bg-color: #1e1e2e;
}

/* Opaque: Hyprland does not blur behind this window, and text over sharp
   text behind it is hard to read. The glass look comes from the soft glows,
   the translucent cards and the rim light instead. */
window.unikey {
    background-color: #181825;
    background-image: radial-gradient(circle at 15% 0%, rgba(180, 190, 254, 0.12), transparent 55%),
                      radial-gradient(circle at 100% 100%, rgba(137, 220, 235, 0.08), transparent 50%);
    border: 1px solid rgba(255, 255, 255, 0.10);
}

window.unikey headerbar {
    background: transparent;
    box-shadow: none;
}

.hero {
    padding: 22px;
    border-radius: 26px;
    background-image: linear-gradient(135deg, rgba(180, 190, 254, 0.30), rgba(137, 220, 235, 0.12));
    border: 1px solid rgba(255, 255, 255, 0.14);
    box-shadow: inset 0 1px rgba(255, 255, 255, 0.18), 0 8px 24px rgba(0, 0, 0, 0.18);
    transition: background-image 300ms ease;
}

.hero.off {
    background-image: linear-gradient(135deg, rgba(127, 132, 156, 0.20), rgba(69, 71, 90, 0.10));
}

.mode-badge {
    font-size: 28px;
    font-weight: 800;
    min-width: 76px;
    min-height: 76px;
    border-radius: 22px;
    background: rgba(17, 17, 27, 0.40);
    color: #b4befe;
    box-shadow: inset 0 1px rgba(255, 255, 255, 0.10);
}

.hero.off .mode-badge {
    color: #7f849c;
}

.hero-title {
    font-size: 20px;
    font-weight: 700;
}

.hero-sub {
    opacity: 0.7;
}

list.boxed-list {
    border-radius: 18px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    box-shadow: inset 0 1px rgba(255, 255, 255, 0.06);
}

.macro-key {
    font-weight: 700;
    color: #b4befe;
}
"#;

fn main() -> glib::ExitCode {
    // A forced GTK theme (the session sets GTK_THEME=Adwaita-dark) replaces
    // libadwaita's own stylesheet and breaks its layout; use libadwaita's.
    std::env::remove_var("GTK_THEME");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("a display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
    // GApplication is already single-instance: a second launch re-activates this
    // one, so raise the open window instead of building another.
    app.connect_activate(|app| match app.active_window() {
        Some(w) => w.present(),
        None => build_window(app),
    });
    app.run()
}

// ---- talking to the input method -------------------------------------------------

fn signal_daemon(sig: i32) {
    for pid in settings::daemon_pids() {
        unsafe { libc::kill(pid, sig) };
    }
}

fn vietnamese_on() -> bool {
    fs::read_to_string(settings::mode_file()).map_or(true, |m| m.trim() != "EN")
}

fn start_daemon() {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join("unikey")));
    let exe = exe.filter(|p| p.exists()).unwrap_or_else(|| "unikey".into());
    let _ = std::process::Command::new(exe).process_group(0).spawn();
}

/// Save the settings and have the input method pick them up.
fn apply(s: &Settings) {
    if let Err(e) = s.save() {
        eprintln!("unikey-settings: cannot save settings: {e}");
    }
    signal_daemon(libc::SIGUSR1);
}

// ---- macros -------------------------------------------------------------------------

fn load_macros() -> Vec<(String, String)> {
    let text = fs::read_to_string(settings::macro_file()).unwrap_or_default();
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter(|l| !(l.contains("***") && l.contains("version=")))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn save_macros(items: &[(String, String)]) {
    let mut text = String::from(MACRO_HEADER);
    for (k, v) in items {
        text.push('\n');
        text.push_str(&format!("{k}:{v}"));
    }
    text.push('\n');
    let _ = fs::create_dir_all(settings::config_dir());
    if let Err(e) = fs::write(settings::macro_file(), text) {
        eprintln!("unikey-settings: cannot save macros: {e}");
    }
    signal_daemon(libc::SIGUSR1);
}

/// The macro list: entries plus the rows showing them.
struct MacroUi {
    items: RefCell<Vec<(String, String)>>,
    list: adw::ExpanderRow,
    rows: RefCell<Vec<adw::ActionRow>>,
}

/// Show `ui.items` as rows, each with a delete button.
fn rebuild_macros(ui: &Rc<MacroUi>) {
    for row in ui.rows.borrow_mut().drain(..) {
        ui.list.remove(&row);
    }
    let items = ui.items.borrow().clone();
    ui.list.set_subtitle(&format!("{} mục", items.len()));
    for (i, (key, text)) in items.into_iter().enumerate() {
        let row = adw::ActionRow::builder().title(key.as_str()).subtitle(text.as_str()).use_markup(false).build();
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        del.set_valign(gtk::Align::Center);
        del.set_tooltip_text(Some("Xoá"));
        let this = ui.clone();
        del.connect_clicked(move |_| {
            this.items.borrow_mut().remove(i);
            save_macros(&this.items.borrow());
            rebuild_macros(&this);
        });
        row.add_suffix(&del);
        ui.list.add_row(&row);
        ui.rows.borrow_mut().push(row);
    }
}

// ---- window ---------------------------------------------------------------------------

fn switch_row(title: &str, subtitle: &str, active: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build()
}

fn build_window(app: &adw::Application) {
    let state = Rc::new(RefCell::new(Settings::load()));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("UniKey")
        .default_width(460)
        .default_height(760)
        .build();
    window.add_css_class("unikey");

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("UniKey", "Bộ gõ tiếng Việt")));
    toolbar.add_top_bar(&header);

    let banner = adw::Banner::new("UniKey chưa chạy");
    banner.set_button_label(Some("Khởi động"));
    banner.connect_button_clicked(|_| start_daemon());
    toolbar.add_top_bar(&banner);

    let page = adw::PreferencesPage::new();

    // ---- hero: Vietnamese on/off
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    hero.add_css_class("hero");
    let badge = gtk::Label::new(Some("VI"));
    badge.add_css_class("mode-badge");
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 4);
    texts.set_hexpand(true);
    texts.set_valign(gtk::Align::Center);
    let hero_title = gtk::Label::builder().label("Tiếng Việt").xalign(0.0).build();
    hero_title.add_css_class("hero-title");
    let hero_sub = gtk::Label::builder().label(toggle_hint(state.borrow().toggle_key)).xalign(0.0).build();
    hero_sub.add_css_class("hero-sub");
    texts.append(&hero_title);
    texts.append(&hero_sub);
    let mode_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    mode_switch.set_tooltip_text(Some("Bật/tắt gõ tiếng Việt"));
    hero.append(&badge);
    hero.append(&texts);
    hero.append(&mode_switch);
    let hero_group = adw::PreferencesGroup::new();
    hero_group.add(&hero);
    page.add(&hero_group);

    let show_mode = {
        let (hero, badge, hero_title, mode_switch) = (hero.clone(), badge.clone(), hero_title.clone(), mode_switch.clone());
        move |on: bool| {
            badge.set_label(if on { "VI" } else { "EN" });
            hero_title.set_label(if on { "Tiếng Việt" } else { "English" });
            if on {
                hero.remove_css_class("off");
            } else {
                hero.add_css_class("off");
            }
            if mode_switch.is_active() != on {
                mode_switch.set_active(on);
            }
        }
    };
    show_mode(vietnamese_on());
    mode_switch.connect_active_notify(|sw| {
        if sw.is_active() != vietnamese_on() {
            signal_daemon(libc::SIGUSR2);
        }
    });

    // ---- input method
    let im_group = adw::PreferencesGroup::builder().title("Kiểu gõ").build();
    let im_row = adw::ComboRow::builder()
        .title("Phương pháp gõ")
        .subtitle("Cách gõ dấu và chữ đặc biệt")
        .model(&gtk::StringList::new(&IM_NAMES))
        .build();
    let current = INPUT_METHODS.iter().position(|(im, _)| *im == state.borrow().input_method).unwrap_or(0);
    im_row.set_selected(current as u32);
    {
        let state = state.clone();
        im_row.connect_selected_notify(move |row| {
            let im: InputMethod = INPUT_METHODS[row.selected() as usize].0;
            state.borrow_mut().input_method = im;
            apply(&state.borrow());
        });
    }
    im_group.add(&im_row);

    let toggle_row = adw::ComboRow::builder()
        .title("Phím chuyển VI/EN")
        .subtitle("Bật/tắt gõ tiếng Việt bằng bàn phím")
        .model(&gtk::StringList::new(&TOGGLE_NAMES))
        .build();
    let current = TOGGLE_KEYS.iter().position(|(k, _)| *k == state.borrow().toggle_key).unwrap_or(0);
    toggle_row.set_selected(current as u32);
    {
        let (state, hero_sub) = (state.clone(), hero_sub.clone());
        toggle_row.connect_selected_notify(move |row| {
            let key = TOGGLE_KEYS[row.selected() as usize].0;
            state.borrow_mut().toggle_key = key;
            hero_sub.set_label(toggle_hint(key));
            apply(&state.borrow());
        });
    }
    im_group.add(&toggle_row);
    page.add(&im_group);

    // ---- options
    let opt_group = adw::PreferencesGroup::builder().title("Tuỳ chọn").build();
    let o = state.borrow().options;
    type Setter = fn(&mut Settings, bool);
    let per_app = state.borrow().per_app_mode;
    let options: [(&str, &str, bool, Setter); 5] = [
        ("Bỏ dấu tự do", "Gõ dấu ở bất kỳ đâu trong từ", o.free_marking, |s, v| s.options.free_marking = v),
        ("Kiểu bỏ dấu mới", "hoà, thuỷ thay cho hòa, thủy", o.modern_style, |s, v| s.options.modern_style = v),
        ("Kiểm tra chính tả", "Chỉ bỏ dấu cho từ tiếng Việt hợp lệ", o.spell_check_enabled, |s, v| {
            s.options.spell_check_enabled = v
        }),
        ("Tự khôi phục phím", "Trả lại phím đã gõ khi từ không phải tiếng Việt", o.auto_non_vn_restore, |s, v| {
            s.options.auto_non_vn_restore = v
        }),
        ("Nhớ chế độ theo ứng dụng", "Mỗi ứng dụng giữ VI/EN riêng khi chuyển qua lại", per_app, |s, v| {
            s.per_app_mode = v
        }),
    ];
    for (title, subtitle, active, set) in options {
        let row = switch_row(title, subtitle, active);
        let state = state.clone();
        row.connect_active_notify(move |row| {
            set(&mut state.borrow_mut(), row.is_active());
            apply(&state.borrow());
        });
        opt_group.add(&row);
    }
    page.add(&opt_group);

    // ---- macros
    let mac_group = adw::PreferencesGroup::builder()
        .title("Gõ tắt")
        .description("Gõ từ viết tắt rồi dấu cách để thay bằng cụm từ đầy đủ")
        .build();
    let mac_switch = switch_row("Bật gõ tắt", "Ví dụ: vn → Việt Nam", o.macro_enabled);
    {
        let state = state.clone();
        mac_switch.connect_active_notify(move |row| {
            state.borrow_mut().options.macro_enabled = row.is_active();
            apply(&state.borrow());
        });
    }
    mac_group.add(&mac_switch);
    let case_switch = switch_row("Tự viết hoa theo từ gõ", "vn → việt nam, VN → VIỆT NAM", o.macro_auto_case);
    {
        let state = state.clone();
        case_switch.connect_active_notify(move |row| {
            state.borrow_mut().options.macro_auto_case = row.is_active();
            apply(&state.borrow());
        });
    }
    mac_group.add(&case_switch);

    // Enabling macros opens the list, which is what you want to see next.
    let mac_list = adw::ExpanderRow::builder().title("Danh sách gõ tắt").expanded(o.macro_enabled).build();
    let macro_ui = Rc::new(MacroUi { items: RefCell::new(load_macros()), list: mac_list.clone(), rows: RefCell::default() });

    // The "add" row: abbreviation + replacement + button.
    let add_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    add_box.set_margin_top(10);
    add_box.set_margin_bottom(10);
    add_box.set_margin_start(12);
    add_box.set_margin_end(12);
    let key_entry = gtk::Entry::builder().placeholder_text("Viết tắt").width_chars(8).build();
    let text_entry = gtk::Entry::builder().placeholder_text("Thay bằng").hexpand(true).build();
    let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
    add_btn.add_css_class("suggested-action");
    add_btn.add_css_class("circular");
    add_btn.set_tooltip_text(Some("Thêm"));
    add_box.append(&key_entry);
    add_box.append(&text_entry);
    add_box.append(&add_btn);
    let add_row = gtk::ListBoxRow::builder().child(&add_box).activatable(false).build();
    mac_list.add_row(&add_row);
    let add = {
        let (ui, key_entry, text_entry) = (macro_ui.clone(), key_entry.clone(), text_entry.clone());
        move || {
            let key = key_entry.text().trim().replace(':', "");
            let text = text_entry.text().to_string();
            if key.is_empty() || text.trim().is_empty() {
                return;
            }
            {
                let mut m = ui.items.borrow_mut();
                m.retain(|(k, _)| k != &key);
                m.push((key, text));
            }
            save_macros(&ui.items.borrow());
            key_entry.set_text("");
            text_entry.set_text("");
            key_entry.grab_focus();
            rebuild_macros(&ui);
        }
    };
    let add = Rc::new(add);
    {
        let add = add.clone();
        add_btn.connect_clicked(move |_| add());
    }
    {
        let add = add.clone();
        text_entry.connect_activate(move |_| add());
    }
    rebuild_macros(&macro_ui);
    {
        let mac_list = mac_list.clone();
        mac_switch.connect_active_notify(move |row| mac_list.set_expanded(row.is_active()));
    }
    mac_group.add(&mac_list);
    page.add(&mac_group);

    // ---- keep in step with the input method (shortcut, Waybar clicks, crashes)
    let refresh_status = {
        let banner = banner.clone();
        move || {
            show_mode(vietnamese_on());
            // On GNOME IBus starts unikey when its input source is picked.
            banner.set_revealed(settings::daemon_pids().is_empty() && !settings::runs_in_ibus());
        }
    };
    refresh_status();
    glib::timeout_add_local(Duration::from_millis(500), move || {
        refresh_status();
        glib::ControlFlow::Continue
    });

    toolbar.set_content(Some(&page));
    window.set_content(Some(&toolbar));
    window.present();
}
