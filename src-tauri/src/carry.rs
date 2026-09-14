//! Перенос настроек из прежнего релиза zapret в новый.
//!
//! Всё, что человек настраивал, живёт файлами внутри папки релиза: Game
//! Filter, свои списки доменов, список IPSet с собранными адресами игр,
//! дополнительные стратегии. Новый релиз распаковывается в новую папку, и
//! раньше всё это молча оставалось в старой. Живой случай: после перехода на
//! 1.10.2 Game Filter оказался выключен, IPSet — пуст, а сбор адресов игры
//! «не находил ничего».
//!
//! Правило одно: перенос ничего не портит. Откат со свежего релиза на
//! старый — это тоже смена корня, и затереть при нём загруженный список
//! заглушкой или свои строки чужими было бы хуже, чем не переносить вовсе.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::gamescan as gs;
use crate::toggles::ipset_mode_from;

/// Заготовки, которые кладёт service.bat. Это не данные человека.
const ЗАГОТОВКИ: &[&str] = &["domain.example.abc", gs::EMPTY_STUB];

/// Настоящие строки списка: без пустых, комментариев и заготовок.
fn свои_строки(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !ЗАГОТОВКИ.contains(l))
        .collect()
}

/// Пользовательский список после переноса. `None` — менять нечего.
///
/// Не перезапись, а объединение: в новом релизе человек мог уже что-то
/// добавить, и при откате это не должно пропасть.
pub fn перенести_список(old: &str, new: &str) -> Option<String> {
    let добавить: Vec<&str> = {
        let есть: BTreeSet<&str> = свои_строки(new).into_iter().collect();
        свои_строки(old).into_iter().filter(|l| !есть.contains(l)).collect()
    };
    if добавить.is_empty() {
        return None;
    }
    // В новом только заготовка — заменяем её целиком: рядом с настоящими
    // строками она не нужна.
    let mut out = if свои_строки(new).is_empty() {
        String::new()
    } else {
        let mut s = new.trim_end().to_string();
        s.push_str("\r\n");
        s
    };
    for l in добавить {
        out.push_str(l);
        out.push_str("\r\n");
    }
    Some(out)
}

/// Новый `ipset-all.txt`. `None` — менять нечего.
///
/// Загруженный список переезжает, только если в новом релизе его нет:
/// свежий релиз Flowseal кладёт заглушку, и обновлённый список иначе
/// пришлось бы качать заново. Обратно — никогда: откат не должен затирать
/// загруженный список заглушкой. Адреса игр переезжают всегда и
/// объединяются с теми, что уже есть в новом.
pub fn перенести_ipset(old: &str, new: &str) -> Option<String> {
    let old_base = gs::without_block(old);
    let new_base = gs::without_block(new);
    let base = if ipset_mode_from(&old_base) == "loaded" && ipset_mode_from(&new_base) != "loaded" {
        old_base
    } else {
        new_base
    };

    let (old_groups, old_skipped) = gs::parse_groups(old);
    let (mut groups, mut skipped) = gs::parse_groups(new);
    for g in old_groups {
        if g.legacy {
            // Сети без оператора — отдельной безымянной группой, как были.
            let занято: BTreeSet<&String> = groups.iter().flat_map(|x| x.nets.iter()).collect();
            let nets: Vec<String> = g.nets.iter().filter(|n| !занято.contains(n)).cloned().collect();
            if !nets.is_empty() {
                groups.push(gs::Group { nets, ..g });
            }
        } else {
            gs::добавить_группу(&mut groups, g.asn, g.name, g.at, g.nets);
        }
    }
    for s in old_skipped {
        if !skipped.iter().any(|x| x.addr == s.addr) {
            skipped.push(s);
        }
    }

    let text = if groups.is_empty() && skipped.is_empty() {
        let mut b = base.trim_end().to_string();
        if !b.is_empty() {
            b.push_str("\r\n");
        }
        b
    } else {
        gs::merge_groups(&base, &groups, &skipped)
    };
    // Пустой ipset у winws значит «применяться ко всему». Если файл вдруг
    // остался без адресов, а был с ними, — лучше не трогать его вовсе.
    if ipset_mode_from(&text) == "any" && ipset_mode_from(new) != "any" {
        return None;
    }
    let норм = |s: &str| s.replace("\r\n", "\n").trim().to_string();
    if норм(&text) == норм(new) {
        return None;
    }
    Some(text)
}

fn групп(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, x) if x != 11 => "группа",
        (2..=4, x) if !(12..=14).contains(&x) => "группы",
        _ => "групп",
    }
}

