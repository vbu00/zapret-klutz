//! Скачивание релизов zapret с GitHub и распаковка .zip.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::{AppHandle, Emitter, Manager};

use crate::sys;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const REPO: &str = "Flowseal/zapret-discord-youtube";

fn curl_text(url: &str) -> Result<String, String> {
    #[allow(unused_mut)]
    let mut cmd = Command::new(sys::system_exe("curl.exe"));
    cmd.args(["-fsSL", "-m", "20", "-H", "User-Agent: klutz", url]);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(sys::CREATE_NO_WINDOW);
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("не удалось связаться с GitHub".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[derive(Debug, Serialize)]
pub struct LatestRelease {
    pub ok: bool,
    pub error: Option<String>,
    pub version: String,
    pub name: String,
    pub size: u64,
    pub url: String,
    #[serde(rename = "notesUrl")]
    pub notes_url: String,
    /// SHA-256 архива из ответа GitHub. Нет — сверяем только размер.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Контрольная сумма файла релиза из ответа GitHub: поле `digest` вида
/// `sha256:<64 hex>`. У старых релизов его нет — тогда `None`.
pub fn parse_digest(asset: &serde_json::Value) -> Option<String> {
    let hex = asset.get("digest")?.as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

pub fn latest_release() -> LatestRelease {
    let fail = |e: String| LatestRelease {
        ok: false,
        error: Some(e),
        version: String::new(),
        name: String::new(),
        size: 0,
        url: String::new(),
        notes_url: String::new(),
        sha256: None,
    };
    let text = match curl_text(&format!("https://api.github.com/repos/{REPO}/releases/latest")) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return fail("GitHub ответил неожиданным форматом.".into());
    };
    let asset = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|a| a.get("name").and_then(|n| n.as_str()).map(|n| n.to_lowercase().ends_with(".zip")).unwrap_or(false))
        });
    let Some(asset) = asset else {
        return fail("В последнем релизе нет .zip файла.".into());
    };
    LatestRelease {
        ok: true,
        error: None,
        version: v.get("tag_name").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        name: asset.get("name").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        size: asset.get("size").and_then(|t| t.as_u64()).unwrap_or(0),
        url: asset.get("browser_download_url").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        notes_url: v.get("html_url").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        sha256: parse_digest(asset),
    }
}

/// Куда мы вообще готовы пойти за архивом.
///
/// Адрес приходит полем `browser_download_url` из ответа GitHub и раньше
/// уезжал в curl как есть. Распакованное из этого архива потом запускается
/// администратором, так что «куда сказали, туда и пошли» здесь слишком
/// дорого стоит.
const ASSET_HOSTS: [&str; 4] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "codeload.github.com",
];

/// Только https и только к GitHub. Отдельно отсекаем `user@host`: это
/// обычный способ увести запрос на чужой адрес, оставив знакомый на вид URL.
pub fn download_url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let host = authority.split(':').next().unwrap_or("");
    ASSET_HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h))
}

