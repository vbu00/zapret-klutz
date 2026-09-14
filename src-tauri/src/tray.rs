//! Трей — единственное, что видно, когда окно скрыто.
//!
//! Меню по правому клику — не системное меню Win32 (выглядит как из 2000-х
//! и не умеет ни тему, ни тумблеры), а маленькое прозрачное окно в стиле
//! приложения (src/tray-menu.html). Оно встаёт над иконкой и прячется, как
//! только теряет фокус.

use once_cell::sync::Lazy;
use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

use crate::state::AppState;
use crate::winws;

pub const TRAY_ID: &str = "klutz-tray";
pub const POPUP: &str = "tray-menu";

pub fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Цвет иконки: зелёный «всё отвечает», оранжевый «работает, но не всё»,
/// красный «включён, но связи нет», серый «выключен».
#[derive(PartialEq, Clone, Copy)]
pub enum TrayState {
    Ok,
    Warn,
    Bad,
    Idle,
}

impl TrayState {
    fn key(self) -> &'static str {
        match self {
            TrayState::Ok => "ok",
            TrayState::Warn => "warn",
            TrayState::Bad => "bad",
            TrayState::Idle => "idle",
        }
    }
}

// ─────────── Иконки ───────────
//
// Windows рисует иконку трея размером SM_CXSMICON: 16 px при 100%, 20 при
// 125%, 24 при 150%, 32 при 200%. Отдай ей 32 px на 125% — сама ужмёт до 20
// и получится мыло, поэтому каждый размер отрендерен отдельно из SVG.

macro_rules! icon_set {
    ($s:literal) => {
        [
            (16, include_bytes!(concat!("../icons/tray-", $s, "-16.png")) as &[u8]),
            (20, include_bytes!(concat!("../icons/tray-", $s, "-20.png")) as &[u8]),
            (24, include_bytes!(concat!("../icons/tray-", $s, "-24.png")) as &[u8]),
            (28, include_bytes!(concat!("../icons/tray-", $s, "-28.png")) as &[u8]),
            (32, include_bytes!(concat!("../icons/tray-", $s, "-32.png")) as &[u8]),
            (40, include_bytes!(concat!("../icons/tray-", $s, "-40.png")) as &[u8]),
            (48, include_bytes!(concat!("../icons/tray-", $s, "-48.png")) as &[u8]),
            (64, include_bytes!(concat!("../icons/tray-", $s, "-64.png")) as &[u8]),
        ]
    };
}

const ICONS_OK: [(i32, &[u8]); 8] = icon_set!("ok");
const ICONS_WARN: [(i32, &[u8]); 8] = icon_set!("warn");
const ICONS_BAD: [(i32, &[u8]); 8] = icon_set!("bad");
const ICONS_IDLE: [(i32, &[u8]); 8] = icon_set!("idle");

#[cfg(target_os = "windows")]
fn small_icon_px() -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    let px = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if px > 0 { px } else { 16 }
}

#[cfg(not(target_os = "windows"))]
fn small_icon_px() -> i32 {
    32
}

fn icon_bytes(s: TrayState) -> &'static [u8] {
    let set: &[(i32, &'static [u8])] = match s {
        TrayState::Ok => &ICONS_OK,
        TrayState::Warn => &ICONS_WARN,
        TrayState::Bad => &ICONS_BAD,
        TrayState::Idle => &ICONS_IDLE,
    };
    let want = small_icon_px();
    set.iter()
        .find(|(px, _)| *px >= want)
        .or_else(|| set.last())
        .map(|(_, b)| *b)
        .unwrap_or(&[])
}

// ─────────── Снимок состояния ───────────

struct Snapshot {
    running: bool,
    active: Option<String>,
    state: TrayState,
    /// Была ли вообще хоть одна проверка связи для текущего запуска.
    checked: bool,
    targets: Vec<(String, bool, u64)>,
    /// Конфиг и его доля ответивших целей в последнем прогоне.
    switch_to: Vec<(String, f64)>,
    tg_running: bool,
    root: Option<String>,
}

