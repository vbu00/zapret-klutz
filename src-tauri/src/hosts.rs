//! Файл hosts: рекомендованные строки zapret-discord-youtube — своим блоком.
//!
//! Раньше «Обновить hosts» ничего не обновлял: открывал рекомендованный текст
//! в блокноте и просил перенести строки руками, а проводник с самим hosts из
//! Klutz, запущенного от администратора, не открывался вовсе. Теперь Klutz
//! кладёт строки в помеченный блок, прежде сохранив копию файла, и тем же
//! путём их убирает.
//!
//! Чужое не трогаем. Если человек сам прописал адрес для того же имени, его
//! запись остаётся, а наша для этого имени не пишется. Всё вне нашего блока
//! переписывается ровно как было.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;

/// Начало нашего блока. Узнаём его по префиксу, чтобы текст пояснения можно
/// было менять, не теряя старые блоки.
const BEGIN_PREFIX: &str = "# >>> Klutz";
pub const BEGIN: &str = "# >>> Klutz: строки zapret-discord-youtube. Убрать — «Обновить hosts» в Klutz";
pub const END: &str = "# <<< Klutz";
/// Копия файла до первой правки Klutz — рядом с hosts.
pub const BACKUP: &str = "hosts.klutz.bak";

/// Пары «адрес, имя» из текста hosts — по одной на имя, имена в нижнем
/// регистре. Комментарии и строки, где первое слово не адрес, пропускаем.
pub fn entries(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let mut f = line.split_whitespace();
        let Some(ip) = f.next() else { continue };
        if ip.parse::<std::net::IpAddr>().is_err() {
            continue;
        }
        for host in f {
            let h = host.to_lowercase();
            if h.contains('.') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_') {
                out.push((ip.to_string(), h));
            }
        }
    }
    out
}

/// Текст вне нашего блока (с переводами строк Windows) и строки внутри него.
pub fn split_block(text: &str) -> (String, Vec<String>) {
    let mut outside = Vec::new();
    let mut block = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if !inside && line.trim_start().starts_with(BEGIN_PREFIX) {
            inside = true;
            continue;
        }
        if inside {
            if line.trim() == END {
                inside = false;
            } else {
                block.push(line.trim().to_string());
            }
            continue;
        }
        outside.push(line);
    }
    (outside.join("\r\n"), block)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Что должно лежать в блоке.
    pub lines: Vec<String>,
    /// Чего в блоке не хватает.
    pub missing: usize,
    /// Что в блоке лишнее: из рекомендованного убрали.
    pub stale: usize,
    /// Имена, которые человек прописал сам на другой адрес, — их не пишем.
    pub conflicts: usize,
}

pub fn plan(current: &str, recommended: &[(String, String)]) -> Plan {
    let (outside, block) = split_block(current);
    let theirs: HashMap<String, String> = entries(&outside).into_iter().map(|(ip, h)| (h, ip)).collect();
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    let mut conflicts = 0;
    for (ip, host) in recommended {
        if !seen.insert(host.clone()) {
            continue;
        }
        match theirs.get(host) {
            // Уже прописано тем же адресом — дублировать незачем.
            Some(their_ip) if their_ip == ip => {}
            Some(_) => conflicts += 1,
            None => lines.push(format!("{ip} {host}")),
        }
    }
    let have: HashSet<String> =
        entries(&block.join("\n")).into_iter().map(|(ip, h)| format!("{ip} {h}")).collect();
    let want: HashSet<&String> = lines.iter().collect();
    let missing = lines.iter().filter(|l| !have.contains(*l)).count();
    let stale = have.iter().filter(|l| !want.contains(l)).count();
    Plan { lines, missing, stale, conflicts }
}

