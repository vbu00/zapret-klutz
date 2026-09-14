//! Дополнительные стратегии: варианты шипованного конфига с другими
//! позициями разреза.
//!
//! Зачем это вообще. Релиз Flowseal привозит два десятка конфигов, и когда
//! ни один не пробивает, подбирать больше не из чего. Соседний проект
//! [necronicle/z2k](https://github.com/necronicle/z2k) (MIT) гоняет на
//! роутерах свои пулы, и часть их параметров у Flowseal не встречается.
//!
//! Что взято и что НЕ взято. Движки разные: у z2k это `nfqws2` с флагами
//! `--lua-desync=...`, у нас `winws.exe` первого поколения с
//! `--dpi-desync-...`. Стратегии как единое целое не переносятся: самый
//! частый приём z2k (`tls_client_hello_clone`, 71 директива из 254) во
//! флагах winws не выражается вовсе. Зато НАБОРЫ ПОЗИЦИЙ РАЗРЕЗА — обычные
//! значения `--dpi-desync-split-pos=`, и они переносятся один в один.
//!
//! Сверено с zapret-discord-youtube 1.10.2: там встречаются только `1`,
//! `1,midsld`, `2` и `2,sniext+1`. Всё, что ниже, — из боевых пулов z2k и
//! у Flowseal отсутствует.
//!
//! Файлы генерируются ИЗ РЕЛИЗА ПОЛЬЗОВАТЕЛЯ и остаются в его папке. Мы
//! ничего не перераспространяем: у релиза Flowseal нет лицензии, которая
//! это позволяла бы.
//!
//! ВАЖНО: проверить, что из этого действительно пробивает, можно только
//! прогоном на живой сети. Здесь гарантируется лишь синтаксическая форма —
//! меняется одно значение в остальном нетронутого рабочего конфига.

use once_cell::sync::Lazy;
use regex::Regex;
use std::fs;
use std::path::Path;

/// Префикс имени сгенерированных файлов. По нему же их и удаляем, так что
/// конфиги Flowseal задеть невозможно.
pub const MARK: &str = "general (Z2K ";

/// (суффикс имени, значение --dpi-desync-split-pos).
const EXTRA_SPLIT_POS: &[(&str, &str)] = &[
    ("midsld10", "10,midsld"),
    ("sld1", "sld+1"),
    ("7sld1", "7,sld+1"),
    ("2sld", "2,sld"),
    ("sniext1", "1,sniext+1"),
    ("endsld", "1,sld+1,endsld-2"),
    ("multi7", "2,5,105,host+5,sld-1,endsld-5,endsld"),
    ("multi8", "1,sniext+1,host+1,midsld-2,midsld,midsld+2,endhost-1"),
];

static SPLIT_POS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"--dpi-desync-split-pos=[^\s\^]+").unwrap());

static FOOLING: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"--dpi-desync-fooling=([^\s\^]+)").unwrap());

/// Какие приёмы обмана встречаются в ЭТОМ релизе, кроме уже стоящего в
/// образце.
///
/// Почему не свой список. Позиция разреза — это число, и любое число
/// синтаксически годится. А `fooling` — перечисление, и неподдерживаемое
/// значение винвс просто не съест: вариант не запустится, а человек решит,
/// что дело в стратегии. Берём только то, что автор релиза уже где-то
/// применил: раз оно лежит в рабочем конфиге, этот бинарник его понимает.
fn fooling_values(root: &Path, template_value: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in crate::release::list_configs(root) {
        if is_variant(&name) {
            continue;
        }
        let Ok(text) = fs::read_to_string(root.join(&name)) else { continue };
        for line in text.lines() {
            // Комментарии и echo — не источник значений. Там лежит текст, а
            // не то, что винвс когда-либо исполнял: взяв значение оттуда, мы
            // бы сами перенесли его В рабочую строку.
            let head = line.trim_start().to_lowercase();
            if head.starts_with("rem ") || head.starts_with("::") || head.starts_with("echo ") {
                continue;
            }
            for c in FOOLING.captures_iter(line) {
                let v = c[1].to_string();
                // Значение уезжает в .bat, который дальше исполняет cmd.
                // Регулярка запрещает пробел и «^», но «&», «%» и скобки
                // пропускала — а этого хватает, чтобы дописать команду.
                if !v.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == ',' || ch == '-' || ch == '_') {
                    continue;
                }
                if Some(v.as_str()) != template_value && !out.contains(&v) {
                    out.push(v);
                }
            }
        }
    }
    out.sort();
    out
}