/// «general (FAKE TLS AUTO).bat» → «FAKE TLS AUTO», «general.bat» →
/// «Базовый» — те же подписи, что в окне (prettyName в renderer.js).
fn pretty(config: &str) -> String {
    let base = config.trim_end_matches(".bat").trim();
    let lower = base.to_lowercase();
    if lower == "general" {
        return "Базовый".into();
    }
    if lower.starts_with("general") {
        let rest = base[7..].trim();
        if rest.starts_with('(') && rest.ends_with(')') {
            return rest[1..rest.len() - 1].trim().to_string();
        }
    }
    base.to_string()
}

fn snapshot(app: &AppHandle) -> Snapshot {
    let st = app.state::<AppState>();
    let running = winws::is_winws_running();
    let (active, root) = {
        let p = st.persisted.lock().unwrap();
        (p.active_config.clone(), p.root_path.clone())
    };
    let last = *st.last_check.lock().unwrap();
    // None — проверок ещё не было. Раньше этот случай проваливался в ветку
    // `_ => Ok`, и иконка уверенно горела зелёным «всё отвечает», ничего не
    // проверив: первые секунды после запуска и после каждого включения.
    let state = match (running, last) {
        (false, _) => TrayState::Idle,
        (true, None) => TrayState::Warn,
        (true, Some((0, total))) if total > 0 => TrayState::Bad,
        (true, Some((ok, total))) if ok < total => TrayState::Warn,
        (true, _) => TrayState::Ok,
    };
    let checked = last.is_some();
    let targets = if running { st.last_targets.lock().unwrap().clone() } else { vec![] };

    // Лучшие из последнего прогона — тот же источник, которому доверяет
    // самолечение, чтобы переключаться, не открывая окно.
    let switch_to = match &root {
        Some(r) => {
            let root = std::path::Path::new(r);
            crate::monitor::latest_ranking_scored(root)
                .into_iter()
                .filter(|(c, _)| Some(c) != active.as_ref() && root.join(c).exists())
                .take(5)
                .collect()
        }
        None => vec![],
    };

    let tg_running = st.tgws_pid.lock().unwrap().is_some();

    Snapshot { running, active, state, checked, targets, switch_to, tg_running, root }
}

fn tooltip(s: &Snapshot) -> String {
    let name = s.active.as_deref().map(pretty).unwrap_or_else(|| "вариант не выбран".into());
    let status = match (s.running, s.checked, s.state) {
        (false, _, _) => "обход выключен".to_string(),
        (true, false, _) => format!("проверяю связь · {name}"),
        (true, _, TrayState::Bad) => format!("цели не отвечают · {name}"),
        (true, _, TrayState::Warn) => format!("работает с ошибками · {name}"),
        (true, _, _) => format!("работает · {name}"),
    };
    let mut t = format!("Klutz — {status}");
    if s.tg_running {
        t.push_str("\nTelegram-прокси включён");
    }
    // Подсказка трея в Windows обрезается на 127 символах.
    t.chars().take(120).collect()
}

// ─────────── Всплывающее меню ───────────

#[derive(Serialize)]
pub struct TargetRow {
    name: String,
    ok: bool,
    ms: u64,
}

#[derive(Serialize)]
pub struct SwitchRow {
    file: String,
    name: String,
    /// Доля ответивших целей, 0..1 — та же, что в процентах в окне.
    score: f64,
}

#[derive(Serialize)]
pub struct TrayVersions {
    app: String,
    zapret: Option<String>,
    tgws: &'static str,
}

#[derive(Serialize)]
pub struct TrayMenuState {
    running: bool,
    state: &'static str,
    active: Option<String>,
    targets: Vec<TargetRow>,
    #[serde(rename = "switchTo")]
    switch_to: Vec<SwitchRow>,
    #[serde(rename = "tgRunning")]
    tg_running: bool,
    versions: TrayVersions,
}

/// Прямоугольник иконки трея в физических пикселях (x, y, w, h) — от него
/// меню и позиционируется.
static ANCHOR: Lazy<Mutex<Option<(f64, f64, f64, f64)>>> = Lazy::new(|| Mutex::new(None));
/// Когда меню спряталось. Клик по иконке при открытом меню сначала снимает
/// с меню фокус (оно прячется), а потом приходит сам клик — без этой
/// отсечки меню тут же открылось бы снова вместо того, чтобы закрыться.
static LAST_HIDE: Lazy<Mutex<Option<Instant>>> = Lazy::new(|| Mutex::new(None));

