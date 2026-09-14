use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

/// Всё, что переживает перезапуск — аналог state.json из Electron-версии.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(rename = "rootPath")]
    pub root_path: Option<String>,
    #[serde(rename = "activeConfig")]
    pub active_config: Option<String>,
    #[serde(rename = "installedAsService")]
    pub installed_as_service: bool,
    #[serde(rename = "canInstallService")]
    pub can_install_service: bool,
    #[serde(rename = "startedAt")]
    pub started_at: Option<u64>,
    /// None — пользователь список не трогал, берём стандартный.
    #[serde(rename = "gameTargets")]
    pub game_targets: Option<Vec<crate::targets::Target>>,
    pub tgws: Option<crate::tgws::TgSettings>,
    #[serde(rename = "autoSwitch")]
    pub auto_switch: Option<AutoSwitch>,
    pub notifications: Option<bool>,
    #[serde(rename = "onboardingDone")]
    pub onboarding_done: Option<bool>,
    #[serde(rename = "healLog")]
    pub heal_log: Option<Vec<HealEntry>>,
    #[serde(rename = "notifyVolume")]
    pub notify_volume: Option<u8>,
    #[serde(rename = "notifyDuration")]
    pub notify_duration: Option<String>,
    #[serde(rename = "autotestEnabled")]
    pub autotest_enabled: Option<bool>,
    #[serde(rename = "autotestDays")]
    pub autotest_days: Option<u32>,
    #[serde(rename = "autotestLastRun")]
    pub autotest_last_run: Option<u64>,
    #[serde(rename = "autotestMode")]
    pub autotest_mode: Option<String>,
    /// Последняя стратегия, на которой проверка связи прошла ЧИСТО.
    ///
    /// `active_config` отвечает на вопрос «что сейчас включено», и это не
    /// одно и то же: включить можно что угодно, в том числе неработающее.
    /// Здесь — то, что подтвердилось замером, и это лучшее знание о сети,
    /// чем рейтинг прогона, снятый когда-то давно.
    #[serde(rename = "workingConfig")]
    pub working_config: Option<String>,
    #[serde(rename = "workingAt")]
    pub working_at: Option<u64>,
    /// Корень релиза, с которого переключились последний раз. Нужен, чтобы
    /// предложить вернуться, если на новом стало хуже.
    #[serde(rename = "previousRoot")]
    pub previous_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoSwitch {
    pub enabled: bool,
    pub threshold: u32,
    #[serde(rename = "intervalSec")]
    pub interval_sec: u64,
}

impl Default for AutoSwitch {
    fn default() -> Self {
        Self { enabled: false, threshold: 3, interval_sec: 30 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealEntry {
    pub at: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub ok: bool,
    /// Сколько стратегий было перепробовано к моменту записи. Осмысленно
    /// только у `gave-up`; у переключения всегда 0.
    #[serde(rename = "triedCount")]
    pub tried_count: u32,
}

pub struct AppState {
    pub persisted: Mutex<PersistedState>,
    pub winws_child: Mutex<Option<std::process::Child>>,
    pub winws_log: Mutex<Vec<String>>,
    pub winws_intentional_stop: Mutex<bool>,
    pub tgws_pid: Mutex<Option<u32>>,
    pub tgws_log: Mutex<Vec<String>>,
    /// (сколько ключевых целей ответило, сколько проверяли) — от этого зависит
    /// цвет иконки в трее и решение автопереключения.
    pub last_check: Mutex<Option<(usize, usize)>>,
    pub degraded_ticks: Mutex<u32>,
    pub healing_attempts: Mutex<Vec<String>>,
    /// Про исчерпанный рейтинг сообщаем один раз за серию, а не каждый тик.
    pub heal_exhausted: Mutex<bool>,
    pub testing: Mutex<bool>,
    /// PID запущенного прогона тестов — чтобы «Остановить» гасило именно его.
    pub test_pid: Mutex<Option<u32>>,
    /// «Остановить» нажали. Второй этап воронки живого процесса ещё не имеет,
    /// поэтому убивать нечего — прогон должен сам увидеть флаг и не начинать.
    pub test_cancel: Mutex<bool>,
    /// Последняя фоновая проверка по целям (имя, ответила, мс) — меню трея
    /// показывает её построчно, как в Electron-версии.
    pub last_targets: Mutex<Vec<(String, bool, u64)>>,
    /// Когда прошла последняя фоновая проверка, мс от эпохи. 0 — ещё ни разу.
    /// Окно по нему решает, относится ли проверка к текущему конфигу.
    pub last_check_at: Mutex<u64>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            persisted: Mutex::new(PersistedState::default()),
            winws_child: Mutex::new(None),
            winws_log: Mutex::new(Vec::new()),
            winws_intentional_stop: Mutex::new(false),
            tgws_pid: Mutex::new(None),
            tgws_log: Mutex::new(Vec::new()),
            last_check: Mutex::new(None),
            degraded_ticks: Mutex::new(0),
            healing_attempts: Mutex::new(Vec::new()),
            heal_exhausted: Mutex::new(false),
            testing: Mutex::new(false),
            test_pid: Mutex::new(None),
            test_cancel: Mutex::new(false),
            last_targets: Mutex::new(Vec::new()),
            last_check_at: Mutex::new(0),
        }
    }
}

/// Право на прогон тестов. Источников два — кнопка в окне и автопрогон, —
/// и раньше каждый вёл учёт сам: `run_tests` брал замок, а `autotest`
/// присваивал `testing = true` в обход него. Два прогона PowerShell шли
/// одновременно и перетирали друг другу и конфиг, и общий `test_pid`.
/// Теперь оба ходят сюда, а снятие флагов делает Drop — в том числе на
/// раннем `return` и по ошибке.
pub struct TestRun<'a> {
    state: &'a AppState,
}