/// Замена только в строках, которые bat действительно исполняет.
///
/// Регулярное выражение не различает команду и текст: `rem` с примером
/// флага или `echo` с подсказкой правились наравне с рабочей строкой. Это
/// не ломало конфиг, но делало его комментарии враньём — а читает их
/// человек, который потом по ним и настраивает.
fn replace_outside_comments(text: &str, re: &Regex, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let head = line.trim_start().to_lowercase();
        if head.starts_with("rem ") || head.starts_with("::") || head.starts_with("echo ") {
            out.push_str(line);
        } else {
            out.push_str(&re.replace_all(line, to));
        }
    }
    out
}

/// Имя варианта несёт и шаблон, и суффикс. Без шаблона два поколения из
/// разных конфигов давали одни и те же имена, и второе молча затирало
/// первое — при том что содержимое у них разное.
fn variant_name(template: &str, suffix: &str) -> String {
    let base = template.trim_end_matches(".bat").trim_end_matches(".BAT");
    format!("{MARK}{base} {suffix}).bat")
}

/// Префикс прежнего варианта конфига, взятого из предыдущего релиза для
/// проверки (configdiff). Живёт и убирается как дополнительная стратегия.
pub const OLD_MARK: &str = "general (Было ";

pub fn is_variant(name: &str) -> bool {
    (name.starts_with(MARK) || name.starts_with(OLD_MARK)) && name.to_lowercase().ends_with(".bat")
}

/// Из какого конфига сделан вариант: `general (Z2K general (ALT11) sld1).bat`
/// → `general (ALT11).bat`. Нужно, чтобы при смене релиза пересоздать
/// варианты от того же образца, а не копировать файлы со старым конфигом.
///
/// Суффикс — последнее слово; у вариантов приёма обмана перед ним ещё «обман».
pub fn template_of(variant: &str) -> Option<String> {
    let inner = variant.strip_prefix(MARK)?.strip_suffix(").bat")?;
    let (base, _suffix) = inner.rsplit_once(' ')?;
    let base = base.strip_suffix(" обман").unwrap_or(base);
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}.bat"))
}

/// Сколько вариантов уже лежит в папке релиза.
pub fn count(root: &Path) -> usize {
    crate::release::list_configs(root).iter().filter(|c| is_variant(c)).count()
}

/// Какой конфиг брать за образец, если пользователь не выбрал сам: активный,
/// иначе первый не-вариант из списка. Вариант образцом не берём — иначе
/// получится вариант варианта.
pub fn default_template(root: &Path, active: Option<&str>) -> Option<String> {
    let configs = crate::release::list_configs(root);
    if let Some(a) = active {
        if !is_variant(a) && configs.iter().any(|c| c == a) {
            return Some(a.to_string());
        }
    }
    configs.into_iter().find(|c| !is_variant(c))
}