fn open_popup(app: &AppHandle, rect: tauri::Rect) {
    if let Some(t) = *LAST_HIDE.lock().unwrap() {
        if t.elapsed() < Duration::from_millis(300) {
            return;
        }
    }
    let scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);
    let pos = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    *ANCHOR.lock().unwrap() = Some((pos.x, pos.y, size.width, size.height));

    if app.get_webview_window(POPUP).is_some() {
        // Страница перерисуется и сама попросит показать окно (tray_menu_ready).
        let _ = app.emit_to(POPUP, "tray-menu-open", ());
        return;
    }
    // Первое открытие: окно создаётся скрытым, страница после загрузки
    // сообщит свой размер — тогда и покажем, без мигания.
    let built = tauri::WebviewWindowBuilder::new(app, POPUP, tauri::WebviewUrl::App("tray-menu.html".into()))
        .title("Klutz")
        .inner_size(300.0, 420.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .build();
    if built.is_err() {
        // Окно не создалось — хотя бы не оставляем пользователя без доступа.
        show_window(app);
    }
}

pub fn hide_popup(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(POPUP) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
            *LAST_HIDE.lock().unwrap() = Some(Instant::now());
        }
    }
}

#[tauri::command]
pub fn tray_menu_state(app: AppHandle) -> TrayMenuState {
    let s = snapshot(&app);
    // Версию zapret читаем только при открытии меню, а не в snapshot: тот
    // зовётся на каждое обновление иконки.
    let zapret = s.root.as_deref().and_then(|r| crate::maintenance::local_version(std::path::Path::new(r)));
    TrayMenuState {
        running: s.running,
        state: s.state.key(),
        active: s.active.as_deref().map(pretty),
        targets: s.targets.into_iter().map(|(name, ok, ms)| TargetRow { name, ok, ms }).collect(),
        switch_to: s
            .switch_to
            .iter()
            .map(|(f, score)| SwitchRow { file: f.clone(), name: pretty(f), score: *score })
            .collect(),
        tg_running: s.tg_running,
        versions: TrayVersions { app: app.package_info().version.to_string(), zapret, tgws: crate::tgws::BUNDLED_VERSION },
    }
}

/// Страница меню отрисовалась и знает свой размер (в CSS-пикселях) —
/// ставим окно над иконкой трея в пределах рабочей области экрана.
#[tauri::command]
pub fn tray_menu_ready(app: AppHandle, width: f64, height: f64) {
    let Some(w) = app.get_webview_window(POPUP) else { return };
    let Some((ax, ay, aw, ah)) = *ANCHOR.lock().unwrap() else { return };
    let cx = ax + aw / 2.0;
    let cy = ay + ah / 2.0;
    let mon = app
        .monitor_from_point(cx, cy)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());
    let scale = mon.as_ref().map(|m| m.scale_factor()).unwrap_or(1.0);
    let (wx, wy, ww, wh) = mon
        .as_ref()
        .map(|m| {
            let a = m.work_area();
            (a.position.x as f64, a.position.y as f64, a.size.width as f64, a.size.height as f64)
        })
        .unwrap_or((0.0, 0.0, 1920.0, 1080.0));

    let pw = (width * scale).round();
    let ph = (height * scale).round();
    // У карточки внутри окна свои 10px прозрачных полей под тень — отступ
    // от панели задач поэтому небольшой.
    let gap = 2.0 * scale;

    // Панель задач снизу (обычный случай) — меню над иконкой; сверху — под
    // ней. Сбоку — по высоте иконки. В любом случае в пределах экрана.
    let y = if cy < wy + wh / 2.0 { ay + ah + gap } else { ay.min(wy + wh) - ph - gap };
    let y = y.clamp(wy, (wy + wh - ph).max(wy));
    let x = (cx - pw / 2.0).clamp(wx, (wx + ww - pw).max(wx));

    let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    let _ = w.set_size(tauri::PhysicalSize::new(pw as u32, ph as u32));
    if !w.is_visible().unwrap_or(false) {
        let _ = w.show();
    }
    let _ = w.set_focus();
}

