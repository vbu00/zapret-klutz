//! Что поменялось в конфиге между релизами — и проверка прежнего варианта.
//!
//! На 1.10.2 ALT11 упал с 36 до 12, а в самом конфиге поменялся один файл
//! подложки: stun.bin на stun2.bin. Найти это можно было, только сличив два
//! .bat глазами. Здесь сравнение по аргументам winws, по профилям, а прежний
//! вариант можно положить рядом и прогнать тестами на нынешнем winws.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Аргументы winws из .bat по профилям (`--new` их разделяет), как написаны:
/// без подстановки %BIN% и %LISTS% — у двух релизов пути разные, и после
/// подстановки не совпала бы ни одна строка.
pub fn profiles(bat: &str) -> Vec<Vec<String>> {
    let text = crate::winws::LINE_CONTINUATION.replace_all(bat, " ");
    let Some(line) = text.lines().find(|l| crate::winws::EXE_MARKER.is_match(l)) else { return Vec::new() };
    let Some(m) = crate::winws::EXE_MARKER.find(line) else { return Vec::new() };
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for t in crate::winws::TOKEN_RE.find_iter(&line[m.end()..]) {
        let t = crate::winws::unescape_token(t.as_str());
        if t.is_empty() || t == "^" {
            continue;
        }
        if t == "--new" {
            out.push(Vec::new());
            continue;
        }
        if let Some(p) = out.last_mut() {
            p.push(t);
        }
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Значение для человека: без переменных путей.
fn показ(v: &str) -> String {
    v.replace("%BIN%", "")
        .replace("%LISTS%", "")
        .replace("%GameFilterTCP%", "игровые порты")
        .replace("%GameFilterUDP%", "игровые порты")
}

/// Чем профиль узнаётся человеком — его фильтром портов.
fn подпись(p: &[String], i: usize) -> String {
    let фильтр = |key: &str| p.iter().find_map(|t| t.strip_prefix(key)).map(показ);
    match (фильтр("--filter-tcp="), фильтр("--filter-udp=")) {
        (Some(t), _) => format!("TCP {t}"),
        (None, Some(u)) => format!("UDP {u}"),
        _ if i == 0 => "общие параметры".into(),
        _ => format!("профиль {}", i + 1),
    }
}

fn по_ключам(p: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for t in p {
        let (k, v) = match t.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (t.clone(), String::new()),
        };
        m.entry(k).or_default().push(v);
    }
    m
}

/// Мультимножество: что есть в `a` сверх `b`. Ключи вроде
/// `--dpi-desync-fake-tls` встречаются в профиле по нескольку раз.
fn разность(a: &[String], b: &[String]) -> Vec<String> {
    let mut rest = b.to_vec();
    a.iter()
        .filter(|x| match rest.iter().position(|y| y == *x) {
            Some(i) => {
                rest.remove(i);
                false
            }
            None => true,
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub profile: String,
    pub text: String,
}

/// Изменения от прежнего конфига к нынешнему, по профилям.
pub fn diff(old_bat: &str, new_bat: &str) -> Vec<Change> {
    let (old, new) = (profiles(old_bat), profiles(new_bat));
    let mut out = Vec::new();
    if old.len() != new.len() {
        out.push(Change {
            profile: "конфиг".into(),
            text: format!("профилей было {}, стало {}", old.len(), new.len()),
        });
    }
    for (i, (o, n)) in old.iter().zip(new.iter()).enumerate() {
        let profile = подпись(n, i);
        let (om, nm) = (по_ключам(o), по_ключам(n));
        let keys: BTreeSet<&String> = om.keys().chain(nm.keys()).collect();
        for k in keys {
            let пусто = Vec::new();
            let ov = om.get(k).unwrap_or(&пусто);
            let nv = nm.get(k).unwrap_or(&пусто);
            let убрано = разность(ov, nv);
            let добавлено = разность(nv, ov);
            let значения = |vs: &[String]| {
                if vs.iter().all(|v| v.is_empty()) {
                    k.clone()
                } else {
                    format!("{k}={}", vs.iter().map(|v| показ(v)).collect::<Vec<_>>().join(", "))
                }
            };
            let text = match (убрано.is_empty(), добавлено.is_empty()) {
                (true, true) => continue,
                (false, false) => format!(
                    "{k}: {} → {}",
                    убрано.iter().map(|v| показ(v)).collect::<Vec<_>>().join(", "),
                    добавлено.iter().map(|v| показ(v)).collect::<Vec<_>>().join(", ")
                ),
                (true, false) => format!("+ {}", значения(&добавлено)),
                (false, true) => format!("− {}", значения(&убрано)),
            };
            out.push(Change { profile: profile.clone(), text });
        }
    }
    out
}

/// Имя прежнего варианта среди конфигов нового релиза.
pub fn old_name(release_label: &str, config: &str) -> String {
    let short = release_label.trim_start_matches("zapret-discord-youtube-");
    format!("{}{short} {}).bat", crate::strategies::OLD_MARK, config.trim_end_matches(".bat"))
}

/// Кладёт конфиг прежнего релиза в текущий под своим именем и докладывает
/// файлы подложек и списков, которых в новом релизе нет. Возвращает имя.
///
/// Бинарник winws остаётся нынешним — так и видно, в конфиге ли дело: на
/// 1.9.9c и 1.10.2 winws.exe один и тот же, разнятся только конфиги.
pub fn import_old(old_root: &Path, new_root: &Path, config: &str) -> Result<String, String> {
    let text = fs::read_to_string(old_root.join(config)).map_err(|e| e.to_string())?;
    let name = old_name(&crate::history::release_label(old_root), config);
    if !crate::commands::safe_name(&name) {
        return Err("Недопустимое имя конфига.".into());
    }
    for t in profiles(&text).iter().flatten() {
        for (var, dir) in [("%BIN%", "bin"), ("%LISTS%", "lists")] {
            let Some(file) = t.split(var).nth(1) else { continue };
            // Имя файла из чужого .bat — только имя, без путей.
            if !crate::commands::safe_name(file) {
                continue;
            }
            let (src, dst) = (old_root.join(dir).join(file), new_root.join(dir).join(file));
            if src.is_file() && !dst.exists() {
                fs::copy(&src, &dst).map_err(|e| e.to_string())?;
            }
        }
    }
    fs::write(new_root.join(&name), text).map_err(|e| e.to_string())?;
    Ok(name)
}

/// Доля целей, начиная с которой конфиг считается работавшим.
pub const GOOD_SHARE: f64 = 0.9;

/// Конфиги прогона, которые работали: прошли не меньше `GOOD_SHARE` целей.
/// Варианты (Z2K, «Было …») не в счёт — переносить вариант варианта незачем.
pub fn good_configs(results: &str) -> Vec<String> {
    let (rows, dpi) = crate::tests::parse_results(results);
    rows.into_iter()
        .filter(|r| r.score(dpi) >= GOOD_SHARE && !crate::strategies::is_variant(&r.config))
        .map(|r| r.config)
        .collect()
}

/// При смене релиза: прежние варианты конфигов, которые на прежнем работали,
/// а в новом изменились, ложатся рядом как «Было …». Возвращает их имена.
///
/// Не все двадцать прежних — это вдвое удлинило бы прогон, — а только те,
/// ради которых стоит: ALT11 из 1.9.9c давал 36 из 36, а на 1.10.2 его
/// изменили и он упал до 12. Прежний вариант так и остаётся в тестах и в
/// рейтинге самолечения нового релиза.
pub fn import_good_old(app: &tauri::AppHandle, old_root: &Path, new_root: &Path) -> Vec<String> {
    let label = crate::history::release_label(old_root);
    let Some(last) = crate::history::runs(app).into_iter().rev().find(|r| r.release == label) else {
        return Vec::new();
    };
    let Ok(results) = fs::read_to_string(&last.path) else { return Vec::new() };
    let mut out = Vec::new();
    for config in good_configs(&results) {
        let (Ok(old_bat), Ok(new_bat)) =
            (fs::read_to_string(old_root.join(&config)), fs::read_to_string(new_root.join(&config)))
        else {
            continue;
        };
        if diff(&old_bat, &new_bat).is_empty() || new_root.join(old_name(&label, &config)).exists() {
            continue;
        }
        if let Ok(name) = import_old(old_root, new_root, &config) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn работавшие_конфиги_прогона() {
        let итоги = "=== ANALYTICS ===\n\
                     general (ALT11).bat : HTTP OK: 36, ERR: 0, UNSUP: 0, Ping OK: 17, Fail: 0\n\
                     general (ALT12).bat : HTTP OK: 32, ERR: 4, UNSUP: 0, Ping OK: 17, Fail: 0\n\
                     general.bat : HTTP OK: 7, ERR: 27, UNSUP: 0, Ping OK: 16, Fail: 0\n\
                     general (Z2K general (ALT11) sld1).bat : HTTP OK: 36, ERR: 0, UNSUP: 0, Ping OK: 17, Fail: 0\n";
        // 32 из 36 — это 0,89: не дотягивает до «работал».
        assert_eq!(good_configs(итоги), vec!["general (ALT11).bat"]);
        assert!(good_configs("").is_empty());
    }

    /// Кусок настоящего ALT11: TLS-профиль, где 1.10.2 заменил подложку.
    fn alt11(stun: &str, extra: &str) -> String {
        format!(
            "@echo off\r\ncd /d \"%~dp0\"\r\nstart \"zapret: %~n0\" /min \"%BIN%winws.exe\" --wf-tcp=80,443 ^\r\n\
             --filter-udp=443 --hostlist=\"%LISTS%list-general.txt\" --dpi-desync=fake --new ^\r\n\
             --filter-tcp=80,443 --hostlist=\"%LISTS%list-general.txt\" --dpi-desync=fake,multisplit \
             --dpi-desync-fake-tls=\"%BIN%{stun}\" --dpi-desync-fake-tls=\"%BIN%tls_clienthello_max_ru.bin\"{extra}\r\n"
        )
    }

    #[test]
    fn профили_без_подстановки_путей() {
        let p = profiles(&alt11("stun.bin", ""));
        assert_eq!(p.len(), 2);
        assert_eq!(p[0][0], "--wf-tcp=80,443");
        assert!(p[1].contains(&"--dpi-desync-fake-tls=%BIN%stun.bin".to_string()), "{:?}", p[1]);
    }

    #[test]
    fn живой_случай_alt11() {
        let old = alt11("stun.bin", "");
        let new = alt11("stun2.bin", " --dpi-desync-fake-unknown=\"%BIN%stun2.bin\"");
        let got: Vec<String> = diff(&old, &new).into_iter().map(|c| format!("{}: {}", c.profile, c.text)).collect();
        assert_eq!(
            got,
            vec![
                "TCP 80,443: --dpi-desync-fake-tls: stun.bin → stun2.bin",
                "TCP 80,443: + --dpi-desync-fake-unknown=stun2.bin",
            ]
        );
        assert!(diff(&old, &old).is_empty(), "одинаковые конфиги — без изменений");
    }

    #[test]
    fn прежний_вариант_ложится_рядом_с_недостающими_файлами() {
        let base = std::env::temp_dir().join(format!("klutz-configdiff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (old, new) = (base.join("zapret-discord-youtube-1.9.9c"), base.join("zapret-discord-youtube-1.10.2"));
        for r in [&old, &new] {
            fs::create_dir_all(r.join("bin")).unwrap();
            fs::create_dir_all(r.join("lists")).unwrap();
            fs::write(r.join("lists").join("list-general.txt"), "discord.com").unwrap();
        }
        fs::write(old.join("bin").join("stun.bin"), "old").unwrap();
        fs::write(old.join("bin").join("tls_clienthello_max_ru.bin"), "old").unwrap();
        fs::write(new.join("bin").join("tls_clienthello_max_ru.bin"), "new").unwrap();
        fs::write(old.join("general (ALT11).bat"), alt11("stun.bin", "")).unwrap();

        let name = import_old(&old, &new, "general (ALT11).bat").unwrap();
        assert_eq!(name, "general (Было 1.9.9c general (ALT11)).bat");
        assert!(new.join(&name).is_file());
        assert_eq!(fs::read_to_string(new.join("bin").join("stun.bin")).unwrap(), "old", "недостающая подложка докладывается");
        assert_eq!(
            fs::read_to_string(new.join("bin").join("tls_clienthello_max_ru.bin")).unwrap(),
            "new",
            "имеющийся файл нового релиза не затирается"
        );
        assert!(crate::strategies::is_variant(&name), "убирается той же кнопкой, что дополнительные стратегии");
        let _ = fs::remove_dir_all(&base);
    }
}
