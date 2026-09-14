//! Архив прогонов тестов — в папке Klutz, а не релиза.
//!
//! Скрипт тестов zapret пишет результаты в `utils\test results` своего
//! релиза. Сменили релиз — история осталась в старой папке, и «Снимки
//! прогонов» показывали один прогон. Сравнить новый релиз со старым было не
//! с чем, хотя после обновления нужно именно это: на 1.10.2 ALT11 упал с 36
//! до 12, и заметил это человек сам, сличая файлы руками.

use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::tests::{parse_results, rank_desc};

pub fn history_dir(app: &AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().expect("no app data dir").join("test-history");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Подпись релиза — имя его папки: `zapret-discord-youtube-1.10.2`.
pub fn release_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "релиз".into())
}

/// Копирует в архив прогоны релиза, которых там ещё нет.
///
/// Копия, а не перенос: файлы в папке релиза читает и сам zapret, и
/// самолечение Klutz — рейтинг текущего релиза берётся оттуда.
pub fn archive(app: &AppHandle, root: &Path) {
    let Ok(entries) = std::fs::read_dir(root.join("utils").join("test results")) else { return };
    let label = release_label(root);
    if !crate::commands::safe_name(&label) {
        return;
    }
    let dst = history_dir(app).join(&label);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.to_lowercase().ends_with(".txt") {
            continue;
        }
        let target = dst.join(&name);
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::metadata(&target).map(|m| m.len() == len).unwrap_or(false) {
            continue;
        }
        let _ = std::fs::create_dir_all(&dst);
        // Время изменения копия сохраняет, а по нему архив и сортируется.
        let _ = std::fs::copy(e.path(), &target);
    }
}

/// Прогон из архива.
pub struct Run {
    pub release: String,
    pub name: String,
    pub path: PathBuf,
    pub modified: std::time::SystemTime,
}

/// Все прогоны архива по всем релизам, от старого к новому.
pub fn runs(app: &AppHandle) -> Vec<Run> {
    runs_in(&history_dir(app))
}

pub fn runs_in(dir: &Path) -> Vec<Run> {
    let mut out = Vec::new();
    let Ok(releases) = std::fs::read_dir(dir) else { return out };
    for r in releases.flatten() {
        if !r.path().is_dir() {
            continue;
        }
        let release = r.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(r.path()) else { continue };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().ends_with(".txt") {
                continue;
            }
            let Ok(modified) = f.metadata().and_then(|m| m.modified()) else { continue };
            out.push(Run { release: release.clone(), name, path: f.path(), modified });
        }
    }
    out.sort_by_key(|r| r.modified);
    out
}

/// Конфиг, который на новом релизе проседает.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigDrop {
    pub name: String,
    #[serde(rename = "prevOk")]
    pub prev_ok: u32,
    #[serde(rename = "curOk")]
    pub cur_ok: u32,
    pub total: u32,
}

/// Текущий прогон против прежнего — когда текущий хуже.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    #[serde(rename = "curBest")]
    pub cur_best: String,
    #[serde(rename = "curOk")]
    pub cur_ok: u32,
    #[serde(rename = "curTotal")]
    pub cur_total: u32,
    #[serde(rename = "prevBest")]
    pub prev_best: String,
    #[serde(rename = "prevOk")]
    pub prev_ok: u32,
    #[serde(rename = "prevTotal")]
    pub prev_total: u32,
    pub drops: Vec<ConfigDrop>,
}

/// Лучший конфиг прогона.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Best {
    pub name: String,
    pub ok: u32,
    pub total: u32,
}

/// Лучший конфиг по тому же порядку, что у самолечения и истории.
pub fn best_of(text: &str) -> Option<Best> {
    let (rows, dpi) = parse_results(text);
    let b = rows.iter().min_by(|a, b| rank_desc(a, b, dpi))?;
    Some(Best { name: b.config.clone(), ok: b.ok, total: b.total(dpi) })
}

/// На сколько должен упасть лучший результат, чтобы назвать релиз хуже.
/// Меньше — это разброс между прогонами: одна цель ответила, другая нет.
pub const WORSE_BY: f64 = 0.10;

/// На сколько должен просесть отдельный конфиг, чтобы попасть в список.
pub const DROP_BY: f64 = 0.25;