/// Переносит настройки. Возвращает, что перенесено, строками для человека;
/// пусто — переносить было нечего.
pub fn carry_over(old: &Path, new: &Path) -> Vec<String> {
    let mut done = Vec::new();
    if old == new || !old.join("lists").is_dir() || !new.join("lists").is_dir() {
        return done;
    }

    // Game Filter: последний выбор человека.
    let gf = crate::toggles::current_game_filter(old);
    if gf != crate::toggles::current_game_filter(new) && crate::toggles::set_game_filter(new, &gf).is_ok() {
        done.push(format!("Game Filter ({})", crate::commands::описание_фильтра(&gf)));
    }

    // Свои списки доменов и исключений.
    let mut списков = 0;
    for name in ["list-general-user.txt", "list-exclude-user.txt", "ipset-exclude-user.txt"] {
        let Ok(old_text) = fs::read_to_string(old.join("lists").join(name)) else { continue };
        let dst = new.join("lists").join(name);
        let new_text = fs::read_to_string(&dst).unwrap_or_default();
        if let Some(text) = перенести_список(&old_text, &new_text) {
            if fs::write(&dst, text).is_ok() {
                списков += 1;
            }
        }
    }
    if списков > 0 {
        done.push("свои списки доменов и исключений".into());
    }

    // IPSet и адреса игр.
    let old_ipset = fs::read_to_string(old.join("lists").join("ipset-all.txt")).unwrap_or_default();
    let new_path = new.join("lists").join("ipset-all.txt");
    let new_ipset = fs::read_to_string(&new_path).unwrap_or_default();
    if let Some(text) = перенести_ipset(&old_ipset, &new_ipset) {
        if fs::write(&new_path, &text).is_ok() {
            let n = gs::parse_groups(&old_ipset).0.len();
            done.push(if n > 0 {
                format!("список IPSet и адреса игр ({n} {})", групп(n))
            } else {
                "список IPSet".into()
            });
        }
    }
    // Резервная копия списка: из неё переключатель режима восстанавливает
    // загруженный список. Без неё в новом релизе он был бы невосстановим.
    let (old_backup, new_backup) = (
        old.join("lists").join("ipset-all.txt.backup"),
        new.join("lists").join("ipset-all.txt.backup"),
    );
    if old_backup.exists() && !new_backup.exists() {
        let _ = fs::copy(old_backup, new_backup);
    }

    // Дополнительные стратегии: пересоздаём от тех же образцов, если они есть
    // в новом релизе. Копировать старые файлы нельзя — в них старые конфиги.
    let new_configs = crate::release::list_configs(new);
    let шаблоны: BTreeSet<String> = crate::release::list_configs(old)
        .iter()
        .filter(|c| crate::strategies::is_variant(c))
        .filter_map(|c| crate::strategies::template_of(c))
        .collect();
    let mut стратегий = 0;
    for t in шаблоны {
        if !new_configs.contains(&t) {
            continue;
        }
        let уже = new_configs.iter().any(|c| {
            crate::strategies::is_variant(c) && crate::strategies::template_of(c).as_deref() == Some(t.as_str())
        });
        if уже {
            continue;
        }
        if let Ok(made) = crate::strategies::generate(new, &t) {
            стратегий += made.len();
        }
    }
    if стратегий > 0 {
        done.push(format!("дополнительные стратегии ({стратегий})"));
    }
    done
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn заготовки_не_переносятся_а_свои_строки_объединяются() {
        let заготовка = "# Never leave this file empty\r\ndomain.example.abc\r\n";
        assert_eq!(перенести_список(заготовка, заготовка), None);
        // Своё из старого заменяет заготовку нового.
        let стало = перенести_список("mysite.ru\r\n", заготовка).unwrap();
        assert!(стало.contains("mysite.ru") && !стало.contains("domain.example.abc"), "{стало:?}");
        // Добавленное в новом не теряется, повторы не дублируются.
        let стало = перенести_список("a.ru\r\nb.ru\r\n", "b.ru\r\nc.ru\r\n").unwrap();
        assert_eq!(стало.replace("\r\n", "\n"), "b.ru\nc.ru\na.ru\n");
        assert_eq!(перенести_список("a.ru\n", "a.ru\n"), None);
    }

    #[test]
    fn загруженный_список_переезжает_только_в_незагруженный() {
        let загруженный = "1.0.0.0/24\r\n1.1.1.0/24\r\n";
        let заглушка = "203.0.113.113/32\r\n";
        let стало = перенести_ipset(загруженный, заглушка).unwrap();
        assert_eq!(ipset_mode_from(&стало), "loaded");
        assert!(стало.contains("1.1.1.0/24"), "{стало:?}");
        // Откат: старый релиз с заглушкой не затирает загруженный список нового.
        assert_eq!(перенести_ipset(заглушка, загруженный), None);
    }

    #[test]
    fn адреса_игр_переезжают_и_объединяются() {
        let riot = gs::Group {
            asn: "6507".into(),
            name: "Riot Games, Inc".into(),
            at: 1,
            nets: vec!["162.249.72.0/21".into()],
            legacy: false,
        };
        let valve = gs::Group {
            asn: "32590".into(),
            name: "Valve Corporation".into(),
            at: 2,
            nets: vec!["146.66.152.0/22".into()],
            legacy: false,
        };
        let старый = gs::merge_groups("203.0.113.113/32", &[riot], &[]);
        let новый = gs::merge_groups("203.0.113.113/32", &[valve], &[]);
        let стало = перенести_ipset(&старый, &новый).unwrap();
        let (groups, _) = gs::parse_groups(&стало);
        let asns: BTreeSet<&str> = groups.iter().map(|g| g.asn.as_str()).collect();
        assert_eq!(asns, ["32590", "6507"].into_iter().collect(), "{стало:?}");
        assert!(!стало.contains(gs::EMPTY_STUB), "рядом с сетями заглушка не нужна: {стало:?}");
        // Повторный перенос ничего не меняет.
        assert_eq!(перенести_ipset(&старый, &стало), None);
    }

    #[test]
    fn перенос_не_делает_список_пустым() {
        // В новом только заглушка, в старом — пусто: «применяться ко всему»
        // из старого переносить нельзя, это худший из режимов для игр.
        assert_eq!(перенести_ipset("", "203.0.113.113/32\r\n"), None);
    }

    #[test]
    fn склонение_групп() {
        assert_eq!(групп(1), "группа");
        assert_eq!(групп(3), "группы");
        assert_eq!(групп(5), "групп");
        assert_eq!(групп(11), "групп");
        assert_eq!(групп(22), "группы");
    }
}