/// Куда складываем скачанные релизы — рядом с настройками приложения.
pub fn releases_dir(app: &AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().expect("no app data dir").join("releases");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Корень релиза внутри папки.
///
/// Архив zapret распаковывается с одной верхней папкой, и в каталоге
/// релизов лежит `<имя>\<имя>\bin\winws.exe`. То же получается, когда
/// человек распаковал .zip сам и в диалоге выбрал внешнюю папку. Если в
/// самой папке релиза нет, а внутри ровно один подкаталог — и релиз в нём,
/// корнем считаем его. Иначе отдаём папку как есть: гадать не будем.
pub fn release_root(dir: &Path) -> PathBuf {
    let is_release = |p: &Path| p.join("bin").join("winws.exe").exists();
    if is_release(dir) {
        return dir.to_path_buf();
    }
    let subdirs: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    match subdirs.as_slice() {
        [only] if is_release(only) => only.clone(),
        _ => dir.to_path_buf(),
    }
}

/// Скачиваем через curl с `--progress-bar`: он пишет проценты в stderr, что
/// даёт живой прогресс без своего HTTP-клиента с редиректами (GitHub отдаёт
/// 302 на S3, curl идёт по ним сам с -L).
pub fn download_latest(app: &AppHandle) -> Result<PathBuf, String> {
    let info = latest_release();
    if !info.ok {
        return Err(info.error.unwrap_or_else(|| "не удалось получить релиз".into()));
    }
    if !download_url_allowed(&info.url) {
        return Err(format!(
            "GitHub вернул ссылку на неожиданный адрес, скачивание отменено: {}",
            info.url
        ));
    }
    let dest = releases_dir(app).join(&info.name);
    download_file(app, &info.url, &dest, info.size, info.sha256.as_deref(), "download-progress")?;
    Ok(dest)
}

/// Скачивание файла с GitHub с живым прогрессом в событие `event`.
///
/// Общее для релиза zapret и установщика Klutz: и то и другое потом
/// запускается с правами администратора, так что правила одни — только https,
/// обрыв при зависании, сверка размера с заявленным.
pub fn download_file(
    app: &AppHandle,
    url: &str,
    dest: &Path,
    expected_size: u64,
    sha256: Option<&str>,
    event: &str,
) -> Result<(), String> {
    #[allow(unused_mut)]
    let mut cmd = Command::new(sys::system_exe("curl.exe"));
    cmd.args([
        "-L",
        "--fail",
        "--progress-bar",
        // Ни сам запрос, ни редирект за ним не должны сойти на http:
        // -L идёт по цепочке сам, и без этого её хвост мог бы оказаться
        // открытым.
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        // Иначе зависшее после установки соединение держит нас вечно:
        // общего таймаута на закачку ставить нельзя (файл большой), а вот
        // «меньше килобайта в секунду полминуты» — верный признак смерти.
        "--connect-timeout",
        "20",
        "--speed-limit",
        "1024",
        "--speed-time",
        "30",
        "-H",
        "User-Agent: klutz",
        "-o",
        &dest.to_string_lossy(),
        url,
    ])
    .stderr(std::process::Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(sys::CREATE_NO_WINDOW);

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(stderr) = child.stderr.take() {
        use std::io::Read;
        let app2 = app.clone();
        let event2 = event.to_string();
        std::thread::spawn(move || {
            let mut reader = stderr;
            let mut buf = [0u8; 256];
            let mut acc = String::new();
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                // curl рисует прогресс через возврат каретки, а не перевод строки.
                if let Some(pct) = acc.rsplit(['\r', '\n']).find_map(parse_percent) {
                    let _ = app2.emit(&event2, pct);
                }
                if acc.len() > 4096 {
                    acc.clear();
                }
            }
        });
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        let _ = fs::remove_file(dest);
        return Err("Скачивание не удалось.".into());
    }
    // Размер известен из ответа API — сверяем: обрыв и усечение он ловит, а
    // скачанное потом запускается с правами администратора.
    if expected_size > 0 {
        match fs::metadata(dest) {
            Ok(m) if m.len() == expected_size => {}
            Ok(m) => {
                let _ = fs::remove_file(dest);
                return Err(format!(
                    "Размер скачанного файла не совпал с заявленным: {} байт вместо {}. Файл удалён.",
                    m.len(),
                    expected_size
                ));
            }
            Err(e) => {
                let _ = fs::remove_file(dest);
                return Err(format!("Не удалось проверить скачанный файл: {e}"));
            }
        }
    }
    // Сумма — если GitHub её дал (поле digest у файла релиза). Размер ловит
    // обрыв, сумма — ещё и подмену по дороге.
    if let Some(want) = sha256 {
        match sys::sha256_file(dest) {
            Ok(got) if got.eq_ignore_ascii_case(want) => {}
            Ok(_) => {
                let _ = fs::remove_file(dest);
                return Err(
                    "Контрольная сумма скачанного файла не совпала с опубликованной на GitHub. Файл удалён — \
                     скачивание было повреждено или подменено."
                        .into(),
                );
            }
            Err(e) => {
                let _ = fs::remove_file(dest);
                return Err(format!("Не удалось проверить контрольную сумму: {e}. Файл удалён."));
            }
        }
    }
    let _ = app.emit(event, 100u8);
    Ok(())
}

fn parse_percent(chunk: &str) -> Option<u8> {
    let t = chunk.trim();
    if t.is_empty() {
        return None;
    }
    // Полоса curl выглядит как "######   45.2%"
    let num: String = t
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if !t.ends_with('%') || num.is_empty() {
        return None;
    }
    num.parse::<f64>().ok().map(|v| v.clamp(0.0, 100.0) as u8)
}

/// Распаковка .zip.
///
/// Пишем во временную папку рядом и подменяем готовое одним движением.
/// Раскладывать файлы поверх существующего релиза нельзя: на любом сбое —
/// занятый файл, нехватка места, антивирус — остаётся огрызок, который
/// выглядит как релиз, но уже без `winws.exe` и части конфигов. Приложение
/// потом честно считает такую папку негодной, а человек видит лишь странную
/// ошибку и сломанную установку.
///
/// Внутри архивы zapret обычно лежат одной верхней папкой — если так,
/// корнем релиза считаем её, а не временную обёртку.
pub fn extract_zip(zip_path: &Path, target_dir: &Path) -> Result<PathBuf, String> {
    let parent = target_dir.parent().ok_or("некуда распаковывать")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;

    let leaf = target_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "release".into());
    // Рядом с целью, а не в %TEMP%: переименование работает мгновенно только
    // в пределах одного тома, а каталог релизов может лежать не на системном.
    let staging = parent.join(format!(".{leaf}.partial"));
    let _ = fs::remove_dir_all(&staging);

    if let Err(e) = unpack_into(zip_path, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    if target_dir.exists() && fs::remove_dir_all(target_dir).is_err() {
        // Почти всегда это загруженный драйвер: он держит bin\WinDivert64.sys
        // и остаётся в ядре после выхода winws.exe. Гасим обход и пробуем ещё
        // раз — иначе обновить релиз можно было бы только перезагрузкой.
        crate::winws::stop_winws();
        for name in ["WinDivert", "WinDivert14"] {
            crate::sys::run("net", &["stop", name]);
        }
        if let Err(e) = fs::remove_dir_all(target_dir) {
            let _ = fs::remove_dir_all(&staging);
            return Err(locked_hint(&e));
        }
    }

    if let Err(e) = fs::rename(&staging, target_dir) {
        let _ = fs::remove_dir_all(&staging);
        return Err(locked_hint(&e));
    }

    let entries: Vec<_> = fs::read_dir(target_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    if entries.len() == 1 && entries[0].path().is_dir() {
        return Ok(entries[0].path());
    }
    Ok(target_dir.to_path_buf())
}

fn unpack_into(zip_path: &Path, dir: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        // enclosed_name отбрасывает пути с ".." — защита от zip slip.
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dir.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(p) = out.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut dst = fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut dst).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// «Процесс не может получить доступ к файлу» человеку не объясняет ничего.
/// Ошибка 32 здесь почти всегда об одном и том же.
fn locked_hint(e: &std::io::Error) -> String {
    if e.raw_os_error() == Some(32) {
        "Файлы релиза заняты: в памяти остался драйвер WinDivert.          Останови обход и попробуй снова; если не поможет — перезагрузи компьютер."
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        e.to_string()
    }
}

#[derive(Debug, Serialize)]
pub struct ReleaseEntry {
    pub name: String,
    pub path: String,
    pub current: bool,
    /// Когда папку распаковали — мс от эпохи, как ждёт `new Date(...)`.
    /// None, если файловая система не отдала время.
    #[serde(rename = "extractedAt")]
    pub extracted_at: Option<u64>,
}

fn dir_created_ms(entry: &fs::DirEntry) -> Option<u64> {
    let meta = entry.metadata().ok()?;
    let t = meta.created().or_else(|_| meta.modified()).ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64)
}

pub fn list_releases(app: &AppHandle, current_root: Option<&str>) -> Vec<ReleaseEntry> {
    let dir = releases_dir(app);
    fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| {
                    let dir = e.path();
                    // Активный релиз узнаём по вложенности, а не по равенству
                    // строк: root_path указывает внутрь архива (внешняя папка
                    // → одноимённая внутренняя), а здесь — внешняя. Строгое
                    // сравнение не совпадало никогда: у активного релиза не
                    // было пометки, зато были «Переключиться» и «Удалить» —
                    // и удалить его из-под себя было можно.
                    let current = current_root
                        .map(|c| Path::new(c).starts_with(&dir))
                        .unwrap_or(false);
                    ReleaseEntry {
                        name: e.file_name().to_string_lossy().to_string(),
                        current,
                        extracted_at: dir_created_ms(&e),
                        // Настоящий корень релиза: «Переключиться» уходило с
                        // внешней папкой и упиралось в «нет bin\winws.exe».
                        path: release_root(&dir).to_string_lossy().to_string(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn delete_release(app: &AppHandle, folder: &str) -> Result<(), String> {
    // Двоеточие тоже: `Path::join("C:Users")` в Windows отбрасывает базовый
    // путь, и remove_dir_all ушёл бы гулять за пределы каталога релизов.
    // «.» тоже: оно проходило все прежние проверки, а join(".") оставляет
    // путь на самом каталоге релизов — remove_dir_all снёс бы их все разом.
    if !crate::commands::safe_name(folder) {
        return Err("Недопустимое имя папки.".into());
    }
    let p = releases_dir(app).join(folder);
    if !p.exists() {
        return Err("Папка не найдена.".into());
    }
    fs::remove_dir_all(p).map_err(|e| e.to_string())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn ссылка_на_архив_принимается_только_от_github_по_https() {
        assert!(download_url_allowed(
            "https://github.com/Flowseal/zapret-discord-youtube/releases/download/1.9.9c/a.zip"
        ));
        assert!(download_url_allowed("https://objects.githubusercontent.com/x/y.zip"));
        assert!(download_url_allowed("https://RELEASE-ASSETS.githubusercontent.com/x.zip"));

        assert!(!download_url_allowed("http://github.com/x.zip"), "http не годится");
        assert!(!download_url_allowed("https://evil.example/x.zip"));
        assert!(!download_url_allowed("https://github.com.evil.example/x.zip"));
        assert!(!download_url_allowed("https://github.com@evil.example/x.zip"), "user@host");
        assert!(!download_url_allowed("ftp://github.com/x.zip"));
        assert!(!download_url_allowed(""));
    }

    #[test]
    fn процент_из_полосы_curl() {
        assert_eq!(parse_percent("######                    45.2%"), Some(45));
        assert_eq!(parse_percent("100.0%"), Some(100));
        assert_eq!(parse_percent(""), None);
        assert_eq!(parse_percent("######"), None);
        assert_eq!(parse_percent("просто текст"), None);
    }
    /// Минимальный .zip с одним файлом внутри.
    fn make_zip(path: &std::path::Path, inner: &str, body: &[u8]) {
        let f = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        w.start_file(inner, zip::write::SimpleFileOptions::default()).unwrap();
        use std::io::Write;
        w.write_all(body).unwrap();
        w.finish().unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klutz-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn распаковка_заменяет_старый_релиз_целиком() {
        let dir = scratch("replace");
        let target = dir.join("release");

        let z1 = dir.join("a.zip");
        make_zip(&z1, "старый.txt", b"1");
        extract_zip(&z1, &target).unwrap();
        assert!(target.join("старый.txt").exists());

        let z2 = dir.join("b.zip");
        make_zip(&z2, "новый.txt", b"2");
        extract_zip(&z2, &target).unwrap();

        // Именно замена, а не подмешивание: файла из прошлой версии остаться
        // не должно, иначе в релизе копятся чужие конфиги от старых выпусков.
        assert!(target.join("новый.txt").exists());
        assert!(!target.join("старый.txt").exists(), "старый файл пережил распаковку");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn сорванная_распаковка_не_портит_то_что_уже_стоит() {
        let dir = scratch("keep");
        let target = dir.join("release");

        let good = dir.join("good.zip");
        make_zip(&good, "winws.exe", b"real release");
        extract_zip(&good, &target).unwrap();

        // Битый архив: раньше файлы ложились прямо в цель, и такой обрыв
        // оставлял папку, похожую на релиз, но без половины файлов.
        let broken = dir.join("broken.zip");
        fs::write(&broken, b"not a zip at all").unwrap();
        assert!(extract_zip(&broken, &target).is_err());

        assert!(target.join("winws.exe").exists(), "рабочий релиз пострадал от чужого сбоя");
        assert_eq!(fs::read(target.join("winws.exe")).unwrap(), b"real release");
        // И мусор после себя не оставили.
        assert!(!dir.join(".release.partial").exists(), "осталась временная папка");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn корень_релиза_находится_во_вложенной_папке() {
        let dir = scratch("root");
        // Как после распаковки архива zapret: <имя>\<имя>\bin\winws.exe.
        let outer = dir.join("zapret-x");
        let inner = outer.join("zapret-x");
        fs::create_dir_all(inner.join("bin")).unwrap();
        fs::write(inner.join("bin").join("winws.exe"), b"").unwrap();

        assert_eq!(release_root(&outer), inner, "внешняя папка → внутренняя");
        assert_eq!(release_root(&inner), inner, "настоящий корень остаётся собой");
        // Папка без релиза и с одним подкаталогом, в котором релиза нет
        // напрямую, — как есть: глубже одного уровня не спускаемся.
        assert_eq!(release_root(&dir), dir);
        // Две подпапки — неоднозначно, ничего не угадываем.
        fs::create_dir_all(outer.join("другая")).unwrap();
        assert_eq!(release_root(&outer), outer);
        let _ = fs::remove_dir_all(&dir);
    }
}
