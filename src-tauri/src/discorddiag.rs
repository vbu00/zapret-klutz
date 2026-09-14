//! «Почему Discord не запускается».
//!
//! Всё, что раньше выяснялось руками, по шагам: где встал последний запуск —
//! по логам самого Discord; не идёт ли он через системный прокси мимо обхода;
//! проходит ли через обход то, что ему нужно при запуске, — и что с этим
//! делать. Живой случай, из которого это выросло: Discord висел на
//! «Starting...», тесты были зелёными, и на выяснение ушёл час.

use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::gamescan::Proto;

// ─────────── последний запуск по логам ───────────

/// Где встал последний запуск Discord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Логов нет — на этой машине Discord не запускался.
    NoLogs,
    /// Застрял на проверке обновлений.
    UpdateStuck,
    /// Проверка обновлений не прошла.
    UpdateFailed,
    /// Обновление прошло, а окно приложения к серверу так и не подключилось.
    StuckLaunching,
    /// Подключение к серверу сообщений началось и не закончилось.
    GatewayStuck,
    /// Подключился к серверу за столько миллисекунд.
    Started { connect_ms: u64 },
}

/// Время из строки лога: `[2026-09-14 13:36:18.408] …` → `2026-09-14 13:36`.
fn отметка_времени(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?.get(..16)?;
    inner.starts_with(|c: char| c.is_ascii_digit()).then(|| inner.to_string())
}

/// Разбирает `renderer_js.log`: последний запуск и когда он начался.
///
/// Берём только строки этапов. Лог Discord видит и личное — его здесь не
/// читают дальше маркеров и никуда не выводят.
pub fn last_start(log: &str) -> (Stage, Option<String>) {
    let lines: Vec<&str> = log.lines().collect();
    let Some(start) = lines.iter().rposition(|l| l.contains("splashScreenPreload: signalReady")) else {
        return (Stage::NoLogs, None);
    };
    let when = отметка_времени(lines[start]);
    let tail = &lines[start..];
    let подключился = tail.iter().rev().find_map(|l| {
        let rest = l.split("[FAST CONNECT] connected in ").nth(1)?;
        rest.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<u64>().ok()
    });
    if let Some(connect_ms) = подключился {
        return (Stage::Started { connect_ms }, when);
    }
    let есть = |needle: &str| tail.iter().any(|l| l.contains(needle));
    let stage = if есть("[FAST CONNECT] wss://") {
        Stage::GatewayStuck
    } else if есть(r#""status":"launching""#) {
        Stage::StuckLaunching
    } else if есть(r#""status":"update-failure""#) {
        Stage::UpdateFailed
    } else {
        Stage::UpdateStuck
    };
    (stage, when)
}

/// Конец лога самого свежего из каналов Discord.
fn renderer_log() -> Option<String> {
    let base = PathBuf::from(std::env::var_os("APPDATA")?);
    let (_, newest) = ["discord", "discordptb", "discordcanary"]
        .iter()
        .map(|d| base.join(d).join("logs").join("renderer_js.log"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max_by_key(|(t, _)| *t)?;
    let bytes = std::fs::read(newest).ok()?;
    // Лог растёт до мегабайтов, а нужен последний запуск — конец файла.
    let tail = &bytes[bytes.len().saturating_sub(512 * 1024)..];
    Some(String::from_utf8_lossy(tail).into_owned())
}

// ─────────── системный прокси ───────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proxy {
    pub server: String,
    /// Программа, которая держит порт прокси, — Hiddify, v2rayN и подобные.
    pub owner: Option<String>,
}

/// Значение из вывода `reg query`: `    ProxyServer    REG_SZ    http://127.0.0.1:12334`.
pub fn reg_value(out: &str, name: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let mut f = l.split_whitespace();
        if f.next()? != name {
            return None;
        }
        let _kind = f.next()?;
        let v = f.collect::<Vec<_>>().join(" ");
        (!v.is_empty()).then_some(v)
    })
}

/// Порт прокси: `http://127.0.0.1:12334`, `127.0.0.1:8080` или
/// `http=127.0.0.1:8080;https=127.0.0.1:8080`.
pub fn proxy_port(server: &str) -> Option<u16> {
    let first = server.split(';').next()?;
    let hostport = first.rsplit("://").next()?.rsplit('=').next()?;
    hostport.trim_end_matches('/').rsplit(':').next()?.parse().ok()
}

