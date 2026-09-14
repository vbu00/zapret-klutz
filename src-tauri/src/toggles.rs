use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Toggles {
    #[serde(rename = "gameMode")]
    pub game_mode: String,
    #[serde(rename = "ipsetMode")]
    pub ipset_mode: String,
    #[serde(rename = "autoUpdate")]
    pub auto_update: bool,
}

fn game_filter_path(root: &Path) -> std::path::PathBuf {
    root.join("utils").join("game_filter.enabled")
}

pub fn current_game_filter(root: &Path) -> String {
    match fs::read_to_string(game_filter_path(root)) {
        Ok(c) => {
            let c = c.trim().to_lowercase();
            if c == "tcp" || c == "udp" || c == "all" {
                c
            } else {
                "all".into()
            }
        }
        Err(_) => "off".into(),
    }
}

pub fn set_game_filter(root: &Path, mode: &str) -> Result<(), String> {
    let p = game_filter_path(root);
    if mode == "off" {
        if p.exists() {
            fs::remove_file(p).map_err(|e| e.to_string())?;
        }
    } else {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        fs::write(p, mode).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// «none» — это реальный адрес из TEST-NET-3 (RFC 5737), который никогда не
/// ответит, а не пустой файл: winws всё равно получает список целей, просто
/// такой, под который ничего живого не попадёт. Пустой файл («any») означал бы
/// «без ограничения по IP, фильтруем только по порту».
///
/// Считаем по настоящим строкам. Раньше хватало, чтобы заглушка встретилась
/// ГДЕ-ТО в файле, и загруженный список, в который она попала, выглядел
/// пустым — а переключение из «none» перезаписывает файл без резервной копии.
/// Комментарии для winws не адреса: файл из одних комментариев — это «any».
pub fn ipset_mode_from(content: &str) -> String {
    let real: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if real.is_empty() {
        return "any".into();
    }
    if real.iter().all(|l| *l == "203.0.113.113/32") {
        return "none".into();
    }
    "loaded".into()
}

pub fn read_toggles(root: &Path) -> Toggles {
    let ipset_path = root.join("lists").join("ipset-all.txt");
    let ipset_mode = fs::read_to_string(&ipset_path)
        .map(|c| ipset_mode_from(&c))
        .unwrap_or_else(|_| "any".into());
    Toggles {
        game_mode: current_game_filter(root),
        ipset_mode,
        auto_update: root.join("utils").join("check_updates.enabled").exists(),
    }
}

pub fn cycle_ipset(root: &Path) -> Result<(), String> {
    let list = root.join("lists").join("ipset-all.txt");
    let backup = root.join("lists").join("ipset-all.txt.backup");
    let content = fs::read_to_string(&list).unwrap_or_default();
    match ipset_mode_from(&content).as_str() {
        "loaded" => {
            fs::copy(&list, &backup).map_err(|e| e.to_string())?;
            fs::write(&list, "203.0.113.113/32\n").map_err(|e| e.to_string())?;
        }
        "none" => fs::write(&list, "").map_err(|e| e.to_string())?,
        _ => {
            if !backup.exists() {
                return Err("Нет бэкапа для восстановления — сначала обнови список IPSet.".into());
            }
            fs::copy(&backup, &list).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub fn set_auto_update(root: &Path, enabled: bool) -> Result<(), String> {
    let p = root.join("utils").join("check_updates.enabled");
    if enabled {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        fs::write(p, "ENABLED\n").map_err(|e| e.to_string())?;
    } else if p.exists() {
        fs::remove_file(p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn режим_ipset_по_содержимому() {
        assert_eq!(ipset_mode_from(""), "any");
        assert_eq!(ipset_mode_from("   \n\n"), "any");
        assert_eq!(ipset_mode_from("203.0.113.113/32\n"), "none");
        assert_eq!(ipset_mode_from("1.2.3.0/24\n5.6.7.8\n"), "loaded");
    }

    #[test]
    fn заглушка_внутри_загруженного_списка_не_делает_его_пустым() {
        // Ровно так было на живой машине: 32 тысячи строк и блок Klutz, куда
        // сбор положил заглушку, потому что все пойманные адреса облачные.
        let файл = "1.0.0.0/24\n1.1.1.0/24\n# ─── klutz ───\n203.0.113.113/32\n# klutz-пропущено 35.181.49.176\n";
        assert_eq!(ipset_mode_from(файл), "loaded");
        assert_eq!(ipset_mode_from("# только комментарий\n"), "any");
        assert_eq!(ipset_mode_from("203.0.113.113/32\n# klutz-пропущено 1.2.3.4\n"), "none");
    }
}
