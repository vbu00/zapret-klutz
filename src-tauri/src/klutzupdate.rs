//! Новая версия Klutz: что в ней и где установщик.
//!
//! Раньше Klutz только сообщал, что вышла версия, и отправлял на страницу
//! релиза — дальше человек сам искал нужный файл, качал и запускал. Теперь
//! окно показывает, что изменилось, и ставит обновление в одно нажатие.

use serde::Serialize;

const REPO: &str = "vbu00/zapret-klutz";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KlutzRelease {
    pub version: String,
    /// Описание релиза без разметки.
    pub notes: String,
    /// Страница релиза — запасной путь, если скачать не выйдет.
    pub url: String,
    /// Установщик. `None` — в релизе его нет.
    pub asset: Option<String>,
    pub size: u64,
    /// SHA-256 установщика из ответа GitHub. Нет — сверяем только размер.
    pub sha256: Option<String>,
}

/// Установщик из списка файлов релиза: `Klutz_X_x64-setup.exe` — адрес,
/// размер и контрольная сумма, если GitHub её дал.
pub fn pick_asset(release: &serde_json::Value) -> Option<(String, u64, Option<String>)> {
    release.get("assets")?.as_array()?.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?.to_lowercase();
        if !name.ends_with("_x64-setup.exe") {
            return None;
        }
        let url = a.get("browser_download_url")?.as_str()?.to_string();
        Some((url, a.get("size").and_then(|s| s.as_u64()).unwrap_or(0), crate::releases::parse_digest(a)))
    })
}

/// Описание релиза без markdown-разметки: окно показывает его простым текстом.
pub fn clean_notes(body: &str) -> String {
    body.lines()
        .map(|l| {
            let l = l.trim_end();
            let l = l.trim_start_matches('#').trim_start_matches('>');
            l.replace("**", "").replace("__", "").replace('`', "")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub fn parse(json: &str) -> Result<KlutzRelease, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "GitHub ответил неожиданным форматом.".to_string())?;
    let version = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .map(|t| t.trim_start_matches('v').to_string())
        .ok_or("В ответе GitHub нет версии.")?;
    let (asset, size, sha256) = match pick_asset(&v) {
        Some((url, size, sha256)) => (Some(url), size, sha256),
        None => (None, 0, None),
    };
    Ok(KlutzRelease {
        version,
        notes: clean_notes(v.get("body").and_then(|b| b.as_str()).unwrap_or("")),
        url: v.get("html_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        asset,
        size,
        sha256,
    })
}

pub fn latest() -> Result<KlutzRelease, String> {
    parse(&crate::maintenance::http_get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn релиз_с_установщиком() {
        // r###: в описании есть кавычка и «##» подряд — это закрыло бы и
        // r#"…"#, и r##"…"## раньше времени.
        let json = r###"{
            "tag_name": "v1.4.0",
            "html_url": "https://github.com/vbu00/zapret-klutz/releases/tag/v1.4.0",
            "body": "## Главное\n\n**Сбор адресов игры.** Одна кнопка.\n- `stun.bin` → `stun2.bin`",
            "assets": [
                {"name": "latest.json", "browser_download_url": "https://github.com/x/latest.json", "size": 10},
                {"name": "Klutz_1.4.0_x64-setup.exe", "browser_download_url": "https://github.com/vbu00/zapret-klutz/releases/download/v1.4.0/Klutz_1.4.0_x64-setup.exe", "size": 12467453, "digest": "sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"}
            ]
        }"###;
        let r = parse(json).unwrap();
        assert_eq!(r.version, "1.4.0");
        assert_eq!(r.size, 12_467_453);
        // Сумма из поля digest — в нижнем регистре, без префикса.
        assert_eq!(
            r.sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert!(r.asset.as_deref().unwrap().ends_with("Klutz_1.4.0_x64-setup.exe"));
        assert_eq!(r.notes, " Главное\n\nСбор адресов игры. Одна кнопка.\n- stun.bin → stun2.bin".trim());
    }

    #[test]
    fn релиз_без_установщика_и_мусор() {
        let r = parse(r#"{"tag_name":"v1.3.0","assets":[{"name":"src.zip"}]}"#).unwrap();
        assert_eq!(r.asset, None);
        assert_eq!(r.sha256, None);
        // Кривой digest — не сумма: без неё сверяем только размер.
        let bad = serde_json::json!({"digest": "sha256:xyz"});
        assert_eq!(crate::releases::parse_digest(&bad), None);
        let md5 = serde_json::json!({"digest": "md5:0123456789abcdef0123456789abcdef"});
        assert_eq!(crate::releases::parse_digest(&md5), None);
        assert!(parse("не json").is_err());
        assert!(parse("{}").is_err());
    }
}