/// Системный прокси Windows. `None` — выключен.
pub fn system_proxy() -> Option<Proxy> {
    let out = crate::sys::run(
        "reg",
        &["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"],
    );
    let on = reg_value(&out, "ProxyEnable").is_some_and(|v| v == "0x1");
    if !on {
        return None;
    }
    let server = reg_value(&out, "ProxyServer").unwrap_or_default();
    let owner = proxy_port(&server).and_then(|port| {
        let pid = crate::gamescan::local_sockets()
            .into_iter()
            .find(|(proto, p, _)| *proto == Proto::Tcp && *p == port)
            .map(|(_, _, pid)| pid)?;
        crate::gamescan::process_names().get(&pid).cloned()
    });
    Some(Proxy { server, owner })
}

// ─────────── сеть через обход ───────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answer {
    pub code: u16,
    pub bytes: u64,
    pub secs: f64,
}

/// Вывод `-w "%{http_code} %{size_download} %{time_total}"`.
pub fn parse_answer(s: &str) -> Answer {
    let mut f = s.split_whitespace();
    Answer {
        code: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        bytes: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        secs: f.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
    }
}

/// curl мимо прокси: проверяем именно путь через обход. Системный прокси curl
/// и так не берёт, а переменные окружения с прокси — может.
fn curl_cmd(timeout: u64) -> Command {
    let mut cmd = Command::new(crate::sys::system_exe("curl.exe"));
    cmd.args(["-s", "--noproxy", "*", "--connect-timeout", "8", "-m"]).arg(timeout.to_string());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(crate::sys::CREATE_NO_WINDOW);
    cmd
}

fn curl(extra: &[&str], url: &str, timeout: u64) -> Answer {
    let mut cmd = curl_cmd(timeout);
    cmd.args(["-o", "NUL", "-w", "%{http_code} %{size_download} %{time_total}"]).args(extra).arg(url);
    cmd.output()
        .map(|o| parse_answer(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Тело страницы и код ответа.
fn curl_body(url: &str) -> (u16, String) {
    let mut cmd = curl_cmd(15);
    cmd.args(["-w", "\\n%{http_code}"]).arg(url);
    let out = cmd.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    match out.rsplit_once('\n') {
        Some((body, code)) => (code.trim().parse().unwrap_or(0), body.to_string()),
        None => (out.trim().parse().unwrap_or(0), String::new()),
    }
}

/// Путь к большому скрипту приложения со страницы discord.com/app. Главный —
/// `web.<хэш>.js` на мегабайты; нет его — любой скрипт из `/assets/`.
pub fn big_asset(html: &str) -> Option<String> {
    let найти = |prefix: &str| {
        html.match_indices(prefix).find_map(|(i, _)| {
            let rest = &html[i..];
            let end = rest.find(|c: char| c == '"' || c == '\'' || c.is_whitespace())?;
            let path = &rest[..end];
            let чисто = path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c));
            (path.ends_with(".js") && чисто).then(|| path.to_string())
        })
    };
    найти("/assets/web.").or_else(|| найти("/assets/"))
}

fn размер(b: u64) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1} МБ", b as f64 / 1_048_576.0)
    } else {
        format!("{} КБ", b.div_ceil(1024))
    }
}

fn ответ(a: &Answer) -> String {
    if a.code == 0 {
        "нет ответа".into()
    } else {
        format!("код {}, {} за {:.1} с", a.code, размер(a.bytes), a.secs)
    }
}

// ─────────── вердикт ───────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    None,
    EnableBypass,
    PickStrategy,
    EnableNoQuic,
    ClearCache,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub label: String,
    /// `None` — просто сведения, не хорошо и не плохо.
    pub ok: Option<bool>,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub verdict: String,
    pub advice: String,
    pub action: Action,
    pub checks: Vec<Check>,
}

pub struct Facts {
    pub installed: bool,
    pub stage: Stage,
    pub proxy: Option<Proxy>,
    pub bypass_on: bool,
    /// Что из нужного Discord через обход не прошло.
    pub net_failed: Vec<String>,
    pub quic_rule: bool,
}

