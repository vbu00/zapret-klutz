//! Что человек включил вне папки Klutz — возвращается, если пропало.
//!
//! Автозапуск, правило «Discord без QUIC» и строки в hosts живут не у нас:
//! в Планировщике, брандмауэре и системном файле. Их снимает деинсталлятор —
//! и при настоящем удалении (так и надо), и при обновлении поверх, если в
//! установщике оставить «удалить перед установкой». После такого обновления
//! Klutz молча работал без них. Теперь намерение хранится в настройках, а при
//! запуске Klutz сверяет его с системой и возвращает пропавшее.
//!
//! И обратное: при удалении Klutz деинсталлятор зовёт `klutz.exe --cleanup`,
//! и то, что убрать больше некому, убирается здесь.

use tauri::{AppHandle, Manager};

use crate::state::{save_state, AppState};

#[derive(Debug, Clone, Copy)]
pub enum Wanted {
    Autostart,
    NoQuic,
    Hosts,
}

/// Человек сам включил или выключил — запоминаем.
pub fn remember(app: &AppHandle, what: Wanted, on: bool) {
    let state = app.state::<AppState>();
    {
        let mut p = state.persisted.lock().unwrap();
        match what {
            Wanted::Autostart => p.autostart_wanted = Some(on),
            Wanted::NoQuic => p.no_quic_wanted = Some(on),
            Wanted::Hosts => p.hosts_wanted = Some(on),
        }
    }
    save_state(app, &state);
}

/// При запуске, в фоне: netsh, schtasks и чтение hosts — не мгновенные.
pub fn reconcile(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (a, q, h) = {
        let p = state.persisted.lock().unwrap();
        (p.autostart_wanted, p.no_quic_wanted, p.hosts_wanted)
    };
    // Первый запуск версии, которая это помнит: намерения ещё не записаны —
    // берём то, что есть в системе сейчас.
    let a = a.unwrap_or_else(crate::autostart::is_enabled);
    let q = q.unwrap_or_else(crate::quic::enabled);
    let h = h.unwrap_or_else(crate::hosts::applied);
    {
        let mut p = state.persisted.lock().unwrap();
        p.autostart_wanted = Some(a);
        p.no_quic_wanted = Some(q);
        p.hosts_wanted = Some(h);
    }
    save_state(app, &state);

    // Задачу пересоздаём и тогда, когда Klutz переехал в другую папку: она
    // запускает exe по полному пути.
    if a && !crate::autostart::is_enabled() {
        let _ = crate::autostart::set_enabled(true);
    }
    if q {
        if crate::quic::enabled() {
            // Discord после обновления переезжает в новую папку, и правило к
            // ней уже не относится — пересобираем.
            crate::quic::refresh();
        } else {
            let _ = crate::quic::set(true);
        }
    }
    if h && !crate::hosts::applied() {
        let _ = crate::hosts::apply();
    }
}

/// `klutz.exe --cleanup` из деинсталлятора (installer-hooks.nsh). Задачу
/// автозапуска и службу снимает сам деинсталлятор; здесь — то, чего он не
/// умеет: правило брандмауэра и наш блок в hosts. Настройки не трогаем:
/// если их не удаляют вместе с Klutz, при следующей установке вернётся всё,
/// что было включено.
pub fn cleanup() {
    let _ = crate::quic::set(false);
    let _ = crate::hosts::remove();
}
