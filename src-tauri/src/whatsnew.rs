//! «Что нового»: разделы CHANGELOG, вшитого в сборку.
//!
//! Самообновления у Klutz нет — новая версия ставится поверх, — и о том, что
//! изменилось, человек узнавал разве что случайно. При первом открытии после
//! обновления окно показывает разделы новее той версии, что стояла раньше.

use serde::Serialize;

const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// Раздел для сборки, которой ещё нет в CHANGELOG под своим номером, — бета.
pub const UNRELEASED: &str = "Не выпущено";

/// Сколько разделов показываем разом: после долгого перерыва их может
/// накопиться десяток, и окно превратилось бы в простыню.
pub const MAX_SECTIONS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    /// Первая фраза пункта — её видно сразу.
    pub title: String,
    /// Пункт целиком — раскрывается по нажатию.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    pub version: String,
    pub items: Vec<Item>,
}

/// Первая фраза: до точки, за которой пробел и заглавная буква или «.
/// Точки в номере версии («1.10.2») идут без пробела и фразу не обрывают,
/// а «…до 12. Второе» — обрывает.
fn первая_фраза(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();
    for i in 0..chars.len().saturating_sub(2) {
        let дальше = chars[i + 2];
        if chars[i] == '.' && chars[i + 1] == ' ' && (дальше.is_uppercase() || дальше == '«') {
            end = i + 1;
            break;
        }
    }
    let фраза: String = chars[..end].iter().collect();
    if фраза.chars().count() > 160 {
        let short: String = фраза.chars().take(157).collect();
        format!("{}…", short.trim_end())
    } else {
        фраза
    }
}

fn чисто(text: &str) -> String {
    text.replace('`', "")
}

fn закрыть(sections: &mut [Section], item: &mut Option<String>) {
    if let (Some(s), Some(text)) = (sections.last_mut(), item.take()) {
        let text = чисто(&text);
        s.items.push(Item { title: первая_фраза(&text), text });
    }
}

/// Разделы CHANGELOG в порядке файла — свежие сверху.
pub fn parse(md: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut cur: Option<String> = None;
    for line in md.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            закрыть(&mut out, &mut cur);
            let version = h.split(" — ").next().unwrap_or(h).trim().to_string();
            out.push(Section { version, items: Vec::new() });
            continue;
        }
        if out.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            закрыть(&mut out, &mut cur);
            cur = Some(rest.trim().to_string());
            continue;
        }
        match cur.as_mut() {
            // Продолжение пункта — строки с отступом.
            Some(text) if line.starts_with("  ") && !line.trim().is_empty() => {
                text.push(' ');
                text.push_str(line.trim());
            }
            // Пустая строка, подзаголовок или абзац вне списка пункт закрывают.
            Some(_) => закрыть(&mut out, &mut cur),
            None => {}
        }
    }
    закрыть(&mut out, &mut cur);
    out
}

/// Разделы новее `since` начиная с текущей версии. Без `since` — только
/// раздел текущей: откуда обновились, неизвестно.
pub fn select(sections: &[Section], current: &str, since: Option<&str>) -> Vec<Section> {
    let Some(start) = sections
        .iter()
        .position(|s| s.version == current)
        .or_else(|| sections.iter().position(|s| s.version == UNRELEASED))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in &sections[start..] {
        if Some(s.version.as_str()) == since {
            break;
        }
        if s.items.is_empty() {
            continue;
        }
        out.push(s.clone());
        if since.is_none() || out.len() == MAX_SECTIONS {
            break;
        }
    }
    out
}

pub fn whats_new(current: &str, since: Option<&str>) -> Vec<Section> {
    select(&parse(CHANGELOG), current, since)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    const MD: &str = "# Изменения Klutz

Вступление, которое к разделам не относится.

## Не выпущено

- Сбор адресов игры не находил ничего. Кнопок было две,
  и обе упирались в то, что должен был сделать человек.
- На 1.10.2 ALT11 упал с 36 до 12. Второе предложение.

## 1.4.0-beta.1 — 2026-09-13

Бета: абзац вне списка.

- Проверка обновлений понимает бету: `1.4.0-beta.1` новее 1.3.0.

## 1.3.0 — 2026-09-12

- Обход не запускался на чистой установке.
";

    #[test]
    fn разделы_и_пункты() {
        let s = parse(MD);
        let versions: Vec<&str> = s.iter().map(|x| x.version.as_str()).collect();
        assert_eq!(versions, vec!["Не выпущено", "1.4.0-beta.1", "1.3.0"]);
        assert_eq!(s[0].items.len(), 2);
        assert_eq!(s[0].items[0].title, "Сбор адресов игры не находил ничего.");
        assert!(s[0].items[0].text.ends_with("должен был сделать человек."), "{}", s[0].items[0].text);
        // Точка в номере версии фразу не обрывает.
        assert_eq!(s[0].items[1].title, "На 1.10.2 ALT11 упал с 36 до 12.");
        // Абзац вне списка пунктом не становится, обратные кавычки убраны.
        assert_eq!(s[1].items.len(), 1);
        assert!(s[1].items[0].text.contains("1.4.0-beta.1 новее"), "{}", s[1].items[0].text);
    }

    #[test]
    fn что_показывать() {
        let s = parse(MD);
        let v = |got: Vec<Section>| got.into_iter().map(|x| x.version).collect::<Vec<_>>();
        // Бета, которой нет в CHANGELOG под номером, — «Не выпущено», до прежней версии.
        assert_eq!(v(select(&s, "1.4.0-beta.2", Some("1.4.0-beta.1"))), vec!["Не выпущено"]);
        // После долгого перерыва — несколько разделов подряд.
        assert_eq!(v(select(&s, "1.4.0-beta.2", Some("1.2.0"))), vec!["Не выпущено", "1.4.0-beta.1", "1.3.0"]);
        // Выпущенная версия — с её собственного раздела.
        assert_eq!(v(select(&s, "1.4.0-beta.1", Some("1.3.0"))), vec!["1.4.0-beta.1"]);
        // Откуда обновились, неизвестно — только текущий раздел.
        assert_eq!(v(select(&s, "1.3.0", None)), vec!["1.3.0"]);
        // Та же версия — показывать нечего.
        assert!(select(&s, "1.3.0", Some("1.3.0")).is_empty());
    }

    #[test]
    fn вшитый_changelog_разбирается() {
        let s = parse(CHANGELOG);
        assert!(s.len() > 3, "в CHANGELOG должны найтись разделы");
        assert!(s.iter().all(|x| x.items.iter().all(|i| !i.title.is_empty())));
    }
}
