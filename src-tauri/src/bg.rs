//! Долгие команды — в пуле для блокирующих задач.
//!
//! Команда с `#[tauri::command(async)]`, написанная обычной функцией,
//! выполняется прямо на рабочем потоке асинхронного пула Tauri и держит его
//! до конца. Потоков там столько, сколько ядер, а прогон тестов идёт минуты,
//! разведка и «Почему Discord» — до минуты. На 2–4 ядрах пара таких операций
//! и автопроверка связи занимали весь пул, и остальные кнопки окна «думали»,
//! пока что-нибудь не освободится. Здесь такие команды уходят в
//! `spawn_blocking`: там потоков заводится столько, сколько нужно.
//!
//! Сами тела остались в `commands.rs` обычными функциями — меняется только
//! то, где они выполняются. Имена команд для окна те же.

use tauri::{AppHandle, Manager};

use crate::commands as c;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(v) => v,
        // Паника внутри задачи. В релизе (panic = "abort") сюда не дойти, а в
        // отладке пусть падает так же громко, как упала бы сама команда.
        Err(e) => panic!("фоновая задача упала: {e}"),
    }
}

#[tauri::command]
pub async fn check_games(app: AppHandle) -> c::CheckGamesResult {
    blocking(move || c::check_games(app.state())).await
}

#[tauri::command]
pub async fn run_tests(app: AppHandle, mode: String) -> c::RunTestsResult {
    blocking(move || c::run_tests(app.clone(), app.state(), mode)).await
}

#[tauri::command]
pub async fn trial_latest_release(app: AppHandle) -> c::TrialResult {
    blocking(move || c::trial_latest_release(app.clone(), app.state())).await
}

#[tauri::command]
pub async fn run_diagnostics(app: AppHandle, deep: Option<bool>) -> c::DiagResult {
    blocking(move || c::run_diagnostics(app.state(), deep)).await
}

#[tauri::command]
pub async fn fix_diagnostic(key: String) -> c::SimpleResult {
    blocking(move || c::fix_diagnostic(key)).await
}

#[tauri::command]
pub async fn update_ipset_list(app: AppHandle) -> serde_json::Value {
    blocking(move || c::update_ipset_list(app.state())).await
}

#[tauri::command]
pub async fn check_updates(app: AppHandle) -> serde_json::Value {
    blocking(move || c::check_updates(app.state())).await
}

#[tauri::command]
pub async fn get_klutz_release() -> Result<crate::klutzupdate::KlutzRelease, String> {
    blocking(c::get_klutz_release).await
}

#[tauri::command]
pub async fn install_klutz_update(app: AppHandle) -> c::SimpleResult {
    blocking(move || c::install_klutz_update(app)).await
}

#[tauri::command]
pub async fn check_klutz_update(app: AppHandle) -> c::ComponentUpdate {
    blocking(move || c::check_klutz_update(app)).await
}

#[tauri::command]
pub async fn check_component_updates(app: AppHandle) -> c::ComponentUpdates {
    blocking(move || c::check_component_updates(app.clone(), app.state())).await
}

#[tauri::command]
pub async fn clear_discord_cache() -> serde_json::Value {
    blocking(c::clear_discord_cache).await
}

#[tauri::command]
pub async fn check_bypass_chance(app: AppHandle) -> c::BypassChance {
    blocking(move || c::check_bypass_chance(app.state())).await
}

#[tauri::command]
pub async fn recon_network(app: AppHandle) -> crate::recon::Recon {
    blocking(move || c::recon_network(app.state())).await
}

#[tauri::command]
pub async fn scan_game_traffic(
    app: AppHandle,
    images: Vec<String>,
    seconds: Option<u64>,
) -> Result<crate::gamescan::ScanResult, String> {
    blocking(move || c::scan_game_traffic(app.clone(), app.state(), images, seconds)).await
}

#[tauri::command]
pub async fn scan_game_from_log(app: AppHandle, seconds: Option<u64>) -> Result<crate::gamescan::ScanResult, String> {
    blocking(move || c::scan_game_from_log(app.clone(), app.state(), seconds)).await
}

#[tauri::command]
pub async fn diagnose_discord(app: AppHandle) -> crate::discorddiag::Report {
    blocking(move || c::diagnose_discord(app.state())).await
}

#[tauri::command]
pub async fn get_latest_release_info() -> crate::releases::LatestRelease {
    blocking(c::get_latest_release_info).await
}

#[tauri::command]
pub async fn download_latest_release(app: AppHandle) -> c::DownloadResult {
    blocking(move || c::download_latest_release(app.clone(), app.state())).await
}

#[tauri::command]
pub async fn check_site(app: AppHandle, host: String) -> crate::lists::SiteCheck {
    blocking(move || c::check_site(app.state(), host)).await
}

// ─────────── hosts ───────────

#[tauri::command]
pub async fn hosts_status() -> crate::hosts::HostsStatus {
    blocking(crate::hosts::status).await
}

#[tauri::command]
pub async fn apply_hosts(app: AppHandle) -> crate::hosts::HostsResult {
    blocking(move || {
        let r = crate::hosts::apply();
        if r.ok {
            crate::keep::remember(&app, crate::keep::Wanted::Hosts, true);
        }
        r
    })
    .await
}

#[tauri::command]
pub async fn remove_hosts(app: AppHandle) -> crate::hosts::HostsResult {
    blocking(move || {
        let r = crate::hosts::remove();
        if r.ok {
            crate::keep::remember(&app, crate::keep::Wanted::Hosts, false);
        }
        r
    })
    .await
}
