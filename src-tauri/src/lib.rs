//! Вся логика приложения живёт в библиотеке, а main.rs — тонкая обёртка.
//!
//! Так делает и шаблон Tauri v2, но здесь у этого есть вторая причина:
//! build.rs вшивает в исполняемый файл манифест requireAdministrator, и
//! тестовый бинарник, собранный из bin-таргета, Windows отказывалась
//! запускать без повышения прав (ошибка 740) — то есть `cargo test` в
//! проекте не работал вообще. Тесты библиотеки манифеста не получают.

mod autostart;
mod autotest;
mod carry;
mod commands;
mod configdiff;
mod diag;
mod discorddiag;
mod favicon;
mod gamescan;
mod history;
mod klutzupdate;
mod maintenance;
mod monitor;
mod notify;
mod probe;
mod quic;
mod release;
mod releases;
mod report;
mod service;
mod state;
mod strategies;
mod sys;
mod targets;
mod tests;
mod tgws;
mod tlsprobe;
mod toggles;
mod tray;
mod udpprobe;
mod whatsnew;
mod winws;

use state::AppState;
use tauri::{Manager, WindowEvent};

pub fn run() {
    tauri::Builder::default()
        // Два процесса Klutz держали бы каждый свою копию «работает ли
        // winws.exe» и расходились бы и друг с другом, и с реальностью.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState::new())
        .setup(|app| {
            let handle = app.handle().clone();
            {
                let state = app.state::<AppState>();
                state::load_state(&handle, &state);
            }
            tgws::ensure_settings(&handle);

            tray::build(&handle)?;
            tgws::adopt_existing(&handle);
            monitor::start(handle.clone());
            autotest::start(handle.clone());
            // Discord после обновления переезжает в новую папку, и правило
            // «без QUIC» к ней уже не относится. netsh небыстрый — в фоне.
            std::thread::spawn(quic::refresh);

            // Автозапуск поднимает приложение свёрнутым в трей: показывать
            // окно при входе в систему никто не просил.
            let autostarted = std::env::args().any(|a| a == "--autostart");
            if let Some(w) = app.get_webview_window("main") {
                if autostarted {
                    let _ = w.hide();
                }
            }

            // Прокси Telegram сам поднимается вместе с приложением, если
            // пользователь это включил.
            let should_start_tg = {
                let state = app.state::<AppState>();
                let p = state.persisted.lock().unwrap();
                p.tgws.as_ref().map(|t| t.auto_start).unwrap_or(false)
            };
            if should_start_tg {
                let h = handle.clone();
                std::thread::spawn(move || {
                    let _ = tgws::start(&h);
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Меню трея ведёт себя как меню: ушёл фокус — спряталось.
            if window.label() == tray::POPUP {
                match event {
                    WindowEvent::Focused(false) => tray::hide_popup(window.app_handle()),
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        tray::hide_popup(window.app_handle());
                    }
                    _ => {}
                }
                return;
            }
            // Закрытие окна прячет в трей, а не завершает работу — иначе
            // обход умрёт вместе с окном. Настоящий выход — через меню трея.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::load_path,
            commands::run_config,
            commands::stop_config,
            commands::get_winws_log,
            commands::check_games,
            commands::get_favicon,
            commands::get_game_targets,
            commands::get_default_game_targets,
            commands::save_game_targets,
            commands::reset_game_targets,
            commands::run_tests,
            commands::get_last_test_results,
            commands::stop_tests,
            commands::install_service,
            commands::remove_service,
            commands::get_service_status,
            commands::get_toggles,
            commands::set_game_filter,
            commands::cycle_ipset_mode,
            commands::set_auto_update,
            commands::get_autostart,
            commands::set_autostart,
            commands::run_diagnostics,
            commands::fix_diagnostic,
            commands::get_auto_switch,
            commands::set_auto_switch,
            commands::get_heal_log,
            commands::get_onboarding_done,
            commands::set_onboarding_done,
            commands::get_notifications,
            commands::set_notifications,
            commands::start_tgwsproxy,
            commands::stop_tgwsproxy,
            commands::restart_tgwsproxy,
            commands::get_tgwsproxy_status,
            commands::get_tgwsproxy_log,
            commands::get_tgwsproxy_settings,
            commands::set_tgwsproxy_settings,
            commands::set_tgwsproxy_autostart,
            commands::regenerate_tgwsproxy_secret,
            commands::open_tg_proxy_link,
            commands::open_external_url,
            commands::open_release_folder,
            commands::get_test_history,
            commands::get_release_regression,
            commands::import_old_config,
            commands::open_result_file,
            commands::update_ipset_list,
            commands::update_hosts_file,
            commands::check_updates,
            commands::clear_discord_cache,
            commands::get_discord_quic,
            commands::set_discord_quic,
            commands::diagnose_discord,
            commands::get_system_proxy,
            commands::get_whats_new,
            commands::trial_latest_release,
            commands::developer_report,
            commands::get_klutz_release,
            commands::install_klutz_update,
            commands::save_report,
            commands::get_custom_lists,
            commands::save_custom_lists,
            commands::export_settings,
            commands::import_settings,
            commands::get_latest_release_info,
            commands::download_latest_release,
            commands::list_releases,
            commands::delete_release,
            commands::load_archive,
            commands::get_notify_sound,
            commands::set_notify_sound,
            commands::test_notification,
            commands::get_auto_test_schedule,
            commands::set_auto_test_schedule,
            commands::copy_text,
            commands::get_versions,
            commands::check_component_updates,
            commands::check_klutz_update,
            commands::check_bypass_chance,
            commands::get_game_scan,
            commands::game_candidates,
            commands::scan_game_traffic,
            commands::scan_game_from_log,
            commands::clear_game_ips,
            commands::exclude_game_ips,
            commands::remove_game_ips,
            commands::identify_game_group,
            commands::get_extra_strategies,
            commands::generate_extra_strategies,
            commands::remove_extra_strategies,
            tray::tray_menu_state,
            tray::tray_menu_ready,
            tray::tray_menu_hide,
            tray::tray_menu_action,
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
            commands::window_is_maximized,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