/// Хуже ли текущий прогон прежнего. `None` — не хуже или сравнивать нельзя.
///
/// Двух признаков, а не одного. Лучший результат сам по себе обманывает:
/// на 1.9.9c лучшим был ALT11 с 36 из 36, на 1.10.2 — ALT12 с 34 из 36,
/// разница шесть процентов, а тот самый ALT11, на котором всё работало,
/// упал до 12. Поэтому хуже — это и заметное падение лучшего, и провал
/// прежнего лучшего конфига.
pub fn compare(cur: &str, prev: &str) -> Option<Comparison> {
    let (cur_rows, cur_dpi) = parse_results(cur);
    let (prev_rows, prev_dpi) = parse_results(prev);
    // Режимы разные — числа несравнимы: у DPI и HTTP разные цели и итоги.
    if cur_rows.is_empty() || prev_rows.is_empty() || cur_dpi != prev_dpi {
        return None;
    }
    let dpi = cur_dpi;
    let cb = cur_rows.iter().min_by(|a, b| rank_desc(a, b, dpi))?;
    let pb = prev_rows.iter().min_by(|a, b| rank_desc(a, b, dpi))?;

    let mut drops: Vec<ConfigDrop> = cur_rows
        .iter()
        .filter_map(|c| {
            let p = prev_rows.iter().find(|p| p.config == c.config)?;
            (p.score(dpi) - c.score(dpi) >= DROP_BY).then(|| ConfigDrop {
                name: c.config.clone(),
                prev_ok: p.ok,
                cur_ok: c.ok,
                total: c.total(dpi),
            })
        })
        .collect();
    let прежний_лучший_провалился = drops.iter().any(|d| d.name == pb.config);
    if pb.score(dpi) - cb.score(dpi) < WORSE_BY && !прежний_лучший_провалился {
        return None;
    }
    drops.sort_by_key(|d| (std::cmp::Reverse(d.prev_ok.saturating_sub(d.cur_ok)), d.name.clone()));
    drops.truncate(5);
    Some(Comparison {
        cur_best: cb.config.clone(),
        cur_ok: cb.ok,
        cur_total: cb.total(dpi),
        prev_best: pb.config.clone(),
        prev_ok: pb.ok,
        prev_total: pb.total(dpi),
        drops,
    })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn итоги(rows: &[(&str, u32, u32)]) -> String {
        let mut s = String::from("=== ANALYTICS ===\n");
        for (name, ok, err) in rows {
            s.push_str(&format!("{name} : HTTP OK: {ok}, ERR: {err}, UNSUP: 0, Ping OK: 16, Fail: 0\n"));
        }
        s
    }

    #[test]
    fn живой_случай_1_9_9c_против_1_10_2() {
        // Настоящие числа из прогонов: лучший упал на шесть процентов, а
        // ALT11, на котором всё работало, — втрое.
        let старый = итоги(&[("general (ALT11).bat", 36, 0), ("general (ALT12).bat", 32, 4), ("general.bat", 7, 27)]);
        let новый = итоги(&[("general (ALT11).bat", 12, 24), ("general (ALT12).bat", 34, 2), ("general.bat", 17, 19)]);
        let c = compare(&новый, &старый).expect("1.10.2 хуже");
        assert_eq!((c.prev_best.as_str(), c.prev_ok), ("general (ALT11).bat", 36));
        assert_eq!((c.cur_best.as_str(), c.cur_ok), ("general (ALT12).bat", 34));
        assert_eq!(
            c.drops,
            vec![ConfigDrop { name: "general (ALT11).bat".into(), prev_ok: 36, cur_ok: 12, total: 36 }]
        );
    }

    #[test]
    fn лучший_конфиг_прогона() {
        let t = итоги(&[("general.bat", 17, 19), ("general (ALT12).bat", 34, 2)]);
        assert_eq!(best_of(&t), Some(Best { name: "general (ALT12).bat".into(), ok: 34, total: 36 }));
        assert_eq!(best_of(""), None);
    }

    #[test]
    fn разброс_между_прогонами_не_тревога() {
        let старый = итоги(&[("general (ALT11).bat", 36, 0), ("general.bat", 20, 16)]);
        let новый = итоги(&[("general (ALT11).bat", 34, 2), ("general.bat", 22, 14)]);
        assert_eq!(compare(&новый, &старый), None);
        // Стало лучше — тем более не тревога.
        assert_eq!(compare(&старый, &итоги(&[("general (ALT11).bat", 20, 16)])), None);
    }

    #[test]
    fn режимы_не_смешиваются() {
        let http = итоги(&[("general.bat", 36, 0)]);
        let dpi = "=== ANALYTICS ===\ngeneral.bat: OK: 1, ERR: 5, UNSUP: 0, BLOCKED: 0\n";
        assert_eq!(compare(dpi, &http), None);
        assert_eq!(compare("", &http), None);
    }

    #[test]
    fn архив_читается_по_релизам() {
        let dir = std::env::temp_dir().join(format!("klutz-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, file) in [("zapret-1.9.9c", "a.txt"), ("zapret-1.10.2", "b.txt"), ("zapret-1.10.2", "note.log")] {
            std::fs::create_dir_all(dir.join(rel)).unwrap();
            std::fs::write(dir.join(rel).join(file), "x").unwrap();
        }
        let runs = runs_in(&dir);
        let got: std::collections::BTreeSet<(String, String)> =
            runs.iter().map(|r| (r.release.clone(), r.name.clone())).collect();
        assert_eq!(
            got,
            [("zapret-1.10.2".to_string(), "b.txt".to_string()), ("zapret-1.9.9c".to_string(), "a.txt".to_string())]
                .into_iter()
                .collect()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