/// Создаёт варианты образца с другими позициями разреза.
///
/// Заменяются только УЖЕ ЕСТЬ в конфиге вхождения `--dpi-desync-split-pos=`.
/// Новых не добавляем: позиция разреза осмысленна лишь для split-приёмов, и
/// приписывать её профилю, который работает одним `fake`, — значит сочинять
/// за автора конфига.
pub fn generate(root: &Path, template: &str) -> Result<Vec<String>, String> {
    if is_variant(template) {
        return Err("Образцом нужен конфиг из релиза, а не другой вариант.".into());
    }
    if !crate::release::list_configs(root).iter().any(|c| c == template) {
        return Err(format!("Нет такого конфига в релизе: {template}"));
    }
    let text = fs::read_to_string(root.join(template)).map_err(|e| e.to_string())?;
    if !SPLIT_POS.is_match(&text) {
        return Err(format!(
            "В «{template}» нет ни одной позиции разреза — менять нечего. \
             Возьми образцом конфиг со split или multisplit."
        ));
    }

    let mut made = Vec::new();
    // Пишем через замыкание, чтобы на любой ошибке убрать уже созданное:
    // иначе на полпути (нет места, отняли права) в папке релиза оставалась
    // половина набора, и пользователь получал случайную выборку вариантов.
    let write = |name: String, body: String, made: &mut Vec<String>| -> Result<(), String> {
        match fs::write(root.join(&name), body) {
            Ok(()) => {
                made.push(name);
                Ok(())
            }
            Err(e) => Err(format!("{name}: {e}")),
        }
    };
    let rollback = |made: &[String]| {
        for n in made {
            let _ = fs::remove_file(root.join(n));
        }
    };

    for (suffix, pos) in EXTRA_SPLIT_POS {
        let body = replace_outside_comments(&text, &SPLIT_POS, &format!("--dpi-desync-split-pos={pos}"));
        let name = variant_name(template, suffix);
        if let Err(e) = write(name, body, &mut made) {
            rollback(&made);
            return Err(e);
        }
    }

    // Вторая ось. Позиция разреза отвечает на вопрос «где резать», приём
    // обмана — на другой: чем именно морочить коробку. Один и тот же разрез
    // с `badsum` и с `md5sig` живёт по-разному, потому что часть коробок
    // сверяет контрольную сумму, а часть нет.
    let own = FOOLING.captures(&text).map(|c| c[1].to_string());
    for value in fooling_values(root, own.as_deref()) {
        let body = replace_outside_comments(&text, &FOOLING, &format!("--dpi-desync-fooling={value}"));
        let name = variant_name(template, &format!("обман {value}"));
        if let Err(e) = write(name, body, &mut made) {
            rollback(&made);
            return Err(e);
        }
    }
    Ok(made)
}

