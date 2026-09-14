//! «Discord без QUIC»: правило брандмауэра Windows, которое не пускает
//! приложение Discord на UDP 443.
//!
//! Зачем. discord.com и cdn.discordapp.com объявляют HTTP/3
//! (`alt-svc: h3=":443"`), и встроенный в Discord Chromium после первого
//! захода ходит к ним по QUIC. Тесты zapret проверяют через curl, а он QUIC не
//! умеет, — стратегия бывает зелёной в тестах, а Discord висит на
//! «Starting...»: рукопожатие QUIC проходит, а данные нет, и Chromium долго
//! ждёт, прежде чем сдаться. Запрещённый UDP 443 он бросает сразу и уходит на
//! TCP, который обход держит надёжнее.
//!
//! Правило привязано к исполняемому файлу, а у Discord он лежит в папке с
//! версией (`app-1.0.9257`) и после каждого обновления переезжает. Поэтому
//! при запуске Klutz сверяет правило с установленными версиями.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Имя правила. Латиницей: netsh печатает его в консольной кодировке, и
/// кириллица там превращалась бы в знаки вопроса.
pub const RULE_NAME: &str = "Klutz - Discord without QUIC";

/// Исполняемые файлы установленных Discord: обычный, PTB и Canary, все версии.
pub fn discord_exes() -> Vec<PathBuf> {
    let Some(base) = std::env::var_os("LOCALAPPDATA") else { return Vec::new() };
    discord_exes_in(Path::new(&base))
}

pub fn discord_exes_in(local: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (dir, exe) in [
        ("Discord", "Discord.exe"),
        ("DiscordPTB", "DiscordPTB.exe"),
        ("DiscordCanary", "DiscordCanary.exe"),
    ] {
        let Ok(entries) = std::fs::read_dir(local.join(dir)) else { continue };
        for e in entries.flatten() {
            if !e.file_name().to_string_lossy().starts_with("app-") {
                continue;
            }
            let p = e.path().join(exe);
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Путь для netsh. С кавычкой внутри его нельзя: строка разорвётся, и хвост
/// уйдёт в netsh отдельными аргументами.
fn годный_путь(p: &Path) -> Option<String> {
    let s = p.to_string_lossy();
    (!s.contains('"')).then(|| s.into_owned())
}

/// Аргументы добавления правила. Только исходящий UDP на порт 443 и только
/// для этой программы: голос Discord ходит по другим портам и не задевается.
pub fn add_rule_args(exe: &str) -> String {
    format!(
        "advfirewall firewall add rule name=\"{RULE_NAME}\" dir=out action=block protocol=UDP remoteport=443 program=\"{exe}\" enable=yes"
    )
}

/// Вывод netsh, если команда удалась.
fn netsh(args: &str) -> Option<String> {
    let mut cmd = Command::new(crate::sys::system_exe("netsh.exe"));
    cmd.raw_arg(args);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(crate::sys::CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    out.status.success().then(|| crate::sys::decode_console(&out.stdout))
}

/// Есть ли наше правило. netsh отвечает ошибкой, когда правил с таким именем нет.
pub fn enabled() -> bool {
    netsh(&format!("advfirewall firewall show rule name=\"{RULE_NAME}\"")).is_some()
}

/// Включает или выключает. Возвращает, сколько версий Discord под правилом.
pub fn set(on: bool) -> Result<usize, String> {
    // Сначала убираем все свои правила: так и выключение, и пересборка под
    // новые версии Discord делаются одним путём, без дублей.
    let _ = netsh(&format!("advfirewall firewall delete rule name=\"{RULE_NAME}\""));
    if !on {
        return if enabled() { Err("Не удалось убрать правило брандмауэра.".into()) } else { Ok(0) };
    }
    let exes = discord_exes();
    if exes.is_empty() {
        return Err("Discord не найден — правило не к чему привязать.".into());
    }
    let added = exes
        .iter()
        .filter_map(|e| годный_путь(e))
        .filter(|p| netsh(&add_rule_args(p)).is_some())
        .count();
    if added == 0 {
        return Err("Брандмауэр Windows не принял правило.".into());
    }
    Ok(added)
}

/// При запуске Klutz: правило включено, а Discord обновился и лежит в новой
/// папке — пересобираем правило под установленные версии.
pub fn refresh() {
    let Some(text) = netsh(&format!("advfirewall firewall show rule name=\"{RULE_NAME}\" verbose")) else {
        return;
    };
    // Подписи полей netsh переведены, поэтому ищем сами пути, а не «Program:».
    let text = text.to_lowercase();
    let не_хватает = discord_exes()
        .iter()
        .any(|e| !text.contains(&e.to_string_lossy().to_lowercase()));
    if не_хватает {
        let _ = set(true);
    }
}

#[derive(Debug, Serialize)]
pub struct QuicStatus {
    pub enabled: bool,
    /// Установлен ли Discord вообще — иначе переключатель бесполезен.
    pub installed: bool,
}

pub fn status() -> QuicStatus {
    QuicStatus { enabled: enabled(), installed: !discord_exes().is_empty() }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn находятся_все_установленные_версии_discord() {
        let dir = std::env::temp_dir().join(format!("klutz-quic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (folder, exe) in [
            ("Discord/app-1.0.9257", "Discord.exe"),
            ("Discord/app-1.0.9300", "Discord.exe"),
            ("DiscordPTB/app-1.0.1100", "DiscordPTB.exe"),
            // Не версия — служебная папка, её exe не берём.
            ("Discord/packages", "Discord.exe"),
        ] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
            std::fs::write(dir.join(folder).join(exe), "").unwrap();
        }
        let got: Vec<String> = discord_exes_in(&dir)
            .iter()
            .map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            got,
            vec![
                "Discord/app-1.0.9257/Discord.exe",
                "Discord/app-1.0.9300/Discord.exe",
                "DiscordPTB/app-1.0.1100/DiscordPTB.exe",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn правило_только_на_исходящий_udp_443_этой_программы() {
        let a = add_rule_args(r"C:\Users\u\AppData\Local\Discord\app-1.0.9257\Discord.exe");
        for part in ["dir=out", "action=block", "protocol=UDP", "remoteport=443"] {
            assert!(a.contains(part), "{a}");
        }
        assert!(a.contains(r#"program="C:\Users\u\AppData\Local\Discord\app-1.0.9257\Discord.exe""#), "{a}");
        assert!(годный_путь(Path::new(r#"C:\x" & calc"#)).is_none(), "кавычка разорвала бы строку netsh");
    }
}