#[tauri::command]
pub fn tray_menu_hide(app: AppHandle) {
    hide_popup(&app);
}

#[tauri::command]
pub fn tray_menu_action(app: AppHandle, id: String) {
    // Тумблер Telegram меню не закрывает — видно, что он переключился.
    if id != "tg" {
        hide_popup(&app);
    }
    match id.as_str() {
        "open" => show_window(&app),
        "stop" => {
            std::thread::spawn(move || {
                winws::kill_winws(&app);
                let state = app.state::<AppState>();
                {
                    let mut p = state.persisted.lock().unwrap();
                    p.active_config = None;
                    p.started_at = None;
                }
                crate::state::save_state(&app, &state);
                refresh(&app);
            });
        }
        "tg" => {
            std::thread::spawn(move || {
                let running = app.state::<AppState>().tgws_pid.lock().unwrap().is_some();
                if running {
                    crate::tgws::stop(&app);
                } else if let Err(e) = crate::tgws::start(&app) {
                    crate::notify::send_from(&app, "Не удалось запустить Telegram-прокси", &e);
                }
                refresh(&app);
            });
        }
        "quit" => {
            // В отдельном потоке, как и соседние ветки: команда объявлена
            // без async, то есть идёт в главном потоке, а kill_winws и
            // tgws::stop синхронно ждут два taskkill — окно на это время
            // замирало.
            std::thread::spawn(move || {
                winws::kill_winws(&app);
                crate::tgws::stop(&app);
                app.exit(0);
            });
        }
        other => {
            if let Some(config) = other.strip_prefix("switch:") {
                let config = config.to_string();
                std::thread::spawn(move || {
                    if let Err(e) = crate::monitor::apply_config(&app, &config) {
                        crate::notify::send_from(&app, "Не удалось переключиться", &e);
                    }
                    refresh(&app);
                });
            }
        }
    }
}

// ─────────── Иконка ───────────

static LAST_ICON: Lazy<Mutex<Option<TrayState>>> = Lazy::new(|| Mutex::new(None));

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let snap = snapshot(app);
    let icon = tauri::image::Image::from_bytes(icon_bytes(snap.state))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(tooltip(&snap))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button, button_state: MouseButtonState::Up, rect, .. } = event {
                match button {
                    // Левый клик — окно, правый — меню, как в Electron.
                    MouseButton::Left => show_window(tray.app_handle()),
                    MouseButton::Right => open_popup(tray.app_handle(), rect),
                    _ => {}
                }
            }
        })
        .build(app)?;

    *LAST_ICON.lock().unwrap() = Some(snap.state);
    Ok(())
}

/// Обновляет иконку и подсказку, а открытое меню — перерисовывает.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let snap = snapshot(app);
    let _ = tray.set_tooltip(Some(tooltip(&snap)));

    let mut last = LAST_ICON.lock().unwrap();
    if *last != Some(snap.state) {
        if let Ok(img) = tauri::image::Image::from_bytes(icon_bytes(snap.state)) {
            let _ = tray.set_icon(Some(img));
            *last = Some(snap.state);
        }
    }
    drop(last);

    if let Some(w) = app.get_webview_window(POPUP) {
        if w.is_visible().unwrap_or(false) {
            let _ = app.emit_to(POPUP, "tray-menu-update", ());
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn подписи_конфигов_как_в_окне() {
        assert_eq!(pretty("general.bat"), "Базовый");
        assert_eq!(pretty("general (FAKE TLS AUTO).bat"), "FAKE TLS AUTO");
        assert_eq!(pretty("general (ALT11).bat"), "ALT11");
        assert_eq!(pretty("другое.bat"), "другое");
    }

    #[test]
    fn имя_с_кириллицей_не_роняет_срез() {
        // Срез base[7..] байтовый — проверяем, что на не-ASCII не паникуем.
        for name in ["генерал.bat", "general (ФЕЙК).bat", "ge.bat", "g.bat", ".bat"] {
            let _ = pretty(name);
        }
    }
}
