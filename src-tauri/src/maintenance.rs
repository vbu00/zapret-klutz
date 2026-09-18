use serde::Serialize;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::sys;
use crate::toggles::ipset_mode_from;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

pub const RAW_BASE: &str =
    "https://raw.githubusercontent.com/Flowseal/zapret-discord-youtube/refs/heads/main/.service";

/// Скачивание через curl.exe — он и так есть в Windows и уже используется
/// для проб связи, тащить ради этого целый HTTP-клиент с TLS незачем.
pub fn http_get(url: &str) -> Result<String, String> {
    #[allow(unused_mut)]
    let mut cmd = Command::new(sys::system_exe("curl.exe"));
    cmd.args(["-fsSL", "-m", "20", url]);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(sys::CREATE_NO_WINDOW);
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("не удалось скачать {url}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[derive(Debug, Serialize)]
pub struct IpsetUpdate {
    pub ok: bool,
    pub error: Option<String>,
    pub mode: String,
    pub applied: bool,
    /// Сколько адресов и сетей в скачанном списке.
    pub count: usize,
}

/// Меньше — это уже не список, а ошибка загрузки: настоящий у Flowseal
/// на десятки тысяч строк.
const MIN_IPSET_LINES: usize = 100;

fn is_net(line: &str) -> bool {
    let (ip, prefix) = match line.split_once('/') {
        Some((ip, p)) => (ip, Some(p)),
        None => (line, None),
    };
    ip.parse::<std::net::IpAddr>().is_ok() && prefix.is_none_or(|p| p.parse::<u8>().is_ok_and(|n| n <= 128))
}

/// Похож ли скачанный текст на список адресов. Проверка до записи обязательна:
/// пустой `ipset-all.txt` для zapret значит «применять ко всем адресам», то
/// есть обход начинает разбирать весь трафик игровых портов. Раньше любой
/// ответ сервера — пустой, обрезанный, страница ошибки — ложился в файл как есть.
pub fn check_ipset(text: &str) -> Result<usize, String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let good = lines.iter().filter(|l| is_net(l)).count();
    if good < MIN_IPSET_LINES {
        return Err(format!(
            "в скачанном списке всего {good} адресов — похоже на ошибку загрузки, файл не тронут"
        ));
    }
    if good * 10 < lines.len() * 9 {
        return Err("скачанный файл не похож на список адресов — файл не тронут".into());
    }
    Ok(good)
}

/// Бэкап всегда получает свежие данные, чтобы возврат в режим «loaded» их
/// подхватил. А живой файл трогаем только если фильтр сейчас и так в
/// «loaded»: режимы «any»/«none» выставлены осознанно, и эта кнопка не должна
/// молча их отменять.
pub fn update_ipset(root: &Path) -> IpsetUpdate {
    let fail = |e: String, mode: String| IpsetUpdate { ok: false, error: Some(e), mode, applied: false, count: 0 };
    let text = match http_get(&format!("{RAW_BASE}/ipset-service.txt")) {
        Ok(t) => t,
        Err(e) => return fail(e, String::new()),
    };
    let count = match check_ipset(&text) {
        Ok(n) => n,
        Err(e) => return fail(e, String::new()),
    };
    let list = root.join("lists").join("ipset-all.txt");
    let backup = root.join("lists").join("ipset-all.txt.backup");
    let mode = fs::read_to_string(&list)
        .map(|c| ipset_mode_from(&c))
        .unwrap_or_else(|_| "loaded".into());

    if let Err(e) = fs::write(&backup, &text) {
        return fail(e.to_string(), mode);
    }
    let applied = mode == "loaded";
    if applied {
        // Адреса игр, собранные сканированием, живут в этом же файле
        // помеченным блоком. Скачанный список кладём вместо чужого, а свой
        // блок переносим: иначе «Обновить список IPSet» молча стирал бы
        // результат сканирования, и человек не понял бы, куда он делся.
        let свои = fs::read_to_string(&list)
            .map(|c| crate::gamescan::extract_block(&c))
            .unwrap_or_default();
        let merged = crate::gamescan::merge_block(&text, &свои);
        if let Err(e) = fs::write(&list, &merged) {
            return fail(e.to_string(), mode);
        }
    }
    IpsetUpdate { ok: true, error: None, mode, applied, count }
}

#[derive(Debug, Serialize)]
pub struct UpdateCheck {
    pub ok: bool,
    pub error: Option<String>,
    pub local: String,
    pub remote: String,
    #[serde(rename = "upToDate")]
    pub up_to_date: bool,
    #[serde(rename = "releaseUrl")]
    pub release_url: String,
}

/// Версия загруженного релиза zapret-discord-youtube — из самого релиза
/// (LOCAL_VERSION в service.bat), а не из имени папки: папку могут
/// переименовать.
pub fn local_version(root: &Path) -> Option<String> {
    let svc = fs::read_to_string(root.join("service.bat")).ok()?;
    let v: String = svc
        .split("LOCAL_VERSION=")
        .nth(1)?
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '.')
        .collect();
    if v.is_empty() { None } else { Some(v) }
}