/// Вердикт, совет и что предложить нажать. Порядок важен: сначала то, что
/// делает остальные проверки бессмысленными.
pub fn decide(f: &Facts) -> (String, String, Action) {
    if !f.installed {
        return (
            "Discord не найден".into(),
            "Klutz ищет его в %LOCALAPPDATA%\\Discord. Если он установлен в другое место, эта проверка его не видит."
                .into(),
            Action::None,
        );
    }
    // Прокси — первым: при нём приложение Discord идёт мимо обхода, и ни
    // стратегия, ни тесты Klutz к нему не относятся. Ровно на этом сегодня
    // ушёл час: тесты шли напрямую, Discord — через Hiddify.
    if let Some(p) = &f.proxy {
        let кто = p.owner.clone().unwrap_or_else(|| p.server.clone());
        return (
            format!("Discord идёт через прокси {кто}"),
            format!(
                "Приложение Discord берёт системный прокси Windows и ходит через него — мимо обхода, поэтому \
                 ни стратегия, ни тесты Klutz на него не влияют. Чтобы проверить обход, выключи {кто} и \
                 запусти Discord заново."
            ),
            Action::None,
        );
    }
    if let Stage::Started { connect_ms } = f.stage {
        if f.net_failed.is_empty() {
            return (
                "Последний запуск прошёл нормально".into(),
                format!(
                    "Discord подключился к серверу за {connect_ms} мс, и всё, что ему нужно, через обход \
                     проходит. Если он зависает сейчас — повтори проверку, пока он висит: тогда будет видно, где."
                ),
                Action::None,
            );
        }
    }
    if !f.bypass_on {
        return (
            "Обход выключен".into(),
            "Без обхода Discord режут. Включи обход и запусти Discord заново.".into(),
            Action::EnableBypass,
        );
    }
    if !f.net_failed.is_empty() {
        return (
            "Обход не пропускает Discord".into(),
            format!(
                "Не проходит: {}. Эта стратегия Discord не пробивает — подбери другую.",
                f.net_failed.join(", ").to_lowercase()
            ),
            Action::PickStrategy,
        );
    }
    match f.stage {
        Stage::NoLogs => (
            "Discord ещё не запускался".into(),
            "Логов запуска нет. Запусти Discord, и если он зависнет — повтори проверку, пока он висит.".into(),
            Action::None,
        ),
        _ if !f.quic_rule => (
            "Сеть в порядке, а Discord зависает".into(),
            "Всё, что Discord качает по обычному HTTPS, через обход проходит, — а сам он висит. Похоже на QUIC: \
             приложение Discord ходит по нему, а тесты его не проверяют. Включи «Discord без QUIC» и \
             перезапусти Discord полностью."
                .into(),
            Action::EnableNoQuic,
        ),
        _ => (
            "Сеть в порядке, QUIC закрыт, а Discord зависает".into(),
            "Остаётся кэш: Discord мог запомнить сломанные ответы. Klutz закроет Discord и очистит кэш — потом \
             запусти его заново."
                .into(),
            Action::ClearCache,
        ),
    }
}

fn этап(stage: &Stage) -> (Option<bool>, String) {
    match stage {
        Stage::NoLogs => (None, "логов нет — Discord на этой машине ещё не запускался".into()),
        Stage::UpdateStuck => (Some(false), "завис на проверке обновлений".into()),
        Stage::UpdateFailed => (Some(false), "проверка обновлений не прошла".into()),
        Stage::StuckLaunching => (Some(false), "обновление прошло, а окно приложения не подключилось".into()),
        Stage::GatewayStuck => (Some(false), "начал подключаться к серверу и не закончил".into()),
        Stage::Started { connect_ms } => (Some(true), format!("подключился к серверу за {connect_ms} мс")),
    }
}

fn добавить(checks: &mut Vec<Check>, failed: &mut Vec<String>, label: &str, ok: bool, detail: String) {
    if !ok {
        failed.push(label.to_string());
    }
    checks.push(Check { label: label.into(), ok: Some(ok), detail });
}