/// Файл целиком: чужое как было, наш блок в конце. Без строк — без блока.
pub fn render(outside: &str, lines: &[String]) -> String {
    let mut out = outside.trim_end().to_string();
    if lines.is_empty() {
        out.push_str("\r\n");
        return out;
    }
    if !out.is_empty() {
        out.push_str("\r\n\r\n");
    }
    out.push_str(BEGIN);
    out.push_str("\r\n");
    for l in lines {
        out.push_str(l);
        out.push_str("\r\n");
    }
    out.push_str(END);
    out.push_str("\r\n");
    out
}

// ─────────── файл ───────────

#[derive(Debug, Serialize)]
pub struct HostsStatus {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub missing: usize,
    pub stale: usize,
    pub conflicts: usize,
    /// Наш блок в файле уже есть.
    pub applied: bool,
    #[serde(rename = "hasBackup")]
    pub has_backup: bool,
}

#[derive(Debug, Serialize)]
pub struct HostsResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub added: usize,
    pub removed: usize,
}

fn fail(e: String) -> HostsResult {
    HostsResult { ok: false, error: Some(e), added: 0, removed: 0 }
}

fn recommended() -> Result<Vec<(String, String)>, String> {
    let text = crate::maintenance::http_get(&format!("{}/hosts", crate::maintenance::RAW_BASE))?;
    let e = entries(&text);
    if e.is_empty() {
        return Err("рекомендованный список пуст — похоже на ошибку загрузки".into());
    }
    Ok(e)
}

/// Текущий hosts. Не UTF-8 — не трогаем: переписав его «как понял», мы бы
/// испортили чужие комментарии в другой кодировке.
fn read_current() -> Result<String, String> {
    let path = crate::sys::hosts_path();
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("не удалось прочитать hosts: {e}")),
    };
    String::from_utf8(bytes).map_err(|_| "файл hosts не в UTF-8 — Klutz не будет его переписывать".to_string())
}

/// Копия до первой правки Klutz. Уже есть — не перезаписываем: в ней должен
/// остаться файл, каким он был до нас, а не после прошлой правки.
fn backup(current: &str) -> Result<(), String> {
    let path = crate::sys::hosts_path().with_file_name(BACKUP);
    if path.exists() {
        return Ok(());
    }
    fs::write(&path, current).map_err(|e| format!("не удалось сохранить копию hosts: {e}"))
}

fn write(text: &str) -> Result<(), String> {
    fs::write(crate::sys::hosts_path(), text).map_err(|e| {
        format!(
            "не удалось записать hosts: {e}. Его может защищать антивирус — разреши Klutz изменить файл \
             или добавь строки вручную"
        )
    })?;
    // Иначе Windows ещё какое-то время отвечает из кэша по-старому.
    crate::sys::run("ipconfig", &["/flushdns"]);
    Ok(())
}

/// Лежит ли наш блок в hosts прямо сейчас.
pub fn applied() -> bool {
    read_current().is_ok_and(|c| c.lines().any(|l| l.trim_start().starts_with(BEGIN_PREFIX)))
}

pub fn status() -> HostsStatus {
    let has_backup = crate::sys::hosts_path().with_file_name(BACKUP).exists();
    let broken = |e: String| HostsStatus {
        ok: false,
        error: Some(e),
        missing: 0,
        stale: 0,
        conflicts: 0,
        applied: false,
        has_backup,
    };
    let current = match read_current() {
        Ok(c) => c,
        Err(e) => return broken(e),
    };
    let rec = match recommended() {
        Ok(r) => r,
        Err(e) => return broken(e),
    };
    let p = plan(&current, &rec);
    let applied = current.lines().any(|l| l.trim_start().starts_with(BEGIN_PREFIX));
    HostsStatus { ok: true, error: None, missing: p.missing, stale: p.stale, conflicts: p.conflicts, applied, has_backup }
}

