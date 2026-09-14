use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};

use crate::release::{validate_release, ReleaseCheck};
use crate::state::{save_state, AppState};
use crate::winws;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[derive(Debug, Serialize)]
pub struct GetStateResult {
    #[serde(rename = "rootPath")]
    root_path: Option<String>,
    configs: Vec<String>,
    #[serde(rename = "activeConfig")]
    active_config: Option<String>,
    running: bool,
    #[serde(rename = "installedAsService")]
    installed_as_service: bool,
    #[serde(rename = "hasTestScript")]
    has_test_script: bool,
    #[serde(rename = "canInstallService")]
    can_install_service: bool,
    monitor: Option<serde_json::Value>,
    #[serde(rename = "startedAt")]
    started_at: Option<u64>,
    /// Стоит ли в системе служба Windows «zapret» — чужая, от service.bat
    /// самого zapret, или наша. Пока она есть, прямой запуск winws и прогон
    /// тестов невозможны, и окну нужно предложить её снять, а не показывать
    /// голую ошибку.
    #[serde(rename = "serviceExists")]
    service_exists: bool,
}

/// Снимок последней фоновой проверки в том виде, в каком его ждёт renderer
/// (функция coreCheck): цели с именем и результатом плюс время проверки.
fn monitor_snapshot(state: &State<AppState>) -> Option<serde_json::Value> {
    let at = *state.last_check_at.lock().unwrap();
    if at == 0 {
        return None;
    }
    let targets: Vec<_> = state
        .last_targets
        .lock()
        .unwrap()
        .iter()
        .map(|(name, ok, ms)| serde_json::json!({ "name": name, "ok": ok, "ms": ms }))
        .collect();
    if targets.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "targets": targets, "checkedAt": at }))
}