pub fn check_updates(root: &Path) -> UpdateCheck {
    let local = local_version(root).unwrap_or_else(|| "unknown".into());

    let remote = match http_get(&format!("{RAW_BASE}/version.txt")) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            return UpdateCheck {
                ok: false,
                error: Some(e),
                local,
                remote: String::new(),
                up_to_date: false,
                release_url: String::new(),
            }
        }
    };
    UpdateCheck {
        up_to_date: local == remote,
        release_url: format!(
            "https://github.com/Flowseal/zapret-discord-youtube/releases/tag/{remote}"
        ),
        ok: true,
        error: None,
        local,
        remote,
    }
}

#[derive(Debug, Serialize)]
pub struct CacheClear {
    pub ok: bool,
    pub cleared: Vec<String>,
}

const DISCORD_IMAGES: [&str; 3] = ["Discord.exe", "DiscordPTB.exe", "DiscordCanary.exe"];

pub fn clear_discord_cache() -> CacheClear {
    // Все каналы, а не только стабильный: PTB и Canary лежат отдельно.
    for image in DISCORD_IMAGES {
        sys::run("taskkill", &["/IM", image, "/F"]);
    }
    // taskkill возвращается раньше, чем процесс действительно исчезает, и
    // удаление падало на «файл занят», молча отдавая пустой список.
    for _ in 0..20 {
        if !DISCORD_IMAGES.iter().any(|i| sys::proc_running(i)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    // Без APPDATA путь получался относительным, и remove_dir_all ушёл бы
    // чистить каталог «discord» рядом с текущим рабочим каталогом.
    let base = match std::env::var("APPDATA") {
        Ok(b) if !b.trim().is_empty() => b,
        _ => return CacheClear { ok: false, cleared: Vec::new() },
    };
    let mut cleared = Vec::new();
    for channel in ["discord", "discordptb", "discordcanary"] {
        for dir in ["Cache", "Code Cache", "GPUCache"] {
            let p = Path::new(&base).join(channel).join(dir);
            if p.exists() && fs::remove_dir_all(&p).is_ok() {
                cleared.push(format!("{channel}/{dir}"));
            }
        }
    }
    CacheClear { ok: true, cleared }
}

/// Пользовательские списки, на которые ссылается КАЖДЫЙ конфиг zapret
/// (`--hostlist="%LISTS%list-general-user.txt"` и ещё два), но которых нет в
/// поставке: их создаёт `service.bat load_user_lists`, а вызывает его сам
/// .bat перед запуском winws.
///
/// Klutz поднимает winws напрямую, разобрав аргументы, и этот шаг пропускал.
/// На чистой установке свежего релиза файлов нет, winws не может открыть
/// список и выходит сразу — окно показывало «winws.exe не запустился,
/// проверь конфиг вручную» на каждом конфиге. Содержимое — ровно то, что
/// пишет service.bat; существующие файлы не трогаем.
pub fn ensure_user_lists(root: &Path) {
    let lists = root.join("lists");
    if !lists.is_dir() {
        return;
    }
    for (name, body) in [
        ("ipset-exclude-user.txt", "203.0.113.113/32
"),
        // Пустым этот файл оставлять нельзя — так написано и в самом
        // релизе: hostlist без строк означает «применять ко всему».
        ("list-general-user.txt", "# Never leave this file empty
domain.example.abc
"),
        ("list-exclude-user.txt", "domain.example.abc
"),
    ] {
        let path = lists.join(name);
        if !path.exists() {
            let _ = fs::write(&path, body);
        }
    }
}

// ─────────── Свои списки доменов ───────────

fn list_paths(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        root.join("lists").join("list-general-user.txt"),
        root.join("lists").join("list-exclude-user.txt"),
    )
}

fn read_list(p: &Path) -> String {
    fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Serialize)]
pub struct CustomLists {
    pub ok: bool,
    pub include: String,
    pub exclude: String,
}

pub fn get_custom_lists(root: &Path) -> CustomLists {
    let (inc, exc) = list_paths(root);
    CustomLists { ok: true, include: read_list(&inc), exclude: read_list(&exc) }
}

/// Строка списка — к домену. zapret сравнивает имя сайта, и вставленная целиком
/// ссылка `https://www.youtube.com/watch?v=…` в списке не срабатывала никогда.
/// Поддомены домен покрывает сам, так что `*.` тоже лишнее. Комментарии
/// оставляем как есть.
pub fn clean_domain(line: &str) -> Option<String> {
    let l = line.trim();
    if l.is_empty() {
        return None;
    }
    if l.starts_with('#') {
        return Some(l.to_string());
    }
    let l = l.to_lowercase();
    let l = l.split_once("://").map_or(l.as_str(), |(_, rest)| rest);
    let l = l.split(['/', '?', '#']).next().unwrap_or("");
    let l = l.rsplit('@').next().unwrap_or(l);
    let l = l.split(':').next().unwrap_or(l);
    let l = l.trim_start_matches("*.").trim_matches('.');
    (!l.is_empty()).then(|| l.to_string())
}

pub fn save_custom_lists(root: &Path, include: &str, exclude: &str) -> Result<(), String> {
    let (inc_path, exc_path) = list_paths(root);
    let clean = |t: &str| {
        let mut seen = std::collections::HashSet::new();
        let body = t
            .lines()
            .filter_map(clean_domain)
            .filter(|l| seen.insert(l.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        if body.is_empty() { String::new() } else { format!("{body}\n") }
    };
    let inc = clean(include);
    let exc = clean(exclude);
    // Пустой файл ломает winws — оставляем заглушку, как это делает сам zapret.
    fs::write(
        &inc_path,
        if inc.is_empty() { "# Never leave this file empty\ndomain.example.abc\n".to_string() } else { inc },
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        &exc_path,
        if exc.is_empty() { "domain.example.abc\n".to_string() } else { exc },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Без этих файлов winws не стартует ни с одним конфигом: в поставке
    /// свежего релиза их нет, а в строке запуска они есть.
    #[test]
    fn недостающие_пользовательские_списки_создаются() {
        let dir = std::env::temp_dir().join(format!("klutz-lists-{}", std::process::id()));
        let lists = dir.join("lists");
        let _ = fs::create_dir_all(&lists);
        // Один файл уже лежит и правлен человеком — его трогать нельзя.
        fs::write(lists.join("list-exclude-user.txt"), "моё.example
").unwrap();

        ensure_user_lists(&dir);

        let general = fs::read_to_string(lists.join("list-general-user.txt")).unwrap();
        assert!(!general.trim().is_empty(), "пустой hostlist означает «применять ко всему»");
        assert!(fs::read_to_string(lists.join("ipset-exclude-user.txt")).unwrap().contains("203.0.113.113"));
        assert_eq!(
            fs::read_to_string(lists.join("list-exclude-user.txt")).unwrap(),
            "моё.example
",
            "существующий список перезаписывать нельзя"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Папки lists нет — значит это не релиз zapret, и создавать там нечего.
    #[test]
    fn без_папки_списков_ничего_не_создаётся() {
        let dir = std::env::temp_dir().join(format!("klutz-nolists-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        ensure_user_lists(&dir);
        assert!(!dir.join("lists").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn строки_списка_приводятся_к_домену() {
        assert_eq!(clean_domain("https://www.YouTube.com/watch?v=1").as_deref(), Some("www.youtube.com"));
        assert_eq!(clean_domain("*.discord.gg").as_deref(), Some("discord.gg"));
        assert_eq!(clean_domain("example.com:443/path").as_deref(), Some("example.com"));
        assert_eq!(clean_domain("  rutracker.org.  ").as_deref(), Some("rutracker.org"));
        assert_eq!(clean_domain("# свой комментарий").as_deref(), Some("# свой комментарий"));
        assert_eq!(clean_domain("   "), None);
        assert_eq!(clean_domain("https://"), None);
    }

    #[test]
    fn скачанный_ipset_проверяется_до_записи() {
        let good: String = (0..150).map(|i| format!("10.{}.0.0/16\n", i % 250)).collect();
        assert_eq!(check_ipset(&format!("# шапка\n{good}\n2606:4700::/32\n")), Ok(151));
        // Пусто, обрезано, страница ошибки — файл не трогаем.
        assert!(check_ipset("").is_err());
        assert!(check_ipset("1.1.1.1/32\n2.2.2.2\n").is_err());
        let html: String = (0..200).map(|_| "<div>error</div>\n").collect();
        assert!(check_ipset(&format!("{good}{html}")).is_err());
        assert!(!is_net("10.0.0.0/129") && !is_net("<html>") && is_net("203.0.113.113/32"));
    }

    #[test]
    fn версия_берётся_из_local_version() {
        let dir = std::env::temp_dir().join(format!("klutz-ver-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("service.bat"), "@echo off\r\nset \"LOCAL_VERSION=1.9.9c\"\r\n").unwrap();
        assert_eq!(local_version(&dir).as_deref(), Some("1.9.9c"));

        fs::write(dir.join("service.bat"), "@echo off\r\n").unwrap();
        assert_eq!(local_version(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