/// Вся проверка. `bypass` — описание работающего обхода, `None` — выключен.
/// Идёт до полуминуты: сетевые пробы по очереди, с таймаутами.
pub fn run(bypass: Option<String>) -> Report {
    let exes = crate::quic::discord_exes();
    let (stage, when) = renderer_log().map(|l| last_start(&l)).unwrap_or((Stage::NoLogs, None));
    let proxy = system_proxy();
    let quic_rule = crate::quic::enabled();

    let mut checks = vec![
        Check {
            label: "Discord установлен".into(),
            ok: Some(!exes.is_empty()),
            detail: if exes.is_empty() {
                "не найден в %LOCALAPPDATA%".into()
            } else {
                format!("версий: {}", exes.len())
            },
        },
        {
            let (ok, d) = этап(&stage);
            Check {
                label: "Последний запуск".into(),
                ok,
                detail: match &when {
                    Some(w) => format!("{w} — {d}"),
                    None => d,
                },
            }
        },
        Check {
            label: "Системный прокси Windows".into(),
            ok: Some(proxy.is_none()),
            detail: match &proxy {
                None => "выключен".into(),
                Some(p) => format!(
                    "включён: {}{}",
                    p.server,
                    p.owner.as_ref().map(|o| format!(" ({o})")).unwrap_or_default()
                ),
            },
        },
        Check {
            label: "Обход".into(),
            ok: Some(bypass.is_some()),
            detail: bypass.clone().unwrap_or_else(|| "выключен".into()),
        },
    ];

    let mut failed = Vec::new();
    let upd = curl(
        &[],
        "https://updates.discord.com/distributions/app/manifests/latest?channel=stable&platform=win&arch=x64",
        15,
    );
    добавить(&mut checks, &mut failed, "Сервер обновлений Discord", upd.code == 200 && upd.bytes > 1000, ответ(&upd));

    let (code, html) = curl_body("https://discord.com/app");
    let page_ok = code == 200 && html.len() > 10_000;
    let page_detail = if code == 0 {
        "нет ответа".into()
    } else {
        format!("код {code}, {}", размер(html.len() as u64))
    };
    добавить(&mut checks, &mut failed, "Страница приложения", page_ok, page_detail);

    // Мегабайт главного скрипта: блокировку «по объёму» — обрыв после
    // 16–20 КБ — короткий запрос не видит, а Discord качает именно мегабайты.
    if let Some(path) = big_asset(&html) {
        let a = curl(&["-r", "0-1048575"], &format!("https://discord.com{path}"), 20);
        let ok = (a.code == 206 || a.code == 200) && a.bytes >= 512 * 1024;
        let detail = if !ok && a.bytes > 0 && crate::probe::code_is_answer(a.code) {
            format!("оборвалось на {} — так режут по объёму", размер(a.bytes))
        } else {
            ответ(&a)
        };
        добавить(&mut checks, &mut failed, "Загрузка файла приложения", ok, detail);
    }

    let gw = curl(
        &[
            "--http1.1",
            "-H",
            "Connection: Upgrade",
            "-H",
            "Upgrade: websocket",
            "-H",
            "Sec-WebSocket-Version: 13",
            "-H",
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
        ],
        "https://gateway.discord.gg/?v=10&encoding=json",
        6,
    );
    let gw_detail = if gw.code == 101 && gw.bytes == 0 {
        "соединение открылось, но данных не пришло".into()
    } else if gw.code == 101 {
        "подключается и отвечает".into()
    } else {
        ответ(&gw)
    };
    добавить(&mut checks, &mut failed, "Сервер сообщений", gw.code == 101 && gw.bytes > 0, gw_detail);

    checks.push(Check {
        label: "Discord без QUIC".into(),
        ok: None,
        detail: if quic_rule { "включено" } else { "выключено" }.into(),
    });

    let facts = Facts { installed: !exes.is_empty(), stage, proxy, bypass_on: bypass.is_some(), net_failed: failed, quic_rule };
    let (verdict, advice, action) = decide(&facts);
    Report { verdict, advice, action, checks }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    const ЗАВИС: &str = "[2026-09-13 22:07:24.204] [info]  splashScreenPreload: signalReady
[2026-09-13 22:07:24.212] [info]  splashScreenPreload: onStateUpdate: {\"status\":\"checking-for-updates\"}
[2026-09-13 22:07:24.412] [info]  splashScreenPreload: onStateUpdate: {\"status\":\"launching\"}";

    #[test]
    fn этап_последнего_запуска_по_логу() {
        assert_eq!(last_start(ЗАВИС), (Stage::StuckLaunching, Some("2026-09-13 22:07".into())));

        let норм = format!(
            "{ЗАВИС}\n[2026-09-14 13:36:18.100] [info]  splashScreenPreload: signalReady\n\
             [2026-09-14 13:36:41.174] [info]  [FAST CONNECT] wss://gateway.discord.gg/?encoding=etf\n\
             [2026-09-14 13:36:41.448] [info]  [FAST CONNECT] connected in 274ms"
        );
        // Смотрим только последний запуск: прежнее зависание не в счёт.
        assert_eq!(last_start(&норм), (Stage::Started { connect_ms: 274 }, Some("2026-09-14 13:36".into())));

        let gw = format!("{ЗАВИС}\n[2026-09-13 22:08:00.000] [info]  [FAST CONNECT] wss://gateway.discord.gg/");
        assert_eq!(last_start(&gw).0, Stage::GatewayStuck);

        let обновление = "[2026-09-13 21:23:14.641] [info]  splashScreenPreload: signalReady
[2026-09-13 21:23:36.121] [info]  splashScreenPreload: onStateUpdate: {\"status\":\"update-failure\",\"seconds\":3}";
        assert_eq!(last_start(обновление).0, Stage::UpdateFailed);

        assert_eq!(last_start("").0, Stage::NoLogs);
        assert_eq!(last_start("[x] что-то постороннее").0, Stage::NoLogs);
    }

    #[test]
    fn прокси_из_реестра() {
        let out = "HKEY_CURRENT_USER\\...\\Internet Settings\r\n    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    http://127.0.0.1:12334\r\n";
        assert_eq!(reg_value(out, "ProxyEnable").as_deref(), Some("0x1"));
        assert_eq!(reg_value(out, "ProxyServer").as_deref(), Some("http://127.0.0.1:12334"));
        assert_eq!(reg_value(out, "AutoConfigURL"), None);
        assert_eq!(proxy_port("http://127.0.0.1:12334"), Some(12334));
        assert_eq!(proxy_port("127.0.0.1:8080"), Some(8080));
        assert_eq!(proxy_port("http=127.0.0.1:8080;https=127.0.0.1:8443"), Some(8080));
        assert_eq!(proxy_port(""), None);
    }

    #[test]
    fn большой_скрипт_со_страницы() {
        let html = r#"<script src="/assets/101809.b361f19ddd3dfe25.js"></script><script src="/assets/web.f34f869e4c648af0.js" defer></script>"#;
        assert_eq!(big_asset(html).as_deref(), Some("/assets/web.f34f869e4c648af0.js"));
        assert_eq!(big_asset(r#"<script src="/assets/1.js">"#).as_deref(), Some("/assets/1.js"));
        assert_eq!(big_asset(r#"<a href="/assets/x.js?a=1&b">"#), None, "в путь не должно попадать лишнее");
        assert_eq!(big_asset(""), None);
    }

    #[test]
    fn ответ_curl() {
        assert_eq!(parse_answer("206 1048576 0.549"), Answer { code: 206, bytes: 1_048_576, secs: 0.549 });
        assert_eq!(parse_answer(""), Answer::default());
    }

    /// Живая проверка на этой машине: логи, реестр, netstat, curl.
    /// `cargo test --lib живая_проверка_discord -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn живая_проверка_discord() {
        let r = run(Some("живой тест".into()));
        println!("ВЕРДИКТ: {}\nСОВЕТ: {}\nДЕЙСТВИЕ: {:?}", r.verdict, r.advice, r.action);
        for c in &r.checks {
            println!("  [{}] {} — {}", c.ok.map(|o| if o { "ok" } else { "!!" }).unwrap_or(".."), c.label, c.detail);
        }
    }

    fn факты() -> Facts {
        Facts {
            installed: true,
            stage: Stage::StuckLaunching,
            proxy: None,
            bypass_on: true,
            net_failed: Vec::new(),
            quic_rule: false,
        }
    }

    #[test]
    fn вердикт_по_порядку() {
        // Сеть в порядке, Discord висит — предлагаем закрыть QUIC.
        assert_eq!(decide(&факты()).2, Action::EnableNoQuic);
        // QUIC уже закрыт — остаётся кэш.
        assert_eq!(decide(&Facts { quic_rule: true, ..факты() }).2, Action::ClearCache);
        // Не проходит сеть — дело в стратегии, про QUIC говорить рано.
        let режут = Facts { net_failed: vec!["Сервер сообщений".into()], ..факты() };
        assert_eq!(decide(&режут).2, Action::PickStrategy);
        // Обход выключен — сначала включить.
        assert_eq!(decide(&Facts { bypass_on: false, ..режут }).2, Action::EnableBypass);
        // Прокси делает всё остальное бессмысленным — и называем, чей он.
        let через_прокси = Facts {
            proxy: Some(Proxy { server: "http://127.0.0.1:12334".into(), owner: Some("Hiddify.exe".into()) }),
            ..факты()
        };
        let (verdict, _, action) = decide(&через_прокси);
        assert!(verdict.contains("Hiddify.exe"), "{verdict}");
        assert_eq!(action, Action::None);
        // Запустился нормально и сеть в порядке — чинить нечего.
        assert_eq!(decide(&Facts { stage: Stage::Started { connect_ms: 274 }, ..факты() }).2, Action::None);
        assert_eq!(decide(&Facts { installed: false, ..факты() }).0, "Discord не найден");
    }
}