impl<'a> TestRun<'a> {
    /// None — прогон уже идёт, начинать второй нельзя.
    pub fn acquire(state: &'a AppState) -> Option<Self> {
        let mut testing = state.testing.lock().unwrap();
        // Сбор адресов игры перезапускает обход — прогон поверх него
        // перетирал бы конфиг под сбором.
        if *testing || crate::gamescan::scan_busy() {
            return None;
        }
        *testing = true;
        drop(testing);
        *state.test_cancel.lock().unwrap() = false;
        Some(TestRun { state })
    }

    pub fn cancelled(&self) -> bool {
        *self.state.test_cancel.lock().unwrap()
    }
}

impl Drop for TestRun<'_> {
    fn drop(&mut self) {
        *self.state.test_pid.lock().unwrap() = None;
        *self.state.testing.lock().unwrap() = false;
    }
}

/// Каталог данных приложения. Раньше здесь стоял `expect`, и на машине,
/// где путь не определяется, приложение падало прямо на загрузке настроек.
fn state_path(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().expect("no app data dir").join("state.json")
}

pub fn load_state(app: &AppHandle, state: &AppState) {
    if let Ok(text) = fs::read_to_string(state_path(app)) {
        if let Ok(mut parsed) = serde_json::from_str::<PersistedState>(&text) {
            if let Some(targets) = parsed.game_targets.as_mut() {
                crate::targets::migrate_hosts(targets);
            }
            *state.persisted.lock().unwrap() = parsed;
        }
    }
}

/// Пишем через временный файл с переименованием: прямой `fs::write` при
/// аварии посреди записи оставлял обрезанный JSON, и настройки терялись
/// целиком. Ошибку больше не проглатываем молча — она видна в логе.
pub fn save_state(app: &AppHandle, state: &AppState) {
    if let Err(e) = try_save_state(app, state) {
        eprintln!("klutz: не удалось сохранить state.json: {e}");
    }
}

/// Сохранения выстроены в очередь. Временный файл один на всех, а зовут
/// сохранение из двух десятков мест, включая фоновые потоки: два
/// одновременных вызова писали в один и тот же `state.json.tmp`, и второе
/// переименование прилетало в `NotFound` — чьё-то изменение пропадало.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

fn try_save_state(app: &AppHandle, state: &AppState) -> std::io::Result<()> {
    // Замок держим на всю запись, включая переименование. Отравление тут
    // не страшно: под ним нет ничего, что могло бы оставить данные битыми.
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app.path().app_data_dir().map_err(std::io::Error::other)?;
    fs::create_dir_all(&dir)?;
    let snapshot = state.persisted.lock().unwrap().clone();
    let json = serde_json::to_string_pretty(&snapshot).map_err(std::io::Error::other)?;
    let target = state_path(app);
    let tmp = target.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &target)
}

