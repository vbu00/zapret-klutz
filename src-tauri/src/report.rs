//! «Отчёт для разработчика»: всё, от чего зависит обход, одним текстом.
//!
//! Раньше друг присылал скриншот с ошибкой, а версии, режимы и итоги прогона
//! приходилось выспрашивать по одному. Личного здесь нет: ни списков
//! доменов, ни собранных адресов игр, ни секрета Telegram-прокси.

use std::path::Path;
use tauri::AppHandle;

use crate::state::AppState;

fn да_нет(b: bool) -> &'static str {
    if b {
        "да"
    } else {
        "нет"
    }
}

/// Итоги последнего прогона: режим, пять лучших и кто не запустился.
pub fn results_summary(text: &str, configs: &[String]) -> Vec<String> {
    let (rows, dpi) = crate::tests::parse_results(text);
    if rows.is_empty() {
        return vec!["итогов нет".into()];
    }
    let mut sorted: Vec<&crate::tests::ResultRow> = rows.iter().collect();
    sorted.sort_by(|a, b| crate::tests::rank_desc(a, b, dpi));
    let mut out = vec![format!(
        "режим: {}, конфигов в итогах: {}",
        if dpi { "DPI" } else { "HTTP/Ping" },
        rows.len()
    )];
    for r in sorted.iter().take(5) {
        out.push(format!("  {} — {} из {}", r.config.trim_end_matches(".bat"), r.ok, r.total(dpi)));
    }
    let skipped = crate::tests::skipped_configs(text, configs);
    if !skipped.is_empty() {
        out.push(format!(
            "не запустились: {}",
            skipped.iter().map(|s| s.trim_end_matches(".bat")).collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

pub fn build(app: &AppHandle, state: &AppState) -> String {
    let p = state.persisted.lock().unwrap().clone();
    let root = p.root_path.as_deref().map(Path::new);
    let mut l = vec!["— Klutz —".to_string()];
    l.push(format!("Klutz: {}", app.package_info().version));
    l.push(format!("Windows: {}", crate::sys::run("cmd", &["/c", "ver"]).trim()));
    match root {
        Some(r) => l.push(format!(
            "Релиз zapret: {} (версия {})",
            crate::history::release_label(r),
            crate::maintenance::local_version(r).unwrap_or_else(|| "?".into())
        )),
        None => l.push("Релиз zapret: не загружен".into()),
    }
    l.push(format!("TgWsProxy: {}", crate::tgws::BUNDLED_VERSION));

    l.push(String::new());
    l.push("— Обход —".into());
    l.push(format!(
        "Стратегия: {}",
        p.active_config.as_deref().map(|c| c.trim_end_matches(".bat")).unwrap_or("не выбрана")
    ));
    l.push(format!(
        "winws запущен: {}; служба zapret: {}; держится службой: {}",
        да_нет(crate::winws::is_winws_running()),
        да_нет(crate::service::service_conflict()),
        да_нет(p.installed_as_service)
    ));
    if let Some(r) = root {
        let t = crate::toggles::read_toggles(r);
        l.push(format!("Game Filter: {}; IPSet: {}", t.game_mode, t.ipset_mode));
    }
    l.push(format!(
        "Системный прокси: {}",
        match crate::discorddiag::system_proxy() {
            None => "выключен".to_string(),
            Some(px) => format!("{}{}", px.server, px.owner.map(|o| format!(" ({o})")).unwrap_or_default()),
        }
    ));
    l.push(format!("Discord без QUIC: {}", if crate::quic::enabled() { "включено" } else { "выключено" }));

    if let Some(r) = root {
        l.push(String::new());
        l.push("— Последний прогон —".into());
        let last = crate::tests::newest_result_file(r)
            .and_then(|f| Some((f.file_name()?.to_string_lossy().into_owned(), std::fs::read_to_string(&f).ok()?)));
        match last {
            Some((name, text)) => {
                l.push(name);
                l.extend(results_summary(&text, &crate::release::list_configs(r)));
            }
            None => l.push("прогонов не было".into()),
        }
    }

    let runs = crate::history::runs(app);
    if !runs.is_empty() {
        let mut per: std::collections::BTreeMap<String, usize> = Default::default();
        for r in &runs {
            *per.entry(r.release.clone()).or_default() += 1;
        }
        l.push(String::new());
        l.push(format!(
            "История прогонов: {}",
            per.iter()
                .map(|(k, v)| format!("{} ({v})", k.trim_start_matches("zapret-discord-youtube-")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    l.join("\n")
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn итоги_прогона_для_отчёта() {
        let итоги = "=== ANALYTICS ===\n\
                     general.bat : HTTP OK: 17, ERR: 19, UNSUP: 0, Ping OK: 16, Fail: 0\n\
                     general (ALT12).bat : HTTP OK: 34, ERR: 2, UNSUP: 0, Ping OK: 16, Fail: 0\n";
        let configs: Vec<String> =
            ["general (ALT).bat", "general (ALT12).bat", "general.bat"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            results_summary(итоги, &configs),
            vec![
                "режим: HTTP/Ping, конфигов в итогах: 2",
                "  general (ALT12) — 34 из 36",
                "  general — 17 из 36",
                "не запустились: general (ALT)",
            ]
        );
        assert_eq!(results_summary("", &configs), vec!["итогов нет"]);
    }
}
