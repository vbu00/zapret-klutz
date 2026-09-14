//! Периодический автопрогон тестов.
//!
//! Прогон выключает обход на несколько минут, поэтому запускаем его, только
//! когда пользователя нет за машиной, и всегда предупреждаем заранее — иначе
//! связь оборвётся посреди работы без объяснений.

use std::time::Duration;
use tauri::{AppHandle, Manager};

use crate::state::{save_state, AppState};

const IDLE_GATE_SEC: u64 = 5 * 60;
const WARNING_LEAD: Duration = Duration::from_secs(2 * 60);
const TICK: Duration = Duration::from_secs(30 * 60);
const FIRST_TICK: Duration = Duration::from_secs(2 * 60);
/// Сколько не трогать пользователя после отменённой попытки. Предупреждение
/// уходит до перепроверки, а отметку о прогоне ставим только после неё — без
/// этой паузы вернувшийся к компьютеру человек получал бы «Скоро автопрогон»
/// каждые полчаса, и прогон при этом так и не начинался.
const RETRY_AFTER_ABORT: Duration = Duration::from_secs(6 * 60 * 60);

static LAST_ABORTED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Сколько секунд не было ввода с клавиатуры/мыши.
#[cfg(target_os = "windows")]
fn idle_seconds() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    unsafe {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut info) == 0 {
            return 0;
        }
        let now = GetTickCount();
        // GetTickCount переполняется примерно раз в 49 суток — wrapping_sub
        // даёт правильную разницу и на переходе через ноль.
        (now.wrapping_sub(info.dwTime) / 1000) as u64
    }
}

#[cfg(not(target_os = "windows"))]
fn idle_seconds() -> u64 {
    u64::MAX
}

/// Ничего своего сейчас не крутится и пользователь отошёл.
fn is_clear(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    if *state.testing.lock().unwrap() || crate::gamescan::scan_busy() {
        return false;
    }
    idle_seconds() >= IDLE_GATE_SEC
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_TICK);
        loop {
            maybe_run(&app);
            std::thread::sleep(TICK);
        }
    });
}

fn maybe_run(app: &AppHandle) {
    let state = app.state::<AppState>();

    let (enabled, days, last_run, root, mode) = {
        let p = state.persisted.lock().unwrap();
        (
            p.autotest_enabled.unwrap_or(false),
            p.autotest_days.unwrap_or(7),
            p.autotest_last_run.unwrap_or(0),
            p.root_path.clone(),
            p.autotest_mode.clone().unwrap_or_else(|| "standard".into()),
        )
    };
    let Some(root) = root else { return };
    if !enabled || !is_clear(app) {
        return;
    }
    let interval_ms = (days.max(1) as u64) * 24 * 60 * 60 * 1000;
    if now_ms().saturating_sub(last_run) < interval_ms {
        return;
    }
    // Служба держит свой winws — прогон в это время бессмыслен. Пробуем на
    // следующем тике, а не через целый интервал.
    if crate::service::service_conflict() {
        return;
    }
    if LAST_ABORTED.lock().unwrap().is_some_and(|t| t.elapsed() < RETRY_AFTER_ABORT) {
        return;
    }

    crate::notify::send_from(
        app,
        "Скоро автопрогон тестов",
        "Через пару минут запустятся тесты и ненадолго прервут обход. Если сейчас не время — выключи автопрогон в Настройках.",
    );
    std::thread::sleep(WARNING_LEAD);

    // Перепроверяем: пользователь мог вернуться, или что-то другое могло
    // начать тест либо переключение — влезать в любом случае не надо.
    if !is_clear(app) || crate::service::service_conflict() {
        *LAST_ABORTED.lock().unwrap() = Some(std::time::Instant::now());
        return;
    }

    // Тот же замок, что берёт кнопка «Подобрать стратегию». Раньше здесь
    // стояло присваивание `testing = true` в обход него — и два прогона
    // PowerShell могли идти одновременно, перетирая друг другу общий PID.
    let Some(run) = crate::state::TestRun::acquire(&state) else {
        *LAST_ABORTED.lock().unwrap() = Some(std::time::Instant::now());
        return;
    };

    {
        let mut p = state.persisted.lock().unwrap();
        p.autotest_last_run = Some(now_ms());
    }
    save_state(app, &state);

    let before = state.persisted.lock().unwrap().active_config.clone();
    // Тот же прогон с повтором незапустившихся, что и по кнопке: иначе
    // автопрогон молча терял бы конфиг, который не поднялся случайно.
    let result = crate::tests::run_full_with_retry(app, std::path::Path::new(&root), mode == "dpi", || {
        run.cancelled()
    });
    crate::commands::restore_after_tests(app, before);

    match result {
        Ok(text) => {
            let (rows, dpi) = crate::tests::parse_results(&text);
            // Тот же порядок, что у трея, истории и самолечения.
            let best = rows
                .iter()
                .min_by(|a, b| crate::tests::rank_desc(a, b, dpi))
                .map(|r| r.config.trim_end_matches(".bat").to_string());
            crate::notify::send_from(
                app,
                "Автопрогон завершён",
                &match best {
                    Some(b) => format!("Лучшая стратегия: {b}"),
                    None => "Прогон закончен, но лучшая стратегия не определилась.".into(),
                },
            );
        }
        Err(e) => crate::notify::send_from(app, "Автопрогон не удался", &e),
    }
}