pub fn apply() -> HostsResult {
    let current = match read_current() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let rec = match recommended() {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let p = plan(&current, &rec);
    if p.missing == 0 && p.stale == 0 {
        return HostsResult { ok: true, error: None, added: 0, removed: 0 };
    }
    if let Err(e) = backup(&current) {
        return fail(e);
    }
    let (outside, _) = split_block(&current);
    if let Err(e) = write(&render(&outside, &p.lines)) {
        return fail(e);
    }
    HostsResult { ok: true, error: None, added: p.missing, removed: p.stale }
}

/// Убирает наш блок. Всё остальное в файле — как было.
pub fn remove() -> HostsResult {
    let current = match read_current() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let (outside, block) = split_block(&current);
    let removed = entries(&block.join("\n")).len();
    if block.is_empty() && !current.lines().any(|l| l.trim_start().starts_with(BEGIN_PREFIX)) {
        return HostsResult { ok: true, error: None, added: 0, removed: 0 };
    }
    if let Err(e) = backup(&current) {
        return fail(e);
    }
    if let Err(e) = write(&render(&outside, &[])) {
        return fail(e);
    }
    HostsResult { ok: true, error: None, added: 0, removed }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn rec(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn строки_hosts_разбираются() {
        let text = "# комментарий\r\n127.0.0.1 localhost\r\n\r\n104.25.158.178 Finland1.discord.media finland2.discord.media # хвост\r\nмусор строка\r\n::1 localhost.v6\r\n";
        assert_eq!(
            entries(text),
            vec![
                ("104.25.158.178".to_string(), "finland1.discord.media".to_string()),
                ("104.25.158.178".to_string(), "finland2.discord.media".to_string()),
                ("::1".to_string(), "localhost.v6".to_string()),
            ]
        );
    }

    #[test]
    fn чужие_строки_не_трогаются() {
        let current = "127.0.0.1 my.site\r\n1.1.1.1 a.discord.media\r\n";
        let r = rec(&[("2.2.2.2", "a.discord.media"), ("3.3.3.3", "b.discord.media"), ("127.0.0.1", "my.site")]);
        let p = plan(current, &r);
        // a — человек прописал сам на другой адрес, my.site — ровно так же.
        assert_eq!(p.conflicts, 1);
        assert_eq!(p.lines, vec!["3.3.3.3 b.discord.media"]);
        assert_eq!(p.missing, 1);

        let (outside, _) = split_block(current);
        let written = render(&outside, &p.lines);
        assert!(written.starts_with("127.0.0.1 my.site\r\n1.1.1.1 a.discord.media\r\n\r\n# >>> Klutz"), "{written}");
        assert!(written.ends_with("3.3.3.3 b.discord.media\r\n# <<< Klutz\r\n"), "{written}");
    }

    #[test]
    fn повторное_применение_ничего_не_меняет() {
        let r = rec(&[("3.3.3.3", "b.discord.media"), ("4.4.4.4", "c.discord.media")]);
        let first = render("127.0.0.1 my.site", &plan("127.0.0.1 my.site", &r).lines);
        let p = plan(&first, &r);
        assert_eq!((p.missing, p.stale), (0, 0));
        let (outside, _) = split_block(&first);
        assert_eq!(render(&outside, &p.lines), first);
    }

    #[test]
    fn из_рекомендованного_убрали_значит_лишнее() {
        let r = rec(&[("3.3.3.3", "b.discord.media"), ("4.4.4.4", "c.discord.media")]);
        let applied = render("", &plan("", &r).lines);
        let p = plan(&applied, &rec(&[("3.3.3.3", "b.discord.media")]));
        assert_eq!((p.missing, p.stale), (0, 1));
    }

    #[test]
    fn убрать_блок_оставляет_остальное() {
        let r = rec(&[("3.3.3.3", "b.discord.media")]);
        let applied = render("127.0.0.1 my.site\r\n# свой комментарий", &plan("", &r).lines);
        let (outside, block) = split_block(&applied);
        assert_eq!(block, vec!["3.3.3.3 b.discord.media"]);
        assert_eq!(render(&outside, &[]), "127.0.0.1 my.site\r\n# свой комментарий\r\n");
    }
}