/// Port of the `get-state` IPC handler.
#[tauri::command(async)]
pub fn get_state(state: State<AppState>) -> GetStateResult {
    let persisted = state.persisted.lock().unwrap().clone();
    let root_path = persisted.root_path.clone().filter(|p| Path::new(p).exists());

    let check = root_path
        .as_ref()
        .map(|p| validate_release(Path::new(p)));

    let running = winws::is_winws_running();

    GetStateResult {
        root_path: root_path.clone(),
        configs: check.as_ref().filter(|c| c.ok).map(|c| c.configs.clone()).unwrap_or_default(),
        active_config: if root_path.is_some() { persisted.active_config } else { None },
        running,
        installed_as_service: persisted.installed_as_service,
        has_test_script: check.as_ref().map(|c| c.has_test_script).unwrap_or(false),
        can_install_service: check.as_ref().map(|c| c.can_install_service).unwrap_or(false),
        // Данные последней фоновой проверки: раньше здесь стоял None за
        // комментарием «монитор ещё не портирован», хотя monitor.rs давно
        // есть и держит результат в last_check/last_targets.
        monitor: monitor_snapshot(&state),
        started_at: if running { persisted.started_at } else { None },
        service_exists: crate::service::service_conflict(),
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum LoadPathResult {
    Ok {
        ok: bool,
        root: String,
        configs: Vec<String>,
        #[serde(rename = "hasTestScript")]
        has_test_script: bool,
        #[serde(rename = "canInstallService")]
        can_install_service: bool,
        /// Что перенесено из прежнего релиза, строками для человека.
        carried: Vec<String>,
    },
    Err {
        ok: bool,
        error: String,
    },
}

/// Делает релиз текущим. Если до него был другой, переносит из прежнего
/// настройки и запоминает его — чтобы можно было вернуться.
///
/// Одно место для всех путей смены релиза: папка, архив, скачивание, откат.
/// Раньше каждый путь сам записывал новый корень, и перенести настройки
/// было негде — они оставались в старой папке.
fn set_root(app: &AppHandle, state: &AppState, root: &Path, can_install_service: bool) -> Vec<String> {
    let old = state.persisted.lock().unwrap().root_path.clone();
    let old = old.filter(|o| Path::new(o) != root);
    let carried = match &old {
        Some(o) if Path::new(o).is_dir() => {
            // Прогоны прежнего релиза — в архив до смены: иначе, если окно
            // тестов на нём ни разу не открывали, сравнивать новый будет не с чем.
            crate::history::archive(app, Path::new(o));
            crate::carry::carry_over(Path::new(o), root)
        }
        _ => Vec::new(),
    };
    {
        let mut p = state.persisted.lock().unwrap();
        if old.is_some() {
            p.previous_root = old;
        }
        p.root_path = Some(root.to_string_lossy().to_string());
        p.active_config = None;
        p.installed_as_service = false;
        p.started_at = None;
        p.can_install_service = can_install_service;
    }
    save_state(app, state);
    carried
}

#[derive(Debug, Serialize)]
pub struct Regression {
    current: String,
    previous: String,
    #[serde(flatten)]
    cmp: crate::history::Comparison,
    /// Куда вернуться. `None` — папки прежнего релиза больше нет.
    #[serde(rename = "rollbackPath")]
    rollback_path: Option<String>,
    /// Что поменялось в каждом просевшем конфиге. Пусто, если сравнить не с
    /// чем — прежнего релиза на диске нет.
    diffs: std::collections::BTreeMap<String, Vec<crate::configdiff::Change>>,
}

/// Стало ли на текущем релизе хуже, чем на прежнем, по последним прогонам.
/// `None` — не хуже, или сравнивать не с чем.
///
/// Берётся последний прогон текущего релиза и последний прогон другого
/// релиза в том же режиме, сделанный раньше. После отката на старый релиз
/// прогоны нового оказываются позже — и тревоги задним числом не будет.
#[tauri::command(async)]
pub fn get_release_regression(app: AppHandle, state: State<AppState>) -> Option<Regression> {
    let root = root_of(&state)?;
    crate::history::archive(&app, &root);
    let label = crate::history::release_label(&root);
    let runs = crate::history::runs(&app);
    let cur = runs.iter().rev().find(|r| r.release == label)?;
    let cur_text = std::fs::read_to_string(&cur.path).ok()?;
    let (cur_rows, cur_dpi) = crate::tests::parse_results(&cur_text);
    if cur_rows.is_empty() {
        return None;
    }
    let (prev, prev_text) = runs
        .iter()
        .rev()
        .filter(|r| r.release != label && r.modified < cur.modified)
        .find_map(|r| {
            let text = std::fs::read_to_string(&r.path).ok()?;
            let (rows, dpi) = crate::tests::parse_results(&text);
            (!rows.is_empty() && dpi == cur_dpi).then_some((r, text))
        })?;
    let cmp = crate::history::compare(&cur_text, &prev_text)?;

    // Вернуться можно туда, откуда переключились, если это тот самый релиз,
    // иначе — в скачанную Klutz папку с тем же именем.
    let годен = |p: &Path| p.join("bin").join("winws.exe").exists();
    let previous_root = state.persisted.lock().unwrap().previous_root.clone();
    let rollback_path = previous_root
        .filter(|p| годен(Path::new(p)) && crate::history::release_label(Path::new(p)) == prev.release)
        .or_else(|| {
            if !safe_name(&prev.release) {
                return None;
            }
            let dir = crate::releases::releases_dir(&app).join(&prev.release);
            let r = crate::releases::release_root(&dir);
            годен(&r).then(|| r.to_string_lossy().into_owned())
        });
    // Что поменялось в просевших конфигах — если прежний релиз ещё на диске.
    let diffs = rollback_path
        .as_deref()
        .map(|old| {
            cmp.drops
                .iter()
                .filter_map(|d| {
                    let old_bat = std::fs::read_to_string(Path::new(old).join(&d.name)).ok()?;
                    let new_bat = std::fs::read_to_string(root.join(&d.name)).ok()?;
                    Some((d.name.clone(), crate::configdiff::diff(&old_bat, &new_bat)))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Regression { current: label, previous: prev.release.clone(), cmp, rollback_path, diffs })
}

/// Кладёт конфиг прежнего релиза в текущий под именем «Было …», чтобы
/// прогнать его тестами на нынешнем winws. Возвращает имя конфига.
#[tauri::command(async)]
pub fn import_old_config(
    app: AppHandle,
    state: State<AppState>,
    old_root: String,
    config: String,
) -> Result<String, String> {
    let root = root_of(&state).ok_or("Сначала загрузи релиз zapret.")?;
    let old = PathBuf::from(&old_root);
    // Путь приходит из интерфейса. Брать конфиги разрешено только из прежнего
    // релиза: запомненного при переключении или скачанного самим Klutz.
    let prev = state.persisted.lock().unwrap().previous_root.clone();
    let разрешён = prev.as_deref().map(Path::new) == Some(old.as_path())
        || old.starts_with(crate::releases::releases_dir(&app));
    if !разрешён || !old.join("bin").join("winws.exe").exists() || old == root {
        return Err("Это не прежний релиз.".into());
    }
    checked_config(&old, &config)?;
    crate::configdiff::import_old(&old, &root, &config)
}

/// Загрузка релиза из папки. Для .zip есть отдельная команда
/// `load_archive` — она распаковывает архив и зовёт эту.
#[tauri::command(async)]
pub fn load_path(app: AppHandle, state: State<AppState>, input_path: String) -> LoadPathResult {
    let picked = PathBuf::from(&input_path);
    if !picked.is_dir() {
        return LoadPathResult::Err {
            ok: false,
            error: "Нужна папка релиза или .zip-архив.".into(),
        };
    }
    // Архив zapret распакован с одной верхней папкой — и в диалоге выбирают
    // обычно её, а не вложенную. Спускаемся сами, вместо того чтобы отвечать
    // «это не похоже на релиз».
    let path = crate::releases::release_root(&picked);

    let check: ReleaseCheck = validate_release(&path);
    if !check.ok {
        return LoadPathResult::Err {
            ok: false,
            error: check.error.unwrap_or_else(|| "Не удалось загрузить релиз.".into()),
        };
    }

    let carried = set_root(&app, &state, &path, check.can_install_service);

    LoadPathResult::Ok {
        ok: true,
        root: path.to_string_lossy().to_string(),
        configs: check.configs,
        has_test_script: check.has_test_script,
        carried,
        can_install_service: check.can_install_service,
    }
}

#[derive(Debug, Serialize)]
pub struct RunConfigResult {
    ok: bool,
    error: Option<String>,
    #[serde(rename = "liveLogs")]
    live_logs: bool,
}

/// Разовый запуск конфига прямым спавном winws.exe. Установка службой —
/// отдельная команда `install_service`.
#[tauri::command(async)]
pub fn run_config(app: AppHandle, state: State<AppState>, file_name: String) -> RunConfigResult {
    let root = match state.persisted.lock().unwrap().root_path.clone() {
        Some(r) => PathBuf::from(r),
        None => {
            return RunConfigResult {
                ok: false,
                error: Some("Сначала загрузи релиз zapret.".into()),
                live_logs: false,
            }
        }
    };

    if let Err(e) = checked_config(&root, &file_name) {
        return RunConfigResult { ok: false, error: Some(e), live_logs: false };
    }

    // Установленная служба zapret держит свой winws.exe — прямой запуск
    // поверх неё конфликтует, поэтому просим сначала снять службу.
    if crate::service::service_conflict() {
        return RunConfigResult {
            ok: false,
            error: Some("Установлена служба Windows «zapret» — сначала сними её.".into()),
            live_logs: false,
        };
    }

    let live_logs = match winws::spawn_winws(&app, &root, &file_name) {
        Ok(v) => v,
        Err(e) => {
            return RunConfigResult {
                ok: false,
                error: Some(e),
                live_logs: false,
            }
        }
    };

    // Те же 1.5 секунды на подъём, что и в applyDirect(), — и только потом
    // записываем состояние. Раньше активная стратегия проставлялась до
    // проверки: команда возвращала ошибку, а окно и трей продолжали
    // показывать конфиг, который не запустился.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let running = winws::is_winws_running();
    if !running {
        // Прежний процесс уже убит в spawn_winws, новый не поднялся — значит
        // не работает НИЧЕГО. Оставить старое имя активным означало бы врать
        // и окну, и трею.
        {
            let mut p = state.persisted.lock().unwrap();
            p.active_config = None;
            p.started_at = None;
        }
        save_state(&app, &state);
        return RunConfigResult {
            ok: false,
            error: Some("winws.exe не запустился, проверь конфиг вручную.".into()),
            live_logs: false,
        };
    }

    {
        let mut p = state.persisted.lock().unwrap();
        p.active_config = Some(file_name);
        p.installed_as_service = false;
        p.started_at = Some(now_ms());
    }
    save_state(&app, &state);

    RunConfigResult { ok: true, error: None, live_logs }
}

#[tauri::command(async)]
pub fn stop_config(app: AppHandle, state: State<AppState>) -> SimpleResult {
    winws::kill_winws(&app);
    {
        let mut p = state.persisted.lock().unwrap();
        p.active_config = None;
        p.started_at = None;
        // Службы после этой команды нет: её снимает отдельная кнопка, а
        // висящий флаг заставлял apply_config снова ставить конфиг службой.
        p.installed_as_service = false;
    }
    save_state(&app, &state);
    // Раньше команда всегда отвечала Ok: если taskkill не справился, окно
    // рапортовало «Обход остановлен» поверх работающего обхода.
    if winws::is_winws_running() {
        err("winws.exe не удалось остановить — возможно, его держит служба.")
    } else {
        ok()
    }
}

#[derive(Debug, Serialize)]
pub struct WinwsLogResult {
    lines: Vec<String>,
    live: bool,
}

#[tauri::command(async)]
pub fn get_winws_log(state: State<AppState>) -> WinwsLogResult {
    WinwsLogResult {
        lines: state.winws_log.lock().unwrap().clone(),
        live: state.winws_child.lock().unwrap().is_some(),
    }
}

// ─────────── Проверка связи ───────────

#[derive(Debug, Serialize)]
pub struct CheckGamesResult {
    ok: bool,
    targets: Vec<crate::targets::TargetResult>,
    pending: bool,
    #[serde(rename = "checkedAt")]
    checked_at: u64,
    running: bool,
    strategy: Option<String>,
}

#[tauri::command(async)]
pub fn check_games(state: State<AppState>) -> CheckGamesResult {
    let targets = {
        let p = state.persisted.lock().unwrap();
        p.game_targets.clone().unwrap_or_else(crate::targets::default_targets)
    };
    let results = crate::targets::check_targets(&targets);
    CheckGamesResult {
        ok: true,
        targets: results,
        pending: false,
        checked_at: now_ms(),
        running: winws::is_winws_running(),
        strategy: state.persisted.lock().unwrap().active_config.clone(),
    }
}

#[derive(Debug, Serialize)]
pub struct TargetsPayload {
    targets: Vec<crate::targets::Target>,
}

#[tauri::command(async)]
pub fn get_game_targets(state: State<AppState>) -> TargetsPayload {
    let p = state.persisted.lock().unwrap();
    TargetsPayload {
        targets: p.game_targets.clone().unwrap_or_else(crate::targets::default_targets),
    }
}

#[tauri::command(async)]
pub fn get_default_game_targets() -> TargetsPayload {
    TargetsPayload { targets: crate::targets::default_targets() }
}

#[derive(Debug, Serialize)]
pub struct SaveTargetsResult {
    ok: bool,
    targets: Vec<crate::targets::Target>,
}

#[tauri::command(async)]
pub fn save_game_targets(
    app: AppHandle,
    state: State<AppState>,
    targets: Vec<crate::targets::Target>,
) -> SaveTargetsResult {
    let clean: Vec<_> = targets
        .into_iter()
        .filter(|t| !t.name.trim().is_empty() && !t.host.trim().is_empty() && t.port > 0)
        .collect();
    {
        let mut p = state.persisted.lock().unwrap();
        p.game_targets = if clean.is_empty() { None } else { Some(clean) };
    }
    save_state(&app, &state);
    let p = state.persisted.lock().unwrap();
    SaveTargetsResult {
        ok: true,
        targets: p.game_targets.clone().unwrap_or_else(crate::targets::default_targets),
    }
}

#[tauri::command(async)]
pub fn reset_game_targets(app: AppHandle, state: State<AppState>) -> SaveTargetsResult {
    state.persisted.lock().unwrap().game_targets = None;
    save_state(&app, &state);
    SaveTargetsResult { ok: true, targets: crate::targets::default_targets() }
}

// ─────────── Тесты стратегий ───────────

#[derive(Debug, Serialize)]
pub struct RunTestsResult {
    ok: bool,
    error: Option<String>,
    text: String,
}

/// mode: "standard" | "dpi" | "funnel".
///
/// «funnel» — то, ради чего всё затевалось: сначала полный прогон DPI, потом
/// HTTP/Ping, но только по тем конфигам, что прошли DPI на 100%. Экономит
/// половину времени и не тратит его на заведомо пробитые блокировкой варианты.
#[tauri::command(async)]
pub fn run_tests(app: AppHandle, state: State<AppState>, mode: String) -> RunTestsResult {
    let root = match state.persisted.lock().unwrap().root_path.clone() {
        Some(r) => PathBuf::from(r),
        None => {
            return RunTestsResult { ok: false, error: Some("Сначала загрузи релиз zapret.".into()), text: String::new() }
        }
    };

    // Скрипт zapret сам отказывается работать при установленной службе
    // («Windows service 'zapret' is installed») и выходит, не написав файла
    // результатов, — а мы отвечали невнятным «файл результатов не найден».
    // Проверяем заранее, тем же условием, что run_config и автопрогон.
    if crate::service::service_conflict() {
        return RunTestsResult {
            ok: false,
            error: Some("Установлена служба Windows «zapret» — сначала сними её: скрипт тестов zapret при службе не работает.".into()),
            text: String::new(),
        };
    }

    // Команды выполняются параллельно, поэтому второй запуск надо отсечь
    // здесь: два прогона одновременно перетирали бы конфиг друг другу.
    // Тот же замок берёт и автопрогон — см. state::TestRun.
    let Some(run) = crate::state::TestRun::acquire(&state) else {
        // Замок занят либо другим прогоном, либо сбором адресов игры — и
        // человеку важно знать, чего именно ждать.
        let why = if crate::gamescan::scan_busy() {
            "Сейчас идёт сбор адресов игры — дождись его окончания."
        } else {
            "Тесты уже идут."
        };
        return RunTestsResult { ok: false, error: Some(why.into()), text: String::new() };
    };

    // Скрипт сам поднимает и гасит winws под каждый конфиг, так что к концу
    // прогона работает что угодно. Запоминаем, что было до.
    // Пара секунд до прогона стоят того: если разрез ClientHello тут не
    // пробивает, перебор двух десятков конфигов, скорее всего, впустую.
    // Не отменяем — человек попросил прогон, — но говорим прямо.
    let chance = bypass_chance(&state);
    let _ = app.emit("test-log", format!("Предпроверка: {}", chance.note));
    if let Some(t13) = &chance.tls13 {
        let _ = app.emit("test-log", format!("Предпроверка: {t13}"));
    }
    if let Some(r) = &chance.response {
        let _ = app.emit("test-log", format!("Предпроверка: {}", r.reason));
    }
    if let Some(v) = &chance.volume {
        let _ = app.emit("test-log", format!("Предпроверка: {}", v.note));
    }

    let before = state.persisted.lock().unwrap().active_config.clone();
    let result = run_tests_inner(&app, &run, &root, &mode);
    restore_after_tests(&app, before);
    result
}

/// Возвращает обход в то состояние, в котором он был до прогона. Без этого
/// окно и трей показывали стратегию, которой уже нет: `active_config` прогон
/// не трогает, а winws остаётся на последнем протестированном конфиге.
pub fn restore_after_tests(app: &AppHandle, before: Option<String>) {
    match before {
        Some(name) => {
            if let Err(e) = crate::monitor::apply_config(app, &name) {
                let _ = app.emit("test-log", format!("Не удалось вернуть {name}: {e}"));
            }
        }
        None => winws::kill_winws(app),
    }
}

fn run_tests_inner(
    app: &AppHandle,
    run: &crate::state::TestRun<'_>,
    root: &Path,
    mode: &str,
) -> RunTestsResult {
    if mode != "funnel" {
        return match crate::tests::run_full_with_retry(app, root, mode == "dpi", || run.cancelled()) {
            Ok(text) => RunTestsResult { ok: true, error: None, text },
            Err(e) => RunTestsResult { ok: false, error: Some(e), text: String::new() },
        };
    }

    // Этап 1 — DPI по всем конфигам.
    let _ = app.emit("test-log", "── Этап 1: DPI-checker по всем конфигам ──".to_string());
    let dpi_text = match crate::tests::run_test_script(app, root, true, None) {
        Ok(t) => t,
        Err(e) => return RunTestsResult { ok: false, error: Some(e), text: String::new() },
    };

    // Между этапами живого процесса нет, и «Остановить» гасить нечего —
    // без этой проверки второй этап стартовал бы уже после отмены.
    if run.cancelled() {
        let _ = app.emit("test-log", "Прогон остановлен — второй этап не запускаем.".to_string());
        return RunTestsResult { ok: true, error: None, text: dpi_text };
    }

    let (dpi_rows, _) = crate::tests::parse_results(&dpi_text);
    let configs = crate::release::list_configs(root);

    // Номера для скрипта — позиция конфига в НАШЕМ списке. Это допущение:
    // мы считаем, что скрипт фильтрует папку и сортирует её так же, как мы
    // (natural sort по ASCII против сортировки PowerShell с учётом культуры).
    // Спросить у скрипта его собственную нумерацию нечем — меню мы не читаем,
    // ответы уходят в stdin одной пачкой. Поэтому запоминаем, что собирались
    // прогнать, и сверяем по именам в результате: разъехалась нумерация —
    // мы это увидим, а не подпишем чужие цифры своими именами.
    let mut passed: Vec<usize> = Vec::new();
    let mut wanted: Vec<String> = Vec::new();
    for r in dpi_rows.iter().filter(|r| r.score(true) >= 1.0) {
        let bare = r.config.trim_end_matches(".bat");
        if let Some(i) = configs.iter().position(|c| c.trim_end_matches(".bat") == bare) {
            passed.push(i + 1);
            wanted.push(bare.to_string());
        }
    }

    if passed.is_empty() {
        let _ = app.emit(
            "test-log",
            "Ни один конфиг не прошёл DPI полностью — второй этап пропущен.".to_string(),
        );
        return RunTestsResult { ok: true, error: None, text: dpi_text };
    }

    let _ = app.emit(
        "test-log",
        format!("── Этап 2: HTTP/Ping по {} конфигам, прошедшим DPI ──", passed.len()),
    );
    match crate::tests::run_test_script(app, root, false, Some(&passed)) {
        Ok(text) => match crate::tests::check_stage2(&wanted, &text) {
            crate::tests::Stage2::Ok => RunTestsResult { ok: true, error: None, text },
            // Конфиг, который не поднялся, скрипт пропускает сам и пишет об
            // этом «Strategy failed to start». Он просто отсутствует в файле —
            // на остальные строки это не влияет, и отбрасывать прогон незачем.
            crate::tests::Stage2::Skipped(missing) => {
                let _ = app.emit(
                    "test-log",
                    format!(
                        "Скрипт пропустил конфиги, они не запустились: {}. Остальное посчитано.",
                        missing.join(", ")
                    ),
                );
                RunTestsResult { ok: true, error: None, text }
            }
            // А вот чужие имена в результате — это уже разъехавшаяся
            // нумерация: подписать её нашими именами нельзя.
            crate::tests::Stage2::Mismatch => {
                let _ = app.emit(
                    "test-log",
                    "Второй этап прогнал не те конфиги — результат отброшен.".to_string(),
                );
                RunTestsResult {
                    ok: false,
                    error: Some(
                        concat!(
                            "Второй этап прогнал не те конфиги: нумерация в ",
                            "скрипте не совпала с нашей. Результаты DPI сохранены, ",
                            "а для HTTP запусти обычный прогон."
                        )
                            .into(),
                    ),
                    text: dpi_text,
                }
            }
            crate::tests::Stage2::Empty => {
                let _ = app.emit(
                    "test-log",
                    "Второй этап не дал ни одной строки результатов.".to_string(),
                );
                RunTestsResult {
                    ok: false,
                    error: Some(
                        "Второй этап не дал результатов. Показаны результаты DPI.".into(),
                    ),
                    text: dpi_text,
                }
            }
        },
        Err(e) => RunTestsResult { ok: false, error: Some(e), text: dpi_text },
    }
}

/// Иконка сервиса для списка поиска. Отдельной командой, а не вместе с
/// каталогом: иконок десятки, каждая — поход в сеть, и ждать их все ради
/// показа списка нельзя. Окно запрашивает их по одной, когда строка уже
/// нарисована.
#[derive(Debug, Serialize)]
pub struct Favicon {
    pub ok: bool,
    #[serde(rename = "dataUri")]
    pub data_uri: Option<String>,
}

#[tauri::command(async)]
pub fn get_favicon(app: AppHandle, host: String) -> Favicon {
    match crate::favicon::get(&app, &host) {
        Ok(Some(uri)) => Favicon { ok: true, data_uri: Some(uri) },
        _ => Favicon { ok: false, data_uri: None },
    }
}

#[derive(Debug, Serialize)]
pub struct LastResults {
    ok: bool,
    text: String,
}

#[tauri::command(async)]
pub fn get_last_test_results(state: State<AppState>) -> LastResults {
    let root = match state.persisted.lock().unwrap().root_path.clone() {
        Some(r) => PathBuf::from(r),
        None => return LastResults { ok: false, text: String::new() },
    };
    // Тот же выбор «самого свежего», что и после прогона: по времени
    // изменения. Раньше здесь был алфавит — а имена задаёт чужой скрипт.
    match crate::tests::newest_result_file(&root).and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => LastResults { ok: true, text },
        None => LastResults { ok: false, text: String::new() },
    }
}

#[tauri::command(async)]
pub fn stop_tests(state: State<AppState>) {
    // Флаг ставим до убийства: воронка между этапами живого процесса не
    // имеет, и увидеть отмену она может только так.
    *state.test_cancel.lock().unwrap() = true;
    // Гасим ровно то дерево, которое сами и запустили: убивать все
    // powershell.exe нельзя — у пользователя могут быть свои открытые окна.
    let pid = *state.test_pid.lock().unwrap();
    if let Some(pid) = pid {
        crate::sys::run("taskkill", &["/PID", &pid.to_string(), "/T", "/F"]);
    }
    // Скрипт поднимает winws.exe сам, отдельным процессом — он переживёт
    // смерть powershell, если его не тронуть.
    winws::stop_winws();
    // `testing` снимает владелец прогона (state::TestRun) на выходе. Снимать
    // его здесь значило бы пустить второй прогон, пока первый ещё
    // сворачивается, — и его PID тут же затёрся бы хвостом первого.
}

// ─────────── Служба Windows ───────────

#[derive(Debug, Serialize)]
pub struct SimpleResult {
    ok: bool,
    error: Option<String>,
}

fn ok() -> SimpleResult {
    SimpleResult { ok: true, error: None }
}
fn err(e: impl Into<String>) -> SimpleResult {
    SimpleResult { ok: false, error: Some(e.into()) }
}

fn root_of(state: &State<AppState>) -> Option<PathBuf> {
    state.persisted.lock().unwrap().root_path.clone().map(PathBuf::from)
}

/// Имя конфига должно быть ровно одним из тех, что мы сами показали в
/// списке. Без этого строка из интерфейса уезжала в `cmd /c <имя>` —
/// запасной путь запуска в winws.rs и установка службой, — а cmd разбирает
/// свою командную строку заново: «general&calc.exe» выполнило бы вторую
/// команду от имени администратора.
pub fn checked_config(root: &Path, file_name: &str) -> Result<(), String> {
    // Членства в списке НЕДОСТАТОЧНО. Список читается с диска, а содержимое
    // папки задаёт архив, который пользователь мог взять где угодно. Файл с
    // именем «x&calc.bat» там вполне может лежать — и тогда cmd.exe в
    // резервной ветке запуска выполнит вторую команду от администратора.
    //
    // Скобок здесь НЕТ, и это важно: у Flowseal так названы почти все
    // конфиги — «general (ALT11).bat». Когда они стояли в списке, запуск
    // отваливался на любом из них, то есть практически на всём релизе.
    // Опасности в них и нет: в cmd скобки группируют команды только в
    // начале выражения, а в аргументе остаются обычными символами — тем
    // более что имя с пробелом уезжает в кавычках.
    if file_name.contains(|c| "&|<>^\"'`%!\r\n".contains(c)) {
        return Err(format!("Недопустимые символы в имени конфига: {file_name}"));
    }
    if crate::release::list_configs(root).iter().any(|c| c == file_name) {
        Ok(())
    } else {
        Err(format!("Нет такого конфига в релизе: {file_name}"))
    }
}

/// Имя файла или папки, которое пришло из интерфейса и будет приклеено к
/// нашему каталогу. Кроме «..» и разделителей отсекаем префикс диска:
/// в Windows `Path::join("C:foo")` выбрасывает базовый путь целиком.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        // «.» и «..» — не имена: join(".") оставляет путь на самом каталоге,
        // и remove_dir_all снёс бы всё его содержимое.
        && name != "."
        && !name.contains("..")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
}

#[tauri::command(async)]
pub fn install_service(app: AppHandle, state: State<AppState>, file_name: String) -> SimpleResult {
    let root = match root_of(&state) {
        Some(r) => r,
        None => return err("Сначала загрузи релиз zapret."),
    };
    if let Err(e) = checked_config(&root, &file_name) {
        return err(e);
    }
    winws::kill_winws(&app);
    std::thread::sleep(std::time::Duration::from_millis(300));
    match crate::service::install_service(&root, &file_name) {
        Ok(()) => {
            {
                let mut p = state.persisted.lock().unwrap();
                p.active_config = Some(file_name);
                p.installed_as_service = true;
                p.started_at = Some(now_ms());
            }
            save_state(&app, &state);
            crate::tray::refresh(&app);
            ok()
        }
        Err(e) => err(e),
    }
}

#[tauri::command(async)]
pub fn remove_service(app: AppHandle, state: State<AppState>) -> SimpleResult {
    winws::kill_winws(&app);
    crate::service::remove_service();
    {
        let mut p = state.persisted.lock().unwrap();
        p.installed_as_service = false;
        p.active_config = None;
        p.started_at = None;
    }
    save_state(&app, &state);
    crate::tray::refresh(&app);
    // Раньше здесь был безусловный успех. `sc delete` при открытом окне
    // «Службы» только помечает службу к удалению, и она висит в системе до
    // закрытия всех дескрипторов или перезагрузки. Интерфейс рапортовал «снята»,
    // а следующий запуск упирался в неё же: «Установлена служба — сначала
    // сними её», и выхода из этого круга человек не видел.
    if crate::service::service_conflict() {
        return err(
            "Служба «zapret» остановлена, но Windows её ещё не удалила. Так бывает, когда \
             открыто окно «Службы» или диспетчер задач на вкладке служб: закрой их и \
             повтори. Не поможет — перезагрузи компьютер.",
        );
    }
    ok()
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    #[serde(rename = "serviceExists")]
    service_exists: bool,
    #[serde(rename = "serviceState")]
    service_state: Option<String>,
    #[serde(rename = "windivertState")]
    windivert_state: Option<String>,
    strategy: Option<String>,
    #[serde(rename = "winwsRunning")]
    winws_running: bool,
}

#[tauri::command(async)]
pub fn get_service_status() -> ServiceStatus {
    let svc = crate::sys::svc_query("zapret");
    let wd = crate::sys::svc_query("WinDivert");
    ServiceStatus {
        service_exists: svc.exists,
        service_state: svc.state,
        windivert_state: wd.state,
        strategy: crate::sys::installed_service_strategy(),
        winws_running: winws::is_winws_running(),
    }
}

// ─────────── Тумблеры релиза ───────────

#[tauri::command(async)]
pub fn get_toggles(state: State<AppState>) -> serde_json::Value {
    match root_of(&state) {
        Some(root) => serde_json::to_value(crate::toggles::read_toggles(&root)).unwrap_or(serde_json::json!({})),
        None => serde_json::json!({}),
    }
}

#[tauri::command(async)]
pub fn set_game_filter(state: State<AppState>, mode: String) -> SimpleResult {
    match root_of(&state) {
        Some(root) => match crate::toggles::set_game_filter(&root, &mode) {
            Ok(()) => ok(),
            Err(e) => err(e),
        },
        None => err("Сначала загрузи релиз zapret."),
    }
}

#[tauri::command(async)]
pub fn cycle_ipset_mode(state: State<AppState>) -> SimpleResult {
    match root_of(&state) {
        Some(root) => match crate::toggles::cycle_ipset(&root) {
            Ok(()) => ok(),
            Err(e) => err(e),
        },
        None => err("Сначала загрузи релиз zapret."),
    }
}

#[tauri::command(async)]
pub fn set_auto_update(state: State<AppState>, enabled: bool) -> SimpleResult {
    match root_of(&state) {
        Some(root) => match crate::toggles::set_auto_update(&root, enabled) {
            Ok(()) => ok(),
            Err(e) => err(e),
        },
        None => err("Сначала загрузи релиз zapret."),
    }
}

// ─────────── Автозапуск ───────────

#[derive(Debug, Serialize)]
pub struct AutostartState {
    enabled: bool,
}

#[tauri::command(async)]
pub fn get_autostart() -> AutostartState {
    AutostartState { enabled: crate::autostart::is_enabled() }
}

#[tauri::command(async)]
pub fn set_autostart(enabled: bool) -> SimpleResult {
    match crate::autostart::set_enabled(enabled) {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

// ─────────── Диагностика системы ───────────

#[derive(Debug, Serialize)]
pub struct DiagResult {
    ok: bool,
    results: Vec<crate::diag::DiagRow>,
}

#[tauri::command(async)]
pub fn run_diagnostics(state: State<AppState>, deep: Option<bool>) -> DiagResult {
    let root = root_of(&state);
    DiagResult {
        ok: true,
        results: crate::diag::run_diagnostics(root.as_deref(), deep.unwrap_or(false)),
    }
}

#[tauri::command(async)]
pub fn fix_diagnostic(key: String) -> SimpleResult {
    match crate::diag::fix(&key) {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

// ─────────── Самолечение / автопереключение ───────────

#[derive(Debug, Serialize)]
pub struct AutoSwitchState {
    enabled: bool,
    threshold: u32,
    #[serde(rename = "intervalSec")]
    interval_sec: u64,
    #[serde(rename = "hasRanking")]
    has_ranking: bool,
}

#[tauri::command(async)]
pub fn get_auto_switch(state: State<AppState>) -> AutoSwitchState {
    let p = state.persisted.lock().unwrap();
    let a = p.auto_switch.clone().unwrap_or_default();
    let has_ranking = p
        .root_path
        .as_ref()
        .map(|r| Path::new(r).join("utils").join("test results").exists())
        .unwrap_or(false);
    AutoSwitchState {
        enabled: a.enabled,
        threshold: a.threshold,
        interval_sec: a.interval_sec,
        has_ranking,
    }
}

#[tauri::command(async)]
pub fn set_auto_switch(
    app: AppHandle,
    state: State<AppState>,
    enabled: bool,
    threshold: Option<u32>,
    interval_sec: Option<u64>,
) -> SimpleResult {
    {
        let mut p = state.persisted.lock().unwrap();
        let mut a = p.auto_switch.clone().unwrap_or_default();
        a.enabled = enabled;
        if let Some(t) = threshold {
            a.threshold = t.clamp(2, 10);
        }
        if let Some(i) = interval_sec {
            a.interval_sec = if [30, 60, 300].contains(&i) { i } else { a.interval_sec };
        }
        p.auto_switch = Some(a);
    }
    *state.degraded_ticks.lock().unwrap() = 0;
    state.healing_attempts.lock().unwrap().clear();
    *state.heal_exhausted.lock().unwrap() = false;
    save_state(&app, &state);
    ok()
}

#[derive(Debug, Serialize)]
pub struct HealLog {
    ok: bool,
    entries: Vec<crate::state::HealEntry>,
    /// Последняя стратегия, на которой проверка прошла чисто, и когда это
    /// было. Не то же самое, что «включено сейчас».
    #[serde(rename = "workingConfig", skip_serializing_if = "Option::is_none")]
    working_config: Option<String>,
    #[serde(rename = "workingAt", skip_serializing_if = "Option::is_none")]
    working_at: Option<u64>,
}

#[tauri::command(async)]
pub fn get_heal_log(state: State<AppState>) -> HealLog {
    let p = state.persisted.lock().unwrap();
    HealLog {
        ok: true,
        entries: p.heal_log.clone().unwrap_or_default(),
        working_config: p.working_config.clone(),
        working_at: p.working_at,
    }
}

// ─────────── Мелкие настройки ───────────

#[tauri::command(async)]
pub fn get_onboarding_done(state: State<AppState>) -> bool {
    state.persisted.lock().unwrap().onboarding_done.unwrap_or(false)
}

#[tauri::command(async)]
pub fn set_onboarding_done(app: AppHandle, state: State<AppState>, done: bool) -> SimpleResult {
    state.persisted.lock().unwrap().onboarding_done = Some(done);
    save_state(&app, &state);
    ok()
}

#[derive(Debug, Serialize)]
pub struct NotifyState {
    enabled: bool,
    /// Тосты есть в любой поддерживаемой Windows (10+); поле ждёт renderer.
    supported: bool,
}

#[tauri::command(async)]
pub fn get_notifications(state: State<AppState>) -> NotifyState {
    NotifyState { enabled: state.persisted.lock().unwrap().notifications.unwrap_or(true), supported: true }
}

#[tauri::command(async)]
pub fn set_notifications(app: AppHandle, state: State<AppState>, enabled: bool) -> SimpleResult {
    state.persisted.lock().unwrap().notifications = Some(enabled);
    save_state(&app, &state);
    ok()
}

// ─────────── Telegram-прокси ───────────

#[tauri::command(async)]
pub fn start_tgwsproxy(app: AppHandle) -> SimpleResult {
    match crate::tgws::start(&app) {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

#[tauri::command(async)]
pub fn stop_tgwsproxy(app: AppHandle) -> SimpleResult {
    crate::tgws::stop(&app);
    ok()
}

#[tauri::command(async)]
pub fn restart_tgwsproxy(app: AppHandle, state: State<AppState>) -> SimpleResult {
    let port = state.persisted.lock().unwrap().tgws.as_ref().map(|t| t.port).unwrap_or(0);
    crate::tgws::stop(&app);
    // taskkill возвращается раньше, чем освобождается сокет. Фиксированные
    // 400 мс не гарантировали ничего: новый процесс падал на «address in
    // use» уже после того, как команда отвечала ok, и статус залипал.
    for _ in 0..40 {
        if port == 0 || crate::tgws::pid_listening_on(port).is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    match crate::tgws::start(&app) {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

#[derive(Debug, Serialize)]
pub struct TgStatus {
    running: bool,
    healthy: bool,
    available: bool,
    host: String,
    port: u16,
    autostart: bool,
    /// Готовая ссылка для Telegram — кнопка «Скопировать» берёт её отсюда.
    #[serde(rename = "tgProxyUrl")]
    tg_proxy_url: String,
}

#[tauri::command(async)]
pub fn get_tgwsproxy_status(app: AppHandle, state: State<AppState>) -> TgStatus {
    crate::tgws::ensure_settings(&app);
    let s = state.persisted.lock().unwrap().tgws.clone().unwrap_or_default();
    let running = state.tgws_pid.lock().unwrap().is_some();
    let healthy = running && crate::tgws::probe_health(&s);
    // Тот же поиск, что и у запуска: раньше здесь была своя пара путей, и
    // она расходилась с exe_path() — окно писало «недоступен» там, где
    // запуск бы сработал, и наоборот.
    let available = crate::tgws::exe_path(&app).exists();
    let tg_proxy_url = crate::tgws::proxy_url(&s);
    TgStatus { running, healthy, available, host: s.host, port: s.port, autostart: s.auto_start, tg_proxy_url }
}

#[derive(Debug, Serialize)]
pub struct TgLog {
    lines: Vec<String>,
}

#[tauri::command(async)]
pub fn get_tgwsproxy_log(state: State<AppState>) -> TgLog {
    TgLog { lines: state.tgws_log.lock().unwrap().clone() }
}

/// Отдаём TgSettings как есть — renderer ждёт именно эти поля
/// (dcIps массивом, cfproxy, autoStart), как было в Electron-версии.
#[tauri::command(async)]
pub fn get_tgwsproxy_settings(app: AppHandle, state: State<AppState>) -> crate::tgws::TgSettings {
    crate::tgws::ensure_settings(&app);
    state.persisted.lock().unwrap().tgws.clone().unwrap_or_default()
}

#[tauri::command(async)]
pub fn set_tgwsproxy_settings(
    app: AppHandle,
    state: State<AppState>,
    host: Option<String>,
    port: Option<u16>,
    secret: Option<String>,
    dc_ips: Option<String>,
    cfproxy: Option<bool>,
) -> SimpleResult {
    // Telegram принимает только 16 байт hex после «dd» — любой другой секрет
    // даёт «неправильную ссылку», поэтому не сохраняем его вовсе.
    let secret = match secret.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) {
        None => None,
        Some(sec) => {
            let sec = if sec.len() == 34 && sec.starts_with("dd") { sec[2..].to_string() } else { sec };
            if sec.len() != 32 || !sec.chars().all(|c| c.is_ascii_hexdigit()) {
                return err("Секрет должен состоять из 32 шестнадцатеричных символов (0-9, a-f).");
            }
            Some(sec)
        }
    };
    {
        let mut p = state.persisted.lock().unwrap();
        let mut s = p.tgws.clone().unwrap_or_default();
        if let Some(h) = host.filter(|h| !h.trim().is_empty()) {
            s.host = h.trim().to_string();
        }
        if let Some(pt) = port.filter(|p| *p > 0) {
            s.port = pt;
        }
        if let Some(sec) = secret {
            s.secret = sec;
        }
        if let Some(d) = dc_ips {
            let list: Vec<String> = d
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if !list.is_empty() {
                s.dc_ips = list;
            }
        }
        if let Some(cf) = cfproxy {
            s.cfproxy = cf;
        }
        p.tgws = Some(s);
    }
    save_state(&app, &state);
    ok()
}

#[tauri::command(async)]
pub fn set_tgwsproxy_autostart(app: AppHandle, state: State<AppState>, enabled: bool) -> SimpleResult {
    {
        let mut p = state.persisted.lock().unwrap();
        let mut s = p.tgws.clone().unwrap_or_default();
        s.auto_start = enabled;
        p.tgws = Some(s);
    }
    save_state(&app, &state);
    ok()
}

#[derive(Debug, Serialize)]
pub struct SecretResult {
    ok: bool,
    secret: String,
}

#[tauri::command(async)]
pub fn regenerate_tgwsproxy_secret(app: AppHandle, state: State<AppState>) -> SecretResult {
    let secret = crate::tgws::random_secret();
    {
        let mut p = state.persisted.lock().unwrap();
        let mut s = p.tgws.clone().unwrap_or_default();
        s.secret = secret.clone();
        p.tgws = Some(s);
    }
    save_state(&app, &state);
    SecretResult { ok: true, secret }
}

#[tauri::command(async)]
pub fn open_tg_proxy_link(app: AppHandle, state: State<AppState>) -> SimpleResult {
    crate::tgws::ensure_settings(&app);
    let s = state.persisted.lock().unwrap().tgws.clone().unwrap_or_default();
    open_url(&crate::tgws::proxy_url(&s))
}

// ─────────── Ссылки и файлы ───────────

/// Напрямую через ShellExecuteW, а не `cmd /c start`: cmd разбирает строку
/// заново и режет её по `&`, так что tg://proxy?server=…&port=…&secret=…
/// доезжал до Telegram без порта и секрета — отсюда «неправильная ссылка».
fn open_url(target: &str) -> SimpleResult {
    if shell_open(target) {
        ok()
    } else {
        err("Не удалось открыть ссылку — нет приложения, которое её обрабатывает.")
    }
}

/// Файлы и папки — тем же ShellExecuteW. `explorer <путь>` из Klutz,
/// запущенного от администратора, молча ничего не открывал: код возврата
/// explorer не значит ничего, и «Открыть» у снимка прогона не срабатывала.
fn open_path(path: &std::path::Path) -> SimpleResult {
    if shell_open(&path.to_string_lossy()) {
        ok()
    } else {
        err("Не удалось открыть — Windows не нашла, чем.")
    }
}

#[cfg(target_os = "windows")]
fn shell_open(target: &str) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let verb = wide("open");
    let file = wide(target);
    let res = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute сигналит успех значением больше 32.
    res as isize > 32
}

#[cfg(not(target_os = "windows"))]
fn shell_open(_target: &str) -> bool {
    false
}

/// Через плагин буфера обмена на стороне Rust: navigator.clipboard в WebView2
/// отказывает в записи, из-за чего копирование молча не срабатывало.
#[tauri::command(async)]
pub fn copy_text(app: AppHandle, text: String) -> SimpleResult {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    match app.clipboard().write_text(text) {
        Ok(()) => ok(),
        Err(e) => err(e.to_string()),
    }
}

#[tauri::command(async)]
pub fn open_external_url(url: String) -> SimpleResult {
    if !url.starts_with("https://") && !url.starts_with("http://") && !url.starts_with("tg://") {
        return err("Недопустимая ссылка.");
    }
    open_url(&url)
}

#[tauri::command(async)]
pub fn open_release_folder(state: State<AppState>) -> SimpleResult {
    match root_of(&state) {
        Some(root) => open_path(&root),
        None => err("Релиз не загружен."),
    }
}

#[tauri::command(async)]
pub fn open_result_file(
    app: AppHandle,
    state: State<AppState>,
    file_name: String,
    release: Option<String>,
) -> SimpleResult {
    // Только внутри папки результатов — имя приходит из интерфейса, но
    // проверить дешевле, чем доверять.
    if !safe_name(&file_name) {
        return err("Недопустимое имя файла.");
    }
    // Прогон из архива Klutz: у него свой релиз, и файл лежит там, а не в
    // папке текущего релиза, — после смены версии zapret его там нет вовсе.
    if let Some(rel) = release.filter(|r| safe_name(r)) {
        let p = crate::history::history_dir(&app).join(&rel).join(&file_name);
        if p.exists() {
            return open_path(&p);
        }
    }
    match root_of(&state) {
        Some(root) => {
            let p = root.join("utils").join("test results").join(&file_name);
            if !p.exists() {
                return err("Файл не найден.");
            }
            open_path(&p)
        }
        None => err("Релиз не загружен."),
    }
}

// ─────────── История прогонов ───────────

#[derive(Debug, Serialize)]
pub struct HistoryRun {
    date: String,
    file: String,
    /// Релиз, на котором шёл прогон, — имя его папки.
    release: String,
    best: Option<String>,
    mode: String,
    /// Сколько целей прошла лучшая строка прогона и сколько их было всего.
    /// Раньше здесь лежало `(доля * 7).round()` — семь целей было в
    /// Electron-версии, а сейчас их число задаёт релиз и режим.
    #[serde(rename = "bestOk")]
    best_ok: u32,
    #[serde(rename = "bestTotal")]
    best_total: u32,
}

#[derive(Debug, Serialize)]
pub struct HistoryConfig {
    name: String,
    #[serde(rename = "latestShare")]
    latest_share: Option<f64>,
    #[serde(rename = "shareSeries")]
    share_series: Vec<f64>,
    wins: u32,
}

#[derive(Debug, Serialize)]
pub struct HistoryResult {
    ok: bool,
    runs: Vec<HistoryRun>,
    configs: Vec<HistoryConfig>,
}

#[tauri::command(async)]
pub fn get_test_history(app: AppHandle, state: State<AppState>) -> HistoryResult {
    // Прогоны текущего релиза — в архив Klutz, а читается история оттуда, по
    // всем релизам разом. Раньше она читалась из папки релиза и пропадала
    // при каждой смене версии zapret.
    if let Some(root) = root_of(&state) {
        crate::history::archive(&app, &root);
    }
    // Порядок «от старого к новому» — по времени изменения. Имена файлов
    // задаёт чужой скрипт, и их алфавит не обязан совпадать с хронологией.
    let archived = crate::history::runs(&app);

    let mut runs = Vec::new();
    // config -> доли по прогонам, в порядке от старого к новому
    let mut series: std::collections::BTreeMap<String, Vec<f64>> = Default::default();
    let mut wins: std::collections::BTreeMap<String, u32> = Default::default();

    for run in &archived {
        let f = &run.name;
        let Ok(text) = std::fs::read_to_string(&run.path) else { continue };
        let (rows, dpi) = crate::tests::parse_results(&text);
        if rows.is_empty() {
            continue;
        }
        let best_score = rows.iter().map(|r| r.score(dpi)).fold(0.0_f64, f64::max);
        // Тот же порядок, что у трея и самолечения, — иначе «лучшая» в
        // истории и «лучшая» в переключении могут разойтись.
        let best_row = rows.iter().min_by(|a, b| crate::tests::rank_desc(a, b, dpi));
        let best = best_row.map(|r| r.config.clone());
        if let Some(b) = &best {
            *wins.entry(b.clone()).or_insert(0) += 1;
        }
        for r in &rows {
            // Доля от лучшего в прогоне: иначе HTTP и DPI с разными
            // максимумами между собой не сравнить.
            let share = if best_score > 0.0 { r.score(dpi) / best_score } else { 0.0 };
            series.entry(r.config.clone()).or_default().push(share);
        }
        runs.push(HistoryRun {
            date: f.trim_end_matches(".txt").to_string(),
            file: f.clone(),
            release: run.release.clone(),
            best,
            mode: if dpi { "dpi".into() } else { "standard".into() },
            best_ok: best_row.map(|r| r.ok).unwrap_or(0),
            best_total: best_row.map(|r| r.total(dpi)).unwrap_or(0),
        });
    }

    let configs = series
        .into_iter()
        .map(|(name, s)| HistoryConfig {
            latest_share: s.last().copied(),
            share_series: s,
            wins: *wins.get(&name).unwrap_or(&0),
            name,
        })
        .collect();

    HistoryResult { ok: true, runs, configs }
}

// ─────────── Обслуживание ───────────

#[tauri::command(async)]
pub fn update_ipset_list(state: State<AppState>) -> serde_json::Value {
    match root_of(&state) {
        Some(root) => serde_json::to_value(crate::maintenance::update_ipset(&root)).unwrap_or_default(),
        None => serde_json::json!({ "ok": false, "error": "Сначала загрузи релиз zapret." }),
    }
}

#[tauri::command(async)]
pub fn update_hosts_file() -> serde_json::Value {
    serde_json::to_value(crate::maintenance::update_hosts()).unwrap_or_default()
}

#[tauri::command(async)]
pub fn check_updates(state: State<AppState>) -> serde_json::Value {
    match root_of(&state) {
        Some(root) => serde_json::to_value(crate::maintenance::check_updates(&root)).unwrap_or_default(),
        None => serde_json::json!({ "ok": false, "error": "Сначала загрузи релиз zapret." }),
    }
}

// ─────────── Версии компонентов ───────────

#[derive(Debug, Serialize)]
pub struct Versions {
    /// Версия Klutz — единственный источник: tauri.conf.json / Cargo.toml.
    app: String,
    zapret: Option<String>,
    tgws: &'static str,
}

#[tauri::command(async)]
pub fn get_versions(app: AppHandle, state: State<AppState>) -> Versions {
    Versions {
        app: app.package_info().version.to_string(),
        zapret: root_of(&state).and_then(|r| crate::maintenance::local_version(&r)),
        tgws: crate::tgws::BUNDLED_VERSION,
    }
}

#[derive(Debug, Serialize)]
pub struct ComponentUpdate {
    current: Option<String>,
    latest: Option<String>,
    error: Option<String>,
    /// Где взять новую версию — страница релиза.
    url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ComponentUpdates {
    klutz: ComponentUpdate,
    zapret: ComponentUpdate,
    tgws: ComponentUpdate,
}

/// Репозиторий самого Klutz: выпуски — GitHub-релизы с тегом vX.Y.Z.
const KLUTZ_REPO: &str = "vbu00/zapret-klutz";

/// Последний релиз Klutz на GitHub против версии этой сборки.
fn klutz_update(app: &AppHandle) -> ComponentUpdate {
    let current = Some(app.package_info().version.to_string());
    let fetched = crate::maintenance::http_get(&format!("https://api.github.com/repos/{KLUTZ_REPO}/releases/latest"))
        .and_then(|text| {
            serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|_| "GitHub ответил неожиданным форматом.".to_string())
        });
    match fetched {
        Ok(v) => {
            let latest = v.get("tag_name").and_then(|t| t.as_str()).map(|t| t.trim_start_matches('v').to_string());
            let url = v.get("html_url").and_then(|u| u.as_str()).map(str::to_string);
            let error = latest.is_none().then(|| "В ответе GitHub нет версии.".to_string());
            ComponentUpdate { current, latest, error, url }
        }
        Err(e) => ComponentUpdate { current, latest: None, error: Some(e), url: None },
    }
}

/// Проверка при запуске — только Klutz, без zapret и прокси.
#[tauri::command(async)]
pub fn check_klutz_update(app: AppHandle) -> ComponentUpdate {
    klutz_update(&app)
}

/// Сверяет Klutz и обе встроенные части с последними версиями на GitHub.
#[tauri::command(async)]
pub fn check_component_updates(app: AppHandle, state: State<AppState>) -> ComponentUpdates {
    let zapret = match root_of(&state) {
        Some(root) => {
            let u = crate::maintenance::check_updates(&root);
            ComponentUpdate {
                current: Some(u.local),
                latest: if u.ok { Some(u.remote) } else { None },
                error: u.error,
                url: if u.release_url.is_empty() { None } else { Some(u.release_url) },
            }
        }
        None => ComponentUpdate {
            current: None,
            latest: None,
            error: Some("Релиз zapret не загружен.".into()),
            url: None,
        },
    };
    let current = Some(crate::tgws::BUNDLED_VERSION.to_string());
    let tgws_url = Some("https://github.com/Flowseal/tg-ws-proxy/releases/latest".to_string());
    let tgws = match crate::tgws::latest_upstream() {
        Ok(latest) => ComponentUpdate { current, latest: Some(latest), error: None, url: tgws_url },
        Err(e) => ComponentUpdate { current, latest: None, error: Some(e), url: tgws_url },
    };
    ComponentUpdates { klutz: klutz_update(&app), zapret, tgws }
}

#[tauri::command(async)]
pub fn clear_discord_cache() -> serde_json::Value {
    serde_json::to_value(crate::maintenance::clear_discord_cache()).unwrap_or_default()
}

#[tauri::command(async)]
pub fn get_custom_lists(state: State<AppState>) -> serde_json::Value {
    match root_of(&state) {
        Some(root) => serde_json::to_value(crate::maintenance::get_custom_lists(&root)).unwrap_or_default(),
        None => serde_json::json!({ "ok": false, "include": "", "exclude": "" }),
    }
}

#[tauri::command(async)]
pub fn save_custom_lists(state: State<AppState>, include: String, exclude: String) -> SimpleResult {
    match root_of(&state) {
        Some(root) => match crate::maintenance::save_custom_lists(&root, &include, &exclude) {
            Ok(()) => ok(),
            Err(e) => err(e),
        },
        None => err("Сначала загрузи релиз zapret."),
    }
}

// ─────────── Поможет ли обход ───────────

#[derive(Debug, Serialize)]
pub struct ChanceTarget {
    name: String,
    host: String,
    verdict: crate::tlsprobe::FragVerdict,
    note: String,
}

#[derive(Debug, Serialize)]
pub struct BypassChance {
    verdict: crate::tlsprobe::FragVerdict,
    note: String,
    targets: Vec<ChanceTarget>,
    /// Не режут ли ОТВЕТ сервера — проверяется по одной цели.
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<crate::tlsprobe::ResponseResult>,
    /// Не нацелена ли блокировка именно на TLS 1.3.
    #[serde(skip_serializing_if = "Option::is_none")]
    tls13: Option<String>,
    /// Доезжает ли ответ целиком. Заполняется, только когда есть что
    /// сказать: «перелезли через потолок» — не новость.
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<crate::probe::VolumeResult>,
}

/// Отвечает за пару секунд на вопрос, ради которого иначе пришлось бы гнать
/// весь прогон: пробивает ли разрез ClientHello на этой сети.
///
/// Берём только ключевые цели и не больше трёх — каждая стоит одного-двух
/// соединений, а вывод от четвёртой уже не меняется.
fn bypass_chance(state: &State<AppState>) -> BypassChance {
    let list = {
        let p = state.persisted.lock().unwrap();
        p.game_targets.clone().unwrap_or_else(crate::targets::default_targets)
    };
    // По одной цели на сервис, а не первые три подряд: стандартный список
    // начинается с четырёх Discord, и `take(3)` не включал YouTube вообще —
    // а он вполне может дать противоположный ответ.
    let mut core: Vec<crate::targets::Target> = Vec::new();
    for want in ["discord", "youtube"] {
        if let Some(t) = list
            .iter()
            .find(|t| t.port == 443 && t.name.to_lowercase().starts_with(want))
        {
            core.push(t.clone());
        }
    }
    // Все цели переименованы — берём первые попавшиеся, иначе предпроверка
    // молча отвечала бы «не удалось» на полностью исправной сети.
    if core.is_empty() {
        core = list.iter().filter(|t| t.port == 443).take(2).cloned().collect();
    }

    let timeout = std::time::Duration::from_secs(3);
    let mut targets = Vec::new();
    // Глубокие пробы (ответное направление и версия TLS) стоят по несколько
    // соединений каждая, поэтому гоняем их по ОДНОЙ цели — первой, до
    // которой достучались. Вывод от второй тот же, а время втрое.
    let mut deep: Option<(std::net::IpAddr, u16, String)> = None;

    for t in core {
        let Some(ip) = crate::probe::first_ip(&t.host, t.port) else { continue };
        let Ok(ip) = ip.parse::<std::net::IpAddr>() else { continue };
        let r = crate::tlsprobe::probe_fragmentation(ip, t.port, &t.host, timeout);
        if deep.is_none() {
            deep = Some((ip, t.port, t.host.clone()));
        }
        targets.push(ChanceTarget { name: t.name, host: t.host, verdict: r.verdict, note: r.note });
    }

    let (mut response, mut tls13, mut volume) = (None, None, None);
    if let Some((ip, port, host)) = deep {
        // Скачиваем ответ целиком: рукопожатие могло пройти безупречно, а
        // поток умереть на втором десятке килобайт. Все остальные пробы
        // этого не видят — они кончаются на рукопожатии.
        let v = crate::probe::http_probe_volume(&host, port, Some(&ip.to_string()), timeout.as_secs().max(10));
        if v.verdict != crate::probe::VolumeVerdict::Clear {
            volume = Some(v);
        }
        // Про ответное направление говорить осмысленно только там, где
        // запрос проходит: если режут запрос, до ответа дело не доходит.
        let (v13, why13) = crate::tlsprobe::probe_tls13_block(ip, port, &host, timeout);
        if v13 != crate::tlsprobe::Tls13Verdict::Ok {
            tls13 = Some(why13);
        }
        let r = crate::tlsprobe::probe_response_direction(ip, port, &host, 2, timeout);
        if r.verdict != crate::tlsprobe::RespVerdict::Clear {
            response = Some(r);
        }
    }

    let (verdict, note) =
        crate::tlsprobe::aggregate(&targets.iter().map(|t| t.verdict).collect::<Vec<_>>());
    BypassChance { verdict, note, targets, response, tls13, volume }
}

#[tauri::command(async)]
pub fn check_bypass_chance(state: State<AppState>) -> BypassChance {
    bypass_chance(&state)
}

// ─────────── Сканирование трафика игры ───────────

#[derive(Debug, Serialize)]
pub struct GameScanState {
    /// Сколько адресов игр уже лежит в списке релиза.
    saved: u32,
    /// Сами адреса — раздел показывает их списком, чтобы человек видел, что
    /// именно попало в обход, а не одно число.
    addrs: Vec<String>,
    /// Применяется ли этот список вообще: при выключенном Game Filter
    /// игровые порты через обход не идут, и адреса там лежат впустую.
    #[serde(rename = "gameFilter")]
    game_filter: String,
    /// Когда список последний раз менялся. `null` — списка ещё нет.
    #[serde(rename = "changedAt")]
    changed_at: Option<u64>,
    /// Адреса, разложенные по операторам: чьи они и когда пойманы.
    groups: Vec<crate::gamescan::Group>,
    /// Что в список не пошло и почему.
    skipped: Vec<crate::gamescan::Skipped>,
}

#[tauri::command(async)]
pub fn get_game_scan(state: State<AppState>) -> GameScanState {
    let Some(root) = root_of(&state) else {
        return GameScanState {
            saved: 0,
            addrs: Vec::new(),
            game_filter: String::new(),
            changed_at: None,
            groups: Vec::new(),
            skipped: Vec::new(),
        };
    };
    let addrs = crate::gamescan::saved_ips(&root);
    let (groups, skipped) = crate::gamescan::parse_groups(
        &std::fs::read_to_string(root.join("lists").join("ipset-all.txt")).unwrap_or_default(),
    );
    GameScanState {
        saved: addrs.len() as u32,
        addrs,
        game_filter: crate::toggles::current_game_filter(&root),
        changed_at: crate::gamescan::changed_at(&root, crate::gamescan::Target::Bypass),
        groups,
        skipped,
    }
}

/// Выясняет, чей это набор сетей, и подписывает его оператором.
///
/// Нужно для списков, записанных прежней версией: там сети лежат без
/// оператора, и страница честно показывает «оператор не сохранён».
/// Заставлять ради этого играть ещё один матч незачем — хватает двух
/// запросов: номер оператора по одной сети, затем его объявленные сети.
///
/// Сам список при этом не меняется: добавляются только подписи, поэтому
/// перезапускать обход не надо.
#[tauri::command(async)]
pub fn identify_game_group(state: State<AppState>, nets: Vec<String>) -> SimpleResult {
    let Some(root) = root_of(&state) else {
        return err("Сначала загрузи релиз zapret.");
    };
    let Some(первая) = nets.first() else {
        return err("Нечего определять.");
    };
    let (asn, объявлено) = match crate::gamescan::operator_of(первая) {
        crate::gamescan::Operator::Nets { asn, nets } => (asn, nets),
        crate::gamescan::Operator::Cloud { asn, prefixes } => {
            return err(format!(
                "Это облако AS{asn}, у него {prefixes} сетей. Игровым оператором оно не бывает."
            ))
        }
        crate::gamescan::Operator::Unknown => {
            return err("Справочник не ответил. Проверь связь и попробуй ещё раз.")
        }
    };
    let (его, _чужие) = crate::gamescan::partition_by(&nets, &объявлено);
    if его.is_empty() {
        return err("Ни одна сеть из этих оператору не принадлежит.");
    }
    let name = crate::gamescan::holder_of(&asn);
    match crate::gamescan::attribute(&root, &его, &asn, &name) {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

/// Убирает из списка перечисленные сети.
///
/// Сбор берёт адреса пачкой и иногда прихватывает чужое — облачный адрес,
/// попутную службу. Чтобы вычистить одну строку, не должно требоваться
/// сбрасывать весь список и играть ещё один матч.
///
/// Принимает именно пачку, а не один адрес: снять группу оператора — это
/// три десятка сетей разом, и по одному вызову на каждую значило бы три
/// десятка перезапусков обхода подряд.
#[tauri::command(async)]
pub fn remove_game_ips(app: AppHandle, state: State<AppState>, addrs: Vec<String>) -> SimpleResult {
    let Some(root) = root_of(&state) else {
        return err("Сначала загрузи релиз zapret.");
    };
    match crate::gamescan::remove_from(&root, crate::gamescan::Target::Bypass, &addrs) {
        Ok(0) => return err("Этих адресов в списке нет."),
        Err(e) => return err(e),
        Ok(_) => {}
    }
    // Список читается при запуске winws, иначе удаление ничего не изменит.
    let active = state.persisted.lock().unwrap().active_config.clone();
    if let Some(name) = active {
        if crate::winws::is_winws_running() {
            let _ = crate::monitor::apply_config(&app, &name);
        }
    }
    ok()
}

/// Кто сейчас похож на игру. Пусто — значит ничего не нашли, и это честный
/// ответ: собрать адреса постороннего процесса хуже, чем не собрать ничего.
#[tauri::command(async)]
pub fn game_candidates() -> Vec<crate::gamescan::Candidate> {
    let names = crate::gamescan::process_names();
    crate::gamescan::candidates(&crate::gamescan::connections(), &names)
}

/// Сканирует, пока идёт указанное время, и сразу кладёт найденное в список.
///
/// Имена процессов приходят из интерфейса и в командную строку НЕ уезжают:
/// они попадают в фильтр tasklist как один аргумент, который система
/// передаёт процессу целиком. Но длину ограничиваем — иначе чужая строка на
/// мегабайт просто съест память.
#[tauri::command(async)]
pub fn scan_game_traffic(
    app: AppHandle,
    state: State<AppState>,
    images: Vec<String>,
    seconds: Option<u64>,
) -> Result<crate::gamescan::ScanResult, String> {
    let root = root_of(&state).ok_or("Сначала загрузи релиз zapret.")?;
    let _scan = crate::gamescan::ScanGuard::acquire(&state)?;
    let images: Vec<String> = images
        .into_iter()
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty() && i.len() <= 120)
        .take(8)
        .collect();
    // Пусто — значит «найди сам»: спрашивать имя процесса у человека,
    // который просто хочет, чтобы игра работала, — плохая мысль.
    let secs = seconds.unwrap_or(30).clamp(5, 300);
    // Полминуты без единого признака жизни — плохой опыт. Шлём, что нашли
    // и у какого процесса, прямо по ходу.
    let app2 = app.clone();
    let mut r = crate::gamescan::scan(
        &images,
        std::time::Duration::from_secs(secs),
        std::time::Duration::from_secs(2),
        |proc, found| {
            let _ = app2.emit("game-scan", serde_json::json!({ "proc": proc, "found": found }));
        },
    );
    if r.addrs.is_empty() {
        return Ok(r);
    }
    let пропущено = crate::gamescan::save_ips(&root, &r.addrs)?;
    if !пропущено.is_empty() {
        r.note.push_str(&облачные(&пропущено));
    }

    // Списки winws читает при запуске. Без перезапуска собранные адреса
    // лежали бы в файле, ничего не меняя, — человек решил бы, что сбор не
    // работает. Перезапускаем только то, что уже работало.
    let active = state.persisted.lock().unwrap().active_config.clone();
    if let Some(name) = active {
        if crate::winws::is_winws_running() {
            let _ = crate::monitor::apply_config(&app, &name);
        }
    }
    Ok(r)
}

/// Сбор адресов игры: адреса берутся из пакетов, которые видит сам обход.
///
/// Таблица сокетов для этого не годится: удалённый адрес в ней есть только у
/// соединённых, а матч ходит через sendto, и напротив такого сокета стоит
/// «*:*» — замерено: из 77 строк UDP адрес был у двух. winws же сидит на
/// WinDivert и с `--debug` печатает каждый пакет, попавший под `--wf-*`.
///
/// Что раньше должен был сделать человек, теперь делается здесь.
/// - Game Filter. Без него игровые порты в `--wf-*` не попадают, и winws их
///   просто не видит: на свежем релизе сбор не находил ничего. На время сбора
///   включаем фильтр на всё, а после удачного сбора оставляем на тех
///   протоколах, по которым ходила игра, — иначе собранное лежало бы без дела.
/// - Процесс игры. Раньше в список шло всё, что увидел обход. Теперь раз в две
///   секунды снимается, чей какой локальный порт, и адрес берётся, только
///   если пакет к нему отправила игра, а не голос Discord или торрент.
///
/// Обход перезапускается дважды: с `--debug` и обратно. Второй раз — ПОСЛЕ
/// записи адресов и фильтра. Раньше порядок был обратный: обход поднимался со
/// старым списком, и собранное начинало работать лишь со следующим запуском.
#[tauri::command(async)]
pub fn scan_game_from_log(
    app: AppHandle,
    state: State<AppState>,
    seconds: Option<u64>,
) -> Result<crate::gamescan::ScanResult, String> {
    use crate::gamescan as gs;
    let root = root_of(&state).ok_or("Сначала загрузи релиз zapret.")?;
    let _scan = gs::ScanGuard::acquire(&state)?;
    let (active, as_service) = {
        let p = state.persisted.lock().unwrap();
        (p.active_config.clone(), p.installed_as_service)
    };
    if as_service {
        return Err("Обход сейчас держится службой Windows, а её пакетов Klutz не видит. \
                    Выключи «Держать обход включённым», включи обход из Klutz и повтори."
            .into());
    }
    let active = active.ok_or("Сначала включи обход: адреса Klutz берёт из пакетов, которые через него идут.")?;
    if !crate::winws::is_winws_running() {
        return Err("Обход выключен. Включи его, запусти игру и повтори.".into());
    }

    let secs = seconds.unwrap_or(60).clamp(10, 300);
    let было = crate::toggles::current_game_filter(&root);
    if было != "all" {
        crate::toggles::set_game_filter(&root, "all")?;
    }
    // Ранний выход обязан вернуть всё как было: копилку снять (иначе и
    // следующий штатный запуск уехал бы в подробный режим), фильтр вернуть,
    // обход поднять обычным.
    let откат = || {
        let _ = gs::harvest_stop();
        let _ = crate::toggles::set_game_filter(&root, &было);
        let _ = crate::monitor::apply_config(&app, &active);
    };

    gs::harvest_start();
    if let Err(e) = crate::monitor::apply_config(&app, &active) {
        откат();
        return Err(format!("Не удалось перезапустить обход: {e}"));
    }
    // Живой лог есть не всегда: когда аргументы из .bat разобрать не вышло,
    // обход поднимается через cmd, и вывод уходит в никуда.
    if !crate::winws::last_run_had_logs() {
        откат();
        return Err("Этот конфиг запускается через .bat, и его вывод Klutz не видит — \
                    собрать адреса на нём не выйдет. Включи другую стратегию и повтори."
            .into());
    }

    // Чей какой локальный порт — копим за весь сбор. Первый владелец порта
    // и есть тот, кто слал с него пакеты: сокет игры живёт весь матч.
    let mut owners: std::collections::HashMap<(gs::Proto, u16), u32> = Default::default();
    let mut names = gs::process_names();
    let started = std::time::Instant::now();
    let total = std::time::Duration::from_secs(secs);
    let mut ticks = 0u32;
    while started.elapsed() < total {
        for (proto, port, pid) in gs::local_sockets() {
            owners.entry((proto, port)).or_insert(pid);
        }
        // Список процессов дороже таблицы и меняется редко.
        if ticks % 5 == 4 {
            names.extend(gs::process_names());
        }
        let сейчас = gs::pick_game(&gs::harvest_snapshot(), &owners, &names);
        let _ = app.emit(
            "game-scan",
            serde_json::json!({
                "proc": сейчас.process.as_deref().unwrap_or("ищу игру"),
                "found": сейчас.addrs.len(),
            }),
        );
        ticks += 1;
        let left = total.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(std::time::Duration::from_secs(2)));
    }

    let hits = gs::harvest_take();
    for (proto, port, pid) in gs::local_sockets() {
        owners.entry((proto, port)).or_insert(pid);
    }
    names.extend(gs::process_names());
    let pick = gs::pick_game(&hits, &owners, &names);

    let note = match &pick.process {
        Some(game) => {
            let пропущено = match gs::save_ips(&root, &pick.addrs) {
                Ok(p) => p,
                Err(e) => {
                    откат();
                    return Err(e);
                }
            };
            // Фильтр трогаем, только если в списке есть сети: иначе обходу на
            // игровых портах не с чем работать, а режим, который человек
            // выбрал сам, мы бы молча поменяли.
            let есть_сети = !gs::saved_ips(&root).is_empty();
            let режим: &str = if есть_сети {
                gs::режим_фильтра(&было, pick.udp, pick.tcp)
            } else {
                &было
            };
            let _ = crate::toggles::set_game_filter(&root, режим);
            let mut n = if есть_сети {
                format!(
                    "Игра: {game}, поймано адресов: {}. В список идёт не сам адрес: Klutz узнаёт \
                     оператора и берёт все его сети — одного пойманного сервера хватает, чтобы \
                     накрыть пул.",
                    pick.addrs.len()
                )
            } else {
                format!(
                    "Игра: {game}, но ни одного её адреса положить в список не вышло, и Game \
                     Filter не тронут."
                )
            };
            if режим != было {
                n.push_str(&format!(
                    " Game Filter включён на {} — без него игровые порты обход не видит, и \
                     список лежал бы без дела.",
                    описание_фильтра(режим)
                ));
            }
            if !пропущено.is_empty() {
                n.push_str(&облачные(&пропущено));
            }
            n
        }
        None => {
            let _ = crate::toggles::set_game_filter(&root, &было);
            if pick.unattributed {
                "Пакеты через обход шли, но ни один не удалось привязать к процессу, и в \
                 список ничего не положено. Повтори сбор прямо в матче."
                    .to_string()
            } else if !pick.others.is_empty() {
                format!(
                    "Игрового трафика не нашлось: пакеты отправляли только {}. Зайди в матч и \
                     повтори.",
                    pick.others.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
                )
            } else {
                "За это время через обход не прошло ни одного игрового пакета. Игра была в \
                 матче? В меню она почти ничего не шлёт."
                    .to_string()
            }
        }
    };
    // Копилка снята, значит запуск без --debug, а список и фильтр уже на месте.
    let _ = crate::monitor::apply_config(&app, &active);

    Ok(gs::ScanResult {
        // Именно проверка: winws мог упасть за время сбора.
        running: crate::winws::is_winws_running(),
        process: pick.process.clone(),
        addrs: pick.addrs,
        tcp_ports: Vec::new(),
        udp_ports: Vec::new(),
        ticks,
        note,
    })
}

pub(crate) fn описание_фильтра(mode: &str) -> &'static str {
    match mode {
        "all" => "TCP и UDP",
        "udp" => "UDP",
        "tcp" => "TCP",
        _ => "выключен",
    }
}

/// Переносит собранные адреса из «обходить» в «не трогать».
///
/// Нужно, когда игра работает, а обход ей мешает. На живом Valorant так и
/// вышло: серверы Riot попали в ipset-all, игровой профиль применил к ним
/// `fake` с двенадцатью повторами, и игра показала высокий пинг с ошибкой
/// сети. Адреса при этом собраны правильно — просто применять их надо в
/// другую сторону.
/// Приписка про облачные серверы, взятые поштучно.
///
/// Об этом надо сказать: у облака сеть оператора не берётся, а значит,
/// следующий матч на соседнем сервере в список не попадёт, и его придётся
/// поймать отдельно.
fn облачные(servers: &[String]) -> String {
    format!(
        " Облачные серверы взяты поштучно, без соседних сетей: {}. Сети облака — это пол-интернета, \
         поэтому матч на другом сервере придётся поймать заново.",
        servers.join("; ")
    )
}

#[tauri::command(async)]
pub fn exclude_game_ips(app: AppHandle, state: State<AppState>) -> SimpleResult {
    let Some(root) = root_of(&state) else {
        return err("Сначала загрузи релиз zapret.");
    };
    let addrs = crate::gamescan::saved_ips_in(&root, crate::gamescan::Target::Bypass);
    if addrs.is_empty() {
        return err("Собранных адресов нет — переносить нечего.");
    }
    // Отдельная очистка списка обхода здесь больше не нужна: запись в один
    // список сама убирает эти адреса из другого. Раньше очисток было две —
    // и ровно то, что они делали порознь, разъезжалось при следующем сборе.
    if let Err(e) = crate::gamescan::save_ips_to(&root, crate::gamescan::Target::Skip, &addrs) {
        return err(e);
    }
    // Списки читаются при запуске, иначе перенос ничего не изменит.
    let active = state.persisted.lock().unwrap().active_config.clone();
    if let Some(name) = active {
        if crate::winws::is_winws_running() {
            let _ = crate::monitor::apply_config(&app, &name);
        }
    }
    ok()
}

#[tauri::command(async)]
pub fn clear_game_ips(app: AppHandle, state: State<AppState>) -> SimpleResult {
    let Some(root) = root_of(&state) else {
        return err("Сначала загрузи релиз zapret.");
    };
    if let Err(e) = crate::gamescan::clear_ips(&root) {
        return err(e);
    }
    // Без перезапуска winws продолжает работать со СТАРЫМ списком: файл
    // очищен, а в памяти адреса остались. Человек жмёт «Убрать», видит
    // пустой список и не понимает, почему ничего не изменилось.
    let active = state.persisted.lock().unwrap().active_config.clone();
    if let Some(name) = active {
        if crate::winws::is_winws_running() {
            let _ = crate::monitor::apply_config(&app, &name);
        }
    }
    ok()
}

// ─────────── Дополнительные стратегии ───────────

#[derive(Debug, Serialize)]
pub struct ExtraStrategies {
    /// Сколько вариантов уже лежит в папке релиза.
    count: usize,
    /// Какой конфиг возьмём образцом, если пользователь не выберет сам.
    template: Option<String>,
}

#[tauri::command(async)]
pub fn get_extra_strategies(state: State<AppState>) -> ExtraStrategies {
    let Some(root) = root_of(&state) else {
        return ExtraStrategies { count: 0, template: None };
    };
    let active = state.persisted.lock().unwrap().active_config.clone();
    ExtraStrategies {
        count: crate::strategies::count(&root),
        template: crate::strategies::default_template(&root, active.as_deref()),
    }
}

/// Создаёт варианты выбранного конфига с позициями разреза, которых в
/// релизе Flowseal нет. Файлы появляются в папке релиза и дальше живут как
/// обычные конфиги: попадают в список, в прогон тестов и в рейтинг.
#[tauri::command(async)]
pub fn generate_extra_strategies(state: State<AppState>, template: Option<String>) -> SimpleResult {
    let Some(root) = root_of(&state) else {
        return err("Сначала загрузи релиз zapret.");
    };
    let active = state.persisted.lock().unwrap().active_config.clone();
    let template = match template.filter(|t| !t.trim().is_empty()) {
        Some(t) => t,
        None => match crate::strategies::default_template(&root, active.as_deref()) {
            Some(t) => t,
            None => return err("В релизе нет конфигов, которые можно взять за образец."),
        },
    };
    match crate::strategies::generate(&root, &template) {
        Ok(made) => {
            let _ = made;
            ok()
        }
        Err(e) => err(e),
    }
}

#[tauri::command(async)]
pub fn remove_extra_strategies(state: State<AppState>) -> SimpleResult {
    let Some(root) = root_of(&state) else {
        return err("Сначала загрузи релиз zapret.");
    };
    match crate::strategies::remove_all(&root) {
        Ok(_) => ok(),
        Err(e) => err(e),
    }
}

// ─────────── Discord без QUIC ───────────

#[tauri::command(async)]
pub fn get_discord_quic() -> crate::quic::QuicStatus {
    crate::quic::status()
}

#[tauri::command(async)]
pub fn set_discord_quic(enabled: bool) -> SimpleResult {
    match crate::quic::set(enabled) {
        Ok(_) => ok(),
        Err(e) => err(e),
    }
}

/// Системный прокси Windows. `null` — выключен.
///
/// Тесты идут напрямую, а приложения вроде Discord — через системный прокси.
/// Если он включён, зелёный тест ничего не говорит о Discord, и об этом надо
/// сказать до того, как человек поверит результатам.
#[tauri::command(async)]
pub fn get_system_proxy() -> Option<crate::discorddiag::Proxy> {
    crate::discorddiag::system_proxy()
}

/// «Почему Discord не запускается». До полуминуты: пробы сети идут по очереди.
#[tauri::command(async)]
pub fn diagnose_discord(state: State<AppState>) -> crate::discorddiag::Report {
    let (active, root) = {
        let p = state.persisted.lock().unwrap();
        (p.active_config.clone(), p.root_path.clone())
    };
    let bypass = crate::winws::is_winws_running().then(|| {
        let name = active
            .as_deref()
            .map(|a| a.trim_end_matches(".bat").to_string())
            .unwrap_or_else(|| "стратегия неизвестна".into());
        match root {
            Some(r) => format!("{name} на {}", crate::history::release_label(Path::new(&r))),
            None => name,
        }
    });
    crate::discorddiag::run(bypass)
}

// ─────────── Экспорт / импорт настроек ───────────
//
// Намеренно узко: только то, что переносимо между машинами и релизами.
// gameFilter, ipsetMode и свои списки доменов живут файлами внутри папки
// релиза, а не здесь, — они вне области.

#[tauri::command(async)]
pub fn export_settings(app: AppHandle, state: State<AppState>, path: String) -> SimpleResult {
    let p = state.persisted.lock().unwrap();
    let payload = serde_json::json!({
        "gameTargets": p.game_targets,
        "autoSwitch": p.auto_switch,
        "notifications": p.notifications,
        "tgws": p.tgws,
    });
    drop(p);
    let _ = &app;
    match std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap_or_default()) {
        Ok(()) => ok(),
        Err(e) => err(e.to_string()),
    }
}

#[tauri::command(async)]
pub fn import_settings(app: AppHandle, state: State<AppState>, path: String) -> SimpleResult {
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return err("Файл настроек повреждён.");
    };
    let mut imported_tgws_bad = false;
    {
        let mut p = state.persisted.lock().unwrap();
        // Только если разобралось: раньше повреждённый список молча
        // превращался в None, то есть сбрасывал цели на стандартные, а
        // импорт при этом считался успешным.
        if let Some(t) = v.get("gameTargets") {
            if let Ok(g) = serde_json::from_value(t.clone()) {
                p.game_targets = Some(g);
            }
        }
        if let Some(a) = v.get("autoSwitch") {
            if let Ok(a) = serde_json::from_value(a.clone()) {
                p.auto_switch = Some(a);
            }
        }
        if let Some(n) = v.get("notifications").and_then(|n| n.as_bool()) {
            p.notifications = Some(n);
        }
        // Настройки прокси из файла проходят те же проверки, что и ручной
        // ввод: иначе импорт возвращал ровно ту поломанную ссылку
        // tg://proxy, ради которой валидацию и добавляли.
        if let Some(t) = v.get("tgws") {
            if let Ok(t) = serde_json::from_value::<crate::tgws::TgSettings>(t.clone()) {
                if t.port > 0 && !t.host.trim().is_empty() && crate::tgws::is_valid_secret(&t.secret) {
                    p.tgws = Some(t);
                } else {
                    imported_tgws_bad = true;
                }
            }
        }
    }
    save_state(&app, &state);
    if imported_tgws_bad {
        return err("Настройки применены, кроме Telegram-прокси: в файле неверный хост, порт или секрет.");
    }
    ok()
}

// ─────────── Релизы zapret ───────────

#[tauri::command(async)]
pub fn get_latest_release_info() -> crate::releases::LatestRelease {
    crate::releases::latest_release()
}

#[derive(Debug, Serialize)]
pub struct DownloadResult {
    ok: bool,
    error: Option<String>,
    root: Option<String>,
    /// Что перенесено из прежнего релиза.
    carried: Vec<String>,
}

/// Скачивает свежий релиз и сразу распаковывает — интерфейсу нужен готовый
/// корень, а не путь к архиву.
#[tauri::command(async)]
pub fn download_latest_release(app: AppHandle, state: State<AppState>) -> DownloadResult {
    let fail = |e: String| DownloadResult { ok: false, error: Some(e), root: None, carried: Vec::new() };
    let zip = match crate::releases::download_latest(&app) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let name = zip.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "release".into());
    let target = crate::releases::releases_dir(&app).join(&name);
    let root = match crate::releases::extract_zip(&zip, &target) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let _ = std::fs::remove_file(&zip);

    let check = validate_release(&root);
    if !check.ok {
        return fail(check.error.unwrap_or_else(|| "Скачанный архив не похож на релиз zapret.".into()));
    }
    let carried = set_root(&app, &state, &root, check.can_install_service);
    DownloadResult { ok: true, error: None, root: Some(root.to_string_lossy().to_string()), carried }
}

#[derive(Debug, Serialize)]
pub struct ReleasesList {
    ok: bool,
    releases: Vec<crate::releases::ReleaseEntry>,
}

#[tauri::command(async)]
pub fn list_releases(app: AppHandle, state: State<AppState>) -> ReleasesList {
    let current = state.persisted.lock().unwrap().root_path.clone();
    ReleasesList { ok: true, releases: crate::releases::list_releases(&app, current.as_deref()) }
}

#[tauri::command(async)]
pub fn delete_release(app: AppHandle, state: State<AppState>, folder_name: String) -> SimpleResult {
    // Интерфейс кнопку у активного релиза прячет, но проверка нужна и здесь:
    // из-под работающего winws папка удалилась бы наполовину — занятые
    // winws.exe и драйвер остались бы, а конфиги и списки пропали.
    if let Some(root) = root_of(&state) {
        let dir = crate::releases::releases_dir(&app).join(&folder_name);
        if safe_name(&folder_name) && root.starts_with(&dir) {
            return err("Это текущий релиз — сначала переключись на другой.");
        }
    }
    match crate::releases::delete_release(&app, &folder_name) {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

/// Распаковка вручную выбранного .zip — второй путь загрузки релиза,
/// помимо скачивания с GitHub.
#[tauri::command(async)]
pub fn load_archive(app: AppHandle, state: State<AppState>, zip_path: String) -> LoadPathResult {
    let zip = PathBuf::from(&zip_path);
    // Архив с именем вида «...zip» даёт file_stem() == "..", и цель
    // распаковки уезжала бы из каталога релизов.
    let name = zip
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|n| safe_name(n))
        .unwrap_or_else(|| "release".into());
    let target = crate::releases::releases_dir(&app).join(&name);
    let root = match crate::releases::extract_zip(&zip, &target) {
        Ok(r) => r,
        Err(e) => return LoadPathResult::Err { ok: false, error: e },
    };
    load_path(app, state, root.to_string_lossy().to_string())
}

// ─────────── Уведомления ───────────

#[derive(Debug, Serialize)]
pub struct NotifySound {
    volume: u8,
    duration: String,
}

#[tauri::command(async)]
pub fn get_notify_sound(state: State<AppState>) -> NotifySound {
    let p = state.persisted.lock().unwrap();
    NotifySound {
        volume: p.notify_volume.unwrap_or(70),
        duration: p.notify_duration.clone().unwrap_or_else(|| "short".into()),
    }
}

#[tauri::command(async)]
pub fn set_notify_sound(
    app: AppHandle,
    state: State<AppState>,
    volume: Option<u8>,
    duration: Option<String>,
) -> SimpleResult {
    {
        let mut p = state.persisted.lock().unwrap();
        if let Some(v) = volume {
            p.notify_volume = Some(v.min(100));
        }
        if let Some(d) = duration {
            p.notify_duration = Some(if d == "long" { d } else { "short".into() });
        }
    }
    save_state(&app, &state);
    ok()
}

#[tauri::command(async)]
pub fn test_notification(app: AppHandle, state: State<AppState>) -> SimpleResult {
    // send() молча ничего не делает при выключенных уведомлениях — без этой
    // проверки кнопка «Проверить» обещала бы тост, которого не будет.
    if !state.persisted.lock().unwrap().notifications.unwrap_or(true) {
        return err("Уведомления выключены — включи переключатель выше.");
    }
    crate::notify::send(&app, &state, "Klutz: тестовое уведомление", "Так будут выглядеть сообщения о сбоях и итогах тестов.");
    ok()
}

// ─────────── Расписание автопрогона тестов ───────────

#[derive(Debug, Serialize)]
pub struct AutoTestSchedule {
    enabled: bool,
    days: u32,
    mode: String,
    #[serde(rename = "lastRunAt")]
    last_run_at: Option<u64>,
}

#[tauri::command(async)]
pub fn get_auto_test_schedule(state: State<AppState>) -> AutoTestSchedule {
    let p = state.persisted.lock().unwrap();
    AutoTestSchedule {
        enabled: p.autotest_enabled.unwrap_or(false),
        days: p.autotest_days.unwrap_or(7),
        mode: p.autotest_mode.clone().unwrap_or_else(|| "standard".into()),
        last_run_at: p.autotest_last_run,
    }
}

#[tauri::command(async)]
pub fn set_auto_test_schedule(
    app: AppHandle,
    state: State<AppState>,
    enabled: Option<bool>,
    days: Option<u32>,
    mode: Option<String>,
) -> SimpleResult {
    {
        let mut p = state.persisted.lock().unwrap();
        if let Some(e) = enabled {
            p.autotest_enabled = Some(e);
        }
        if let Some(d) = days {
            p.autotest_days = Some(d.clamp(1, 30));
        }
        if let Some(m) = mode {
            p.autotest_mode = Some(if m == "dpi" { m } else { "standard".into() });
        }
    }
    save_state(&app, &state);
    ok()
}

#[tauri::command]
pub fn window_minimize(window: tauri::Window) {
    let _ = window.minimize();
}

#[tauri::command]
pub fn window_toggle_maximize(window: tauri::Window) {
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
    } else {
        let _ = window.maximize();
    }
}

/// Не завершает приложение: обработчик CloseRequested в main.rs перехватывает
/// закрытие и прячет окно в трей, иначе обход умер бы вместе с окном.
#[tauri::command]
pub fn window_close(window: tauri::Window) {
    let _ = window.close();
}

#[tauri::command]
pub fn window_is_maximized(window: tauri::Window) -> bool {
    window.is_maximized().unwrap_or(false)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn настоящие_имена_конфигов_проходят_проверку() {
        // Так называются конфиги в релизе Flowseal. Когда скобки попали в
        // список запрещённых, запуск отваливался на каждом из них — то есть
        // приложение не работало вовсе.
        for n in [
            "general.bat",
            "general (ALT).bat",
            "general (ALT11).bat",
            "general (FAKE TLS AUTO ALT2).bat",
            "general (Z2K general midsld10).bat",
        ] {
            assert!(!опасное_имя(n), "должно проходить: {n}");
        }
    }

    #[test]
    fn имена_с_подвохом_не_проходят() {
        for n in [
            "x&calc.bat",
            "a|b.bat",
            "a>b.bat",
            "a<b.bat",
            "a^b.bat",
            "a%PATH%.bat",
            "a!x!.bat",
            "a\"b.bat",
            "a`b.bat",
            "a\nb.bat",
        ] {
            assert!(опасное_имя(n), "должно отвергаться: {n:?}");
        }
    }

    /// Тот же предикат, что и в checked_config: там он внутри проверки
    /// членства в списке, а здесь нужен отдельно.
    fn опасное_имя(n: &str) -> bool {
        n.contains(|c| "&|<>^\"'`%!\r\n".contains(c))
    }

    #[test]
    fn safe_name_отсекает_всё_что_уводит_из_каталога() {
        for bad in ["", ".", "..", "..\\x", "a/b", "a\\b", "C:x", "C:", "release/../../x"] {
            assert!(!safe_name(bad), "должно быть отвергнуто: {bad:?}");
        }
        for good in ["release", "zapret-discord-youtube-1.9.9c", "general (ALT).bat", "имя с пробелом"] {
            assert!(safe_name(good), "должно быть принято: {good:?}");
        }
    }

    #[test]
    fn точка_проходила_старую_проверку_и_сносила_весь_каталог() {
        // Старое условие: только "..", "/" и "\\".
        let старое = |n: &str| !n.contains("..") && !n.contains('/') && !n.contains('\\');
        assert!(старое("."), "старая проверка «.» пропускала");
        assert!(!safe_name("."), "новая обязана отвергнуть");
    }
}