/// Удаляет всё, что сгенерировали. Чужие конфиги не трогает: фильтр по
/// нашему же префиксу.
pub fn remove_all(root: &Path) -> Result<usize, String> {
    let mut n = 0;
    for name in crate::release::list_configs(root) {
        if !is_variant(&name) {
            continue;
        }
        fs::remove_file(root.join(&name)).map_err(|e| format!("{name}: {e}"))?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Образец, из которого генерируем во всех тестах.
    const TPL: &str = "general.bat";

    fn релиз() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "klutz-strat-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin").join("winws.exe"), "").unwrap();
        dir
    }

    fn образец() -> String {
        [
            "@echo off",
            "start \"zapret\" /min \"%BIN%winws.exe\" --wf-tcp=80,443 ^",
            "--filter-tcp=443 --dpi-desync=fake --dpi-desync-repeats=6 --new ^",
            "--filter-tcp=80 --dpi-desync=fake,multisplit --dpi-desync-split-pos=1,midsld --dpi-desync-fooling=ts ^",
            "--filter-udp=443 --dpi-desync=multisplit --dpi-desync-split-pos=2 --dpi-desync-repeats=6",
        ]
        .join("\r\n")
    }

    #[test]
    fn комментарии_и_echo_не_правятся() {
        let dir = релиз();
        let текст = [
            "@echo off",
            "rem пример: --dpi-desync-split-pos=1,midsld",
            ":: и так тоже пишут --dpi-desync-split-pos=2",
            "echo Текущая точка разреза: --dpi-desync-split-pos=1,midsld",
            "start \"zapret\" /min \"%BIN%winws.exe\" --wf-tcp=443 ^",
            "--filter-tcp=443 --dpi-desync=fake,multisplit --dpi-desync-split-pos=1,midsld --dpi-desync-fooling=ts",
        ]
        .join("\r\n");
        fs::write(dir.join("general.bat"), текст).unwrap();

        generate(&dir, "general.bat").unwrap();
        let v = fs::read_to_string(dir.join(variant_name(TPL, "sld1"))).unwrap();

        // Рабочая строка заменена...
        assert!(v.contains("--dpi-desync=fake,multisplit --dpi-desync-split-pos=sld+1"), "{v}");
        // ...а пояснения для человека остались прежними.
        assert!(v.contains("rem пример: --dpi-desync-split-pos=1,midsld"), "{v}");
        assert!(v.contains(":: и так тоже пишут --dpi-desync-split-pos=2"), "{v}");
        assert!(v.contains("echo Текущая точка разреза: --dpi-desync-split-pos=1,midsld"), "{v}");
    }

    #[test]
    fn ошибка_записи_не_оставляет_половину_набора() {
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        // Занимаем именем одного из вариантов КАТАЛОГ: запись в него не
        // пройдёт, а часть файлов к тому моменту уже создана.
        let занято = dir.join(variant_name(TPL, "multi8"));
        fs::create_dir_all(&занято).unwrap();

        assert!(generate(&dir, "general.bat").is_err());
        // Ни одного варианта-ФАЙЛА остаться не должно. Каталог-заглушку,
        // которым мы и сломали запись, считать не надо: его создал тест.
        let файлов = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter(|e| is_variant(&e.file_name().to_string_lossy()))
            .count();
        assert_eq!(файлов, 0, "остались обломки набора");
    }

    #[test]
    fn приёмы_обмана_берутся_только_из_релиза() {
        let dir = релиз();
        // В образце стоит ts. Рядом в релизе автор применил md5sig и badsum —
        // значит этот winws их понимает, и подставлять их безопасно.
        fs::write(dir.join("general.bat"), образец()).unwrap();
        fs::write(dir.join("ALT.bat"), образец().replace("fooling=ts", "fooling=md5sig")).unwrap();
        fs::write(dir.join("ALT2.bat"), образец().replace("fooling=ts", "fooling=badsum")).unwrap();

        let made = generate(&dir, "general.bat").unwrap();
        assert_eq!(made.len(), EXTRA_SPLIT_POS.len() + 2, "две оси: позиции и обман");

        let v = fs::read_to_string(dir.join(variant_name(TPL, "обман badsum"))).unwrap();
        assert!(v.contains("--dpi-desync-fooling=badsum"), "{v}");
        assert!(!v.contains("--dpi-desync-fooling=ts"), "прежнее значение осталось");
        // Позиция разреза при этом не тронута: оси меняем по одной.
        assert!(v.contains("--dpi-desync-split-pos=1,midsld"), "{v}");
    }

    #[test]
    fn значение_обмана_из_комментария_не_уезжает_в_рабочую_строку() {
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        // Чужой архив: в комментарии соседнего конфига лежит значение с
        // «&». Взяв его, мы бы сами перенесли команду В исполняемую строку.
        let злой = [
            "@echo off",
            "rem --dpi-desync-fooling=x&calc",
            "echo подсказка: --dpi-desync-fooling=y|whoami",
            "start \"z\" /min \"%BIN%winws.exe\" --filter-tcp=443 --dpi-desync=fake --dpi-desync-fooling=md5sig",
        ]
        .join("\r\n");
        fs::write(dir.join("ALT.bat"), злой).unwrap();

        let made = generate(&dir, "general.bat").unwrap();
        let обманы: Vec<&String> = made.iter().filter(|n| n.contains("обман")).collect();
        // Из рабочей строки значение взяли, из комментариев — нет.
        assert_eq!(обманы.len(), 1, "{обманы:?}");
        assert!(обманы[0].contains("md5sig"), "{обманы:?}");
        for name in &made {
            let v = fs::read_to_string(dir.join(name)).unwrap();
            assert!(!v.contains("fooling=x&calc"), "{name}: утекло из комментария");
            assert!(!v.contains("fooling=y|whoami"), "{name}: утекло из echo");
        }
    }

    #[test]
    fn выдуманных_приёмов_обмана_не_появляется() {
        // Единственный конфиг, единственное значение — подставлять нечего.
        // Свой список значений мы не держим намеренно: неподдерживаемое
        // winws не съест, и вариант просто не запустится.
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        let made = generate(&dir, "general.bat").unwrap();
        assert_eq!(made.len(), EXTRA_SPLIT_POS.len());
        assert!(!made.iter().any(|n| n.contains("обман")), "{made:?}");
    }

    #[test]
    fn варианты_из_разных_образцов_не_затирают_друг_друга() {
        // Раньше имя не зависело от образца: сгенерировали из ALT после
        // general — и файлы с теми же именами молча подменились, хотя
        // содержимое у них разное.
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        fs::write(dir.join("ALT.bat"), образец().replace("fake", "fakedsplit")).unwrap();

        let a = generate(&dir, "general.bat").unwrap();
        let b = generate(&dir, "ALT.bat").unwrap();
        assert_eq!(count(&dir), a.len() + b.len(), "файлы подменили друг друга");
        for name in &a {
            assert!(!b.contains(name), "{name} совпало у двух образцов");
        }
        // И удаление по-прежнему забирает всё своё разом.
        assert_eq!(remove_all(&dir).unwrap(), a.len() + b.len());
    }

    #[test]
    fn создаёт_вариант_на_каждый_набор_позиций() {
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();

        let made = generate(&dir, "general.bat").unwrap();
        assert_eq!(made.len(), EXTRA_SPLIT_POS.len());
        assert_eq!(count(&dir), EXTRA_SPLIT_POS.len());

        let v = fs::read_to_string(dir.join(variant_name(TPL, "sld1"))).unwrap();
        assert!(v.contains("--dpi-desync-split-pos=sld+1"), "{v}");
        // Заменены ОБА вхождения, а не первое.
        assert_eq!(v.matches("--dpi-desync-split-pos=sld+1").count(), 2, "{v}");
        assert!(!v.contains("split-pos=1,midsld"), "старое значение осталось");
        // Всё остальное — нетронутый конфиг.
        assert!(v.contains("--dpi-desync-fooling=ts") && v.contains("--new"), "{v}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn новых_позиций_не_приписывает() {
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        generate(&dir, "general.bat").unwrap();

        let v = fs::read_to_string(dir.join(variant_name(TPL, "2sld"))).unwrap();
        // В образце два split-pos — ровно столько же должно остаться.
        assert_eq!(v.matches("--dpi-desync-split-pos=").count(), 2, "{v}");
        // Профиль на одном fake позиции разреза не получил.
        let fake_line = v.lines().find(|l| l.contains("--dpi-desync=fake ")).unwrap();
        assert!(!fake_line.contains("split-pos"), "{fake_line}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn без_позиций_в_образце_понятная_ошибка() {
        let dir = релиз();
        fs::write(
            dir.join("general.bat"),
            "start \"z\" \"%BIN%winws.exe\" --filter-tcp=443 --dpi-desync=fake",
        )
        .unwrap();
        let e = generate(&dir, "general.bat").unwrap_err();
        assert!(e.contains("нет ни одной позиции разреза"), "{e}");
        assert_eq!(count(&dir), 0, "при ошибке файлы создаваться не должны");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn вариант_нельзя_взять_образцом() {
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        generate(&dir, "general.bat").unwrap();
        let e = generate(&dir, &variant_name(TPL, "sld1")).unwrap_err();
        assert!(e.contains("не другой вариант"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn удаляет_только_своё() {
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        fs::write(dir.join("general (ALT).bat"), образец()).unwrap();
        generate(&dir, "general.bat").unwrap();

        let n = remove_all(&dir).unwrap();
        assert_eq!(n, EXTRA_SPLIT_POS.len());
        assert!(dir.join("general.bat").exists(), "чужой конфиг удалять нельзя");
        assert!(dir.join("general (ALT).bat").exists(), "чужой конфиг удалять нельзя");
        assert_eq!(count(&dir), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn образец_по_умолчанию_не_вариант() {
        let dir = релиз();
        fs::write(dir.join("general.bat"), образец()).unwrap();
        generate(&dir, "general.bat").unwrap();

        // Активен вариант — образцом всё равно берём настоящий конфиг.
        let t = default_template(&dir, Some(&variant_name(TPL, "sld1"))).unwrap();
        assert!(!is_variant(&t), "{t}");
        // Активен настоящий — берём именно его.
        assert_eq!(default_template(&dir, Some("general.bat")).as_deref(), Some("general.bat"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn имена_вариантов_узнаются() {
        assert!(is_variant("general (Z2K sld1).bat"));
        assert!(!is_variant("general (ALT).bat"));
        assert!(!is_variant("general.bat"));
        assert!(!is_variant("general (Z2K sld1).txt"));
        // Образец по имени варианта — для пересоздания в новом релизе.
        assert_eq!(template_of(&variant_name("general (ALT11).bat", "sld1")).as_deref(), Some("general (ALT11).bat"));
        assert_eq!(
            template_of("general (Z2K general (ALT11) обман badseq).bat").as_deref(),
            Some("general (ALT11).bat")
        );
        assert_eq!(template_of("general (ALT11).bat"), None);
    }
}
