//! Экран «Списки»: что обход трогает, а что нет — и почему.
//!
//! Списки — это файлы в папке релиза, и понять, почему конкретный сайт не
//! открывается при включённом обходе, раньше можно было только прочитав .bat
//! и сверив его с файлами руками. Здесь это делает Klutz: разбирает строку
//! запуска текущего конфига на правила (`--new`) и смотрит, какое из них
//! поймает сайт, — если хоть одно поймает.

use serde::Serialize;
use std::fs;
use std::net::IpAddr;
use std::path::Path;

use crate::probe::PathVerdict;
use crate::targets::TargetResult;

/// Файл релиза и что он значит для человека.
pub struct Known {
    pub file: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    /// «Не трогать», а не «обходить».
    pub exclude: bool,
    /// Адреса и сети, а не имена сайтов.
    pub ips: bool,
    /// Свой список человека: обновление релиза его не затирает.
    pub user: bool,
}

pub const KNOWN: &[Known] = &[
    Known {
        file: "list-general.txt",
        title: "Сайты из релиза",
        desc: "Discord и другие сайты, которые собрал автор релиза. Обход узнаёт их по имени сайта.",
        exclude: false,
        ips: false,
        user: false,
    },
    Known {
        file: "list-google.txt",
        title: "YouTube",
        desc: "Домены YouTube и Google — у них своё правило со своими настройками.",
        exclude: false,
        ips: false,
        user: false,
    },
    Known {
        file: "list-general-user.txt",
        title: "Твои сайты",
        desc: "Что ты добавил сам. Обновление релиза их не затирает — Klutz переносит.",
        exclude: false,
        ips: false,
        user: true,
    },
    Known {
        file: "ipset-all.txt",
        title: "По адресам (IPSet)",
        desc: "Адреса без имени сайта: серверы игр, голос. Работает в режиме «загружен список».",
        exclude: false,
        ips: true,
        user: false,
    },
    Known {
        file: "list-exclude.txt",
        title: "Не трогать: из релиза",
        desc: "Сайты, которые обход пропускает всегда, даже если они попали в другие списки.",
        exclude: true,
        ips: false,
        user: false,
    },
    Known {
        file: "list-exclude-user.txt",
        title: "Не трогать: твои",
        desc: "Сайты, которые ты сам попросил не трогать.",
        exclude: true,
        ips: false,
        user: true,
    },
    Known {
        file: "ipset-exclude.txt",
        title: "Не трогать: адреса из релиза",
        desc: "Адреса и сети, которые обход пропускает всегда.",
        exclude: true,
        ips: true,
        user: false,
    },
    Known {
        file: "ipset-exclude-user.txt",
        title: "Не трогать: твои адреса",
        desc: "Адреса, которые ты сам исключил, — например, игровые, которым обход мешал.",
        exclude: true,
        ips: true,
        user: true,
    },
];

/// Заглушки, которые кладут в пустые файлы: записями они не считаются.
const PLACEHOLDERS: &[&str] = &["domain.example.abc", "203.0.113.113/32"];

pub fn entries(text: &str) -> Vec<String> {
    text.lines()
        // BOM от Блокнота в начале файла — не часть первой записи.
        .map(|l| l.trim().trim_start_matches('\u{feff}').trim().to_lowercase())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !PLACEHOLDERS.contains(&l.as_str()))
        .collect()
}

fn read_entries(path: &Path) -> Vec<String> {
    fs::read_to_string(path).map(|t| entries(&t)).unwrap_or_default()
}

fn title_of(file_name: &str) -> String {
    KNOWN
        .iter()
        .find(|k| k.file.eq_ignore_ascii_case(file_name))
        .map(|k| format!("«{}» ({file_name})", k.title))
        .unwrap_or_else(|| file_name.to_string())
}

// ─────────── сводка ───────────

#[derive(Debug, Serialize)]
pub struct ListInfo {
    pub file: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    pub exclude: bool,
    pub ips: bool,
    pub user: bool,
    pub count: usize,
    pub exists: bool,
}

#[derive(Debug, Serialize)]
pub struct Overview {
    pub lists: Vec<ListInfo>,
    #[serde(rename = "ipsetMode")]
    pub ipset_mode: String,
    /// Сколько сетей игр собрано в IPSet.
    #[serde(rename = "gameNets")]
    pub game_nets: usize,
}

pub fn overview(root: &Path) -> Overview {
    let dir = root.join("lists");
    let lists = KNOWN
        .iter()
        .map(|k| {
            let p = dir.join(k.file);
            ListInfo {
                file: k.file,
                title: k.title,
                desc: k.desc,
                exclude: k.exclude,
                ips: k.ips,
                user: k.user,
                count: read_entries(&p).len(),
                exists: p.exists(),
            }
        })
        .collect();
    let ipset_mode = fs::read_to_string(dir.join("ipset-all.txt"))
        .map(|c| crate::toggles::ipset_mode_from(&c))
        .unwrap_or_else(|_| "none".into());
    Overview { lists, ipset_mode, game_nets: crate::gamescan::saved_ips(root).len() }
}

// ─────────── сопоставление ───────────

/// Запись списка покрывает сам домен и все его поддомены — так сравнивает
/// zapret. Запись с `^` — только сам домен.
pub fn host_matches(entry: &str, host: &str) -> bool {
    if let Some(exact) = entry.strip_prefix('^') {
        return host == exact;
    }
    host == entry || host.ends_with(&format!(".{entry}"))
}

/// Входит ли адрес в сеть `a.b.c.d/n` (или равен адресу без маски).
pub fn net_contains(net: &str, ip: IpAddr) -> bool {
    let (addr, len) = match net.split_once('/') {
        Some((a, l)) => (a, l.parse::<u32>().ok()),
        None => (net, None),
    };
    match (addr.parse::<IpAddr>(), ip) {
        (Ok(IpAddr::V4(a)), IpAddr::V4(b)) => {
            let l = len.unwrap_or(32).min(32);
            let mask = if l == 0 { 0 } else { u32::MAX << (32 - l) };
            (u32::from(a) & mask) == (u32::from(b) & mask)
        }
        (Ok(IpAddr::V6(a)), IpAddr::V6(b)) => {
            let l = len.unwrap_or(128).min(128);
            let mask = if l == 0 { 0 } else { u128::MAX << (128 - l) };
            (u128::from(a) & mask) == (u128::from(b) & mask)
        }
        _ => false,
    }
}

/// `80,443,1024-65535` — есть ли среди них порт.
pub fn port_in(spec: &str, port: u16) -> bool {
    spec.split(',').any(|p| match p.split_once('-') {
        Some((a, b)) => matches!((a.trim().parse::<u16>(), b.trim().parse::<u16>()), (Ok(a), Ok(b)) if a <= port && port <= b),
        None => p.trim().parse::<u16>() == Ok(port),
    })
}

pub fn split_profiles(args: &[String]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for a in args {
        if a == "--new" {
            out.push(Vec::new());
        } else if let Some(p) = out.last_mut() {
            p.push(a.clone());
        }
    }
    out.retain(|p| !p.is_empty());
    out
}

fn values<'a>(p: &'a [String], key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    p.iter().filter_map(move |t| t.strip_prefix(key))
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// Первое правило конфига, которое поймает HTTPS на этот сайт: порт 443 по
/// TCP, имя в его списках (если они есть), адрес в его IPSet (если он есть),
/// и не в его исключениях. zapret берёт первое подходящее. `read` читает
/// файл списка — снаружи, чтобы правило проверялось тестами без файлов.
pub fn rule_for(
    profiles: &[Vec<String>],
    host: &str,
    ip: Option<IpAddr>,
    read: &dyn Fn(&str) -> Vec<String>,
) -> Option<(usize, String)> {
    for (i, p) in profiles.iter().enumerate() {
        let tcp = values(p, "--filter-tcp=").next();
        let udp = values(p, "--filter-udp=").next();
        match tcp {
            Some(t) if !port_in(t, 443) => continue,
            None if udp.is_some() => continue,
            _ => {}
        }
        if let Some(l7) = values(p, "--filter-l7=").next() {
            if !l7.split(',').any(|x| x == "tls" || x == "http") {
                continue;
            }
        }

        let hostlists: Vec<&str> = values(p, "--hostlist=").collect();
        let domains: Vec<&str> = values(p, "--hostlist-domains=").flat_map(|d| d.split(',')).collect();
        let by_name = !hostlists.is_empty() || !domains.is_empty();
        if by_name {
            let hit = hostlists.iter().any(|f| read(f).iter().any(|e| host_matches(e, host)))
                || domains.iter().any(|d| host_matches(&d.to_lowercase(), host));
            if !hit {
                continue;
            }
        }
        let excluded_name = values(p, "--hostlist-exclude=").any(|f| read(f).iter().any(|e| host_matches(e, host)))
            || values(p, "--hostlist-exclude-domains=")
                .flat_map(|d| d.split(','))
                .any(|d| host_matches(&d.to_lowercase(), host));
        if excluded_name {
            continue;
        }

        let ipsets: Vec<&str> = values(p, "--ipset=").collect();
        let by_ip = !ipsets.is_empty();
        if by_ip {
            // Пустой список (режим «любые IP») для zapret значит «все адреса».
            let hit = ipsets.iter().any(|f| {
                let e = read(f);
                e.is_empty() || ip.is_some_and(|ip| e.iter().any(|n| net_contains(n, ip)))
            });
            if !hit {
                continue;
            }
        }
        if let Some(ip) = ip {
            if values(p, "--ipset-exclude=").any(|f| read(f).iter().any(|n| net_contains(n, ip))) {
                continue;
            }
        }

        let ports = tcp.unwrap_or("все порты");
        let by = if by_name {
            let mut names: Vec<String> = hostlists.iter().map(|f| title_of(&file_name(f))).collect();
            if !domains.is_empty() {
                names.push(domains.join(", "));
            }
            format!("по имени — {}", names.join(", "))
        } else if by_ip {
            "по адресу — IPSet".to_string()
        } else {
            "для всех сайтов".to_string()
        };
        return Some((i, format!("правило {} из {}: TCP {ports}, {by}", i + 1, profiles.len())));
    }
    None
}

// ─────────── вывод ───────────

pub struct Inputs<'a> {
    pub excluded_by: &'a [String],
    /// Попадает ли под обход: по правилу конфига, а без конфига — по спискам.
    pub covered: bool,
    pub running: bool,
    pub reach_ok: Option<bool>,
    pub path: Option<PathVerdict>,
}

/// Вывод, совет и какую кнопку показать: (вывод, совет, «добавить в обход»).
pub fn decide(i: &Inputs) -> (String, String, bool) {
    if !i.excluded_by.is_empty() {
        return (
            "Этот сайт обход не трогает".into(),
            format!(
                "Он в списке «не трогать»: {}. Так делают с сайтами, которым обход мешает. Если сайт не открывается — убери его оттуда.",
                i.excluded_by.join(", ")
            ),
            false,
        );
    }
    if !i.covered {
        return if i.reach_ok == Some(true) {
            ("Открывается и без обхода".into(), "Обход этот сайт не трогает — и ему это не нужно.".into(), false)
        } else {
            (
                "Обход этот сайт не трогает".into(),
                "Его нет ни в одном списке, по которому работает обход. Добавь его в обход и перезапусти обход.".into(),
                true,
            )
        };
    }
    if !i.running {
        return (
            "Попадает под обход, но обход выключен".into(),
            "Включи обход на Главной — и сайт пойдёт через него.".into(),
            false,
        );
    }
    if i.reach_ok == Some(true) {
        return ("Под обходом и открывается".into(), "Всё в порядке.".into(), false);
    }
    let advice = match i.path {
        Some(PathVerdict::Sni) => {
            "Режут по имени сайта, а текущий вариант обхода это не пробивает. Попробуй подобрать другой вариант на Главной."
        }
        Some(PathVerdict::Ip) => "Режут адрес целиком — от этого стратегии обхода не спасают.",
        Some(PathVerdict::Server) | Some(PathVerdict::Legal) => {
            "Отвечает не сам сервер (451 или чужой сертификат) — это не блокировка DPI, и обход тут не поможет."
        }
        Some(PathVerdict::Cutoff) => "Соединение пускают, но обрывают по дороге — нужен другой вариант обхода.",
        _ => "Попробуй другой вариант обхода или «Как у меня режут» на Диагностике.",
    };
    ("Под обходом, но не открывается".into(), advice.into(), false)
}

#[derive(Debug, Serialize)]
pub struct SiteCheck {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub host: String,
    /// Списки «обходить», где есть сайт.
    #[serde(rename = "inLists")]
    pub in_lists: Vec<String>,
    /// Списки «не трогать», где есть сайт или его адрес.
    #[serde(rename = "excludedBy")]
    pub excluded_by: Vec<String>,
    pub ip: Option<String>,
    #[serde(rename = "ipInIpset")]
    pub ip_in_ipset: bool,
    /// Правило текущего конфига, которое его поймает.
    pub rule: Option<String>,
    pub config: Option<String>,
    pub reach: Option<TargetResult>,
    pub verdict: String,
    pub advice: String,
    #[serde(rename = "canAdd")]
    pub can_add: bool,
    /// Сайт в твоём «не трогать» — его можно оттуда убрать.
    #[serde(rename = "canUnexclude")]
    pub can_unexclude: bool,
}

impl SiteCheck {
    pub fn failed(host: &str, e: &str) -> Self {
        SiteCheck {
            ok: false,
            error: Some(e.into()),
            host: host.into(),
            in_lists: Vec::new(),
            excluded_by: Vec::new(),
            ip: None,
            ip_in_ipset: false,
            rule: None,
            config: None,
            reach: None,
            verdict: String::new(),
            advice: String::new(),
            can_add: false,
            can_unexclude: false,
        }
    }
}

/// Имя сайта из того, что ввели: ссылка, домен, с портом — неважно.
pub fn site_host(input: &str) -> Result<String, String> {
    crate::maintenance::clean_domain(input)
        .filter(|h| !h.starts_with('#') && h.contains('.') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .ok_or_else(|| "Это не похоже на адрес сайта — нужно что-то вроде rutracker.org.".to_string())
}

pub fn check(root: &Path, active: Option<&str>, running: bool, input: &str) -> SiteCheck {
    let host = match site_host(input) {
        Ok(h) => h,
        Err(e) => return SiteCheck::failed(input.trim(), &e),
    };
    let dir = root.join("lists");
    let mut in_lists = Vec::new();
    let mut excluded_by = Vec::new();
    let mut can_unexclude = false;
    for k in KNOWN.iter().filter(|k| !k.ips) {
        if read_entries(&dir.join(k.file)).iter().any(|e| host_matches(e, &host)) {
            if k.exclude {
                excluded_by.push(k.title.to_string());
                can_unexclude |= k.user;
            } else {
                in_lists.push(k.title.to_string());
            }
        }
    }

    // Адрес — IPv4, если есть: по нему сверяются списки адресов.
    let ips = crate::probe::resolve_ips(&host, 443);
    let ip = ips
        .iter()
        .find(|i| i.parse::<std::net::Ipv4Addr>().is_ok())
        .or(ips.first())
        .and_then(|i| i.parse::<IpAddr>().ok());
    let mut ip_in_ipset = false;
    if let Some(ip) = ip {
        for k in KNOWN.iter().filter(|k| k.ips) {
            if read_entries(&dir.join(k.file)).iter().any(|n| net_contains(n, ip)) {
                if k.exclude {
                    excluded_by.push(k.title.to_string());
                } else {
                    ip_in_ipset = true;
                }
            }
        }
    }

    // Правило текущего конфига. Файлы списков он называет полными путями.
    let rule = active.and_then(|cfg| {
        let args = crate::winws::extract_winws_args(root, cfg)?;
        let read = |path: &str| read_entries(Path::new(path));
        rule_for(&split_profiles(&args), &host, ip, &read).map(|(_, text)| text)
    });
    let covered = match active {
        Some(_) => rule.is_some(),
        None => !in_lists.is_empty() || ip_in_ipset,
    };

    let reach = crate::targets::check_targets(&[crate::targets::Target {
        name: host.clone(),
        host: host.clone(),
        port: 443,
    }])
    .into_iter()
    .next();

    let (verdict, advice, can_add) = decide(&Inputs {
        excluded_by: &excluded_by,
        covered,
        running,
        reach_ok: reach.as_ref().map(|r| r.ok),
        path: reach.as_ref().map(|r| r.verdict),
    });
    SiteCheck {
        ok: true,
        error: None,
        host,
        in_lists,
        excluded_by,
        ip: ip.map(|i| i.to_string()),
        ip_in_ipset,
        rule,
        config: active.map(String::from),
        reach,
        verdict,
        advice,
        can_add,
        can_unexclude,
    }
}

// ─────────── правка своих списков ───────────

fn without_placeholders(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !PLACEHOLDERS.contains(&l.to_lowercase().as_str()))
        .collect()
}

/// Добавляет сайт в твой список «обходить» или «не трогать» — и убирает его
/// из противоположного: лежать в обоих сразу бессмысленно.
pub fn add(root: &Path, input: &str, exclude: bool) -> Result<String, String> {
    let host = site_host(input)?;
    let cur = crate::maintenance::get_custom_lists(root);
    let mut inc = without_placeholders(&cur.include);
    let mut exc = without_placeholders(&cur.exclude);
    let (to, from) = if exclude { (&mut exc, &mut inc) } else { (&mut inc, &mut exc) };
    from.retain(|e| !host_matches(&e.to_lowercase(), &host));
    if !to.iter().any(|e| host_matches(&e.to_lowercase(), &host)) {
        to.push(host.clone());
    }
    crate::maintenance::save_custom_lists(root, &inc.join("\n"), &exc.join("\n"))?;
    Ok(host)
}

/// Убирает из твоего «не трогать» всё, что покрывает этот сайт.
pub fn unexclude(root: &Path, input: &str) -> Result<String, String> {
    let host = site_host(input)?;
    let cur = crate::maintenance::get_custom_lists(root);
    let mut exc = without_placeholders(&cur.exclude);
    exc.retain(|e| !host_matches(&e.to_lowercase(), &host));
    crate::maintenance::save_custom_lists(root, &cur.include, &exc.join("\n"))?;
    Ok(host)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn поддомены_и_точное_совпадение() {
        assert!(host_matches("discord.com", "discord.com"));
        assert!(host_matches("discord.com", "cdn.discord.com"));
        assert!(!host_matches("discord.com", "notdiscord.com"));
        assert!(host_matches("^discord.com", "discord.com"));
        assert!(!host_matches("^discord.com", "cdn.discord.com"));
    }

    #[test]
    fn адрес_в_сети() {
        let ip: IpAddr = "162.159.130.233".parse().unwrap();
        assert!(net_contains("162.159.128.0/19", ip));
        assert!(!net_contains("162.159.0.0/24", ip));
        assert!(net_contains("162.159.130.233", ip));
        assert!(net_contains("0.0.0.0/0", ip));
        let v6: IpAddr = "2606:4700::6810:84e5".parse().unwrap();
        assert!(net_contains("2606:4700::/32", v6));
        assert!(!net_contains("2606:4700::/32", ip));
        assert!(!net_contains("мусор", ip));
    }

    #[test]
    fn bom_блокнота_не_часть_записи() {
        assert_eq!(entries("\u{feff}Discord.com\r\n# c\r\ndomain.example.abc\r\n"), vec!["discord.com"]);
    }

    #[test]
    fn порты() {
        assert!(port_in("80,443", 443));
        assert!(port_in("1024-65535", 2053));
        assert!(!port_in("2053,2083", 443));
        assert!(!port_in("12", 443));
    }

    /// Правила как в ALT11 у Flowseal: QUIC, голос, discord.media, YouTube,
    /// общий список, IPSet.
    fn alt11() -> Vec<Vec<String>> {
        split_profiles(&args(
            "--wf-tcp=80,443 --filter-udp=443 --hostlist=L/list-general.txt \
             --new --filter-udp=19294-19344,50000-50100 --filter-l7=discord,stun \
             --new --filter-tcp=2053,2083,2087,2096,8443 --hostlist-domains=discord.media \
             --new --filter-tcp=443 --hostlist=L/list-google.txt \
             --new --filter-tcp=80,443 --hostlist=L/list-general.txt --hostlist=L/list-general-user.txt \
                   --hostlist-exclude=L/list-exclude.txt --hostlist-exclude=L/list-exclude-user.txt \
             --new --filter-tcp=80,443,8443 --ipset=L/ipset-all.txt --hostlist-exclude=L/list-exclude.txt \
                   --ipset-exclude=L/ipset-exclude.txt",
        ))
    }

    fn files(ipset: &'static [&'static str]) -> impl Fn(&str) -> Vec<String> {
        move |path: &str| {
            let v: &[&str] = match file_name(path).as_str() {
                "list-general.txt" => &["discord.com", "discord.gg"],
                "list-google.txt" => &["youtube.com", "googlevideo.com"],
                "list-general-user.txt" => &["rutracker.org"],
                "list-exclude.txt" => &["gosuslugi.ru"],
                "ipset-all.txt" => ipset,
                _ => &[],
            };
            v.iter().map(|s| s.to_string()).collect()
        }
    }

    #[test]
    fn первое_подходящее_правило() {
        let read = files(&["10.0.0.0/8"]);
        let p = alt11();
        // YouTube ловит своё правило, а не общее ниже.
        let (i, text) = rule_for(&p, "www.youtube.com", None, &read).unwrap();
        assert_eq!(i, 3);
        assert!(text.contains("YouTube"), "{text}");
        // Свой сайт — общее правило, через «Твои сайты».
        assert_eq!(rule_for(&p, "rutracker.org", None, &read).unwrap().0, 4);
        // Исключённый по имени — ни общее, ни IPSet.
        assert_eq!(rule_for(&p, "gosuslugi.ru", "10.1.2.3".parse().ok(), &read), None);
        // Нет в списках, но адрес в IPSet — правило по адресу.
        let (i, text) = rule_for(&p, "example.org", "10.1.2.3".parse().ok(), &read).unwrap();
        assert_eq!(i, 5);
        assert!(text.contains("IPSet"), "{text}");
        // Нигде — никто.
        assert_eq!(rule_for(&p, "example.org", "8.8.8.8".parse().ok(), &read), None);
    }

    #[test]
    fn пустой_ipset_значит_все_адреса() {
        let read = files(&[]);
        assert_eq!(rule_for(&alt11(), "example.org", "8.8.8.8".parse().ok(), &read).unwrap().0, 5);
    }

    #[test]
    fn выводы() {
        let none: Vec<String> = Vec::new();
        let base = |covered, running, reach_ok, path| Inputs { excluded_by: &none, covered, running, reach_ok, path };
        // Не в списках и не открывается — предложить добавить.
        let (v, _, add) = decide(&base(false, true, Some(false), Some(PathVerdict::Sni)));
        assert!(v.contains("не трогает") && add);
        // Не в списках, но открывается — добавлять незачем.
        let (v, _, add) = decide(&base(false, true, Some(true), Some(PathVerdict::Ok)));
        assert!(v.contains("без обхода") && !add);
        // Под обходом, но режут по имени — другой вариант.
        let (v, a, _) = decide(&base(true, true, Some(false), Some(PathVerdict::Sni)));
        assert!(v.contains("не открывается") && a.contains("другой вариант"), "{a}");
        // Под обходом, но обход выключен.
        assert!(decide(&base(true, false, Some(false), None)).0.contains("выключен"));
        // В «не трогать» — главное это.
        let ex = vec!["Не трогать: из релиза".to_string()];
        let i = Inputs { excluded_by: &ex, covered: true, running: true, reach_ok: Some(false), path: None };
        assert!(decide(&i).1.contains("Не трогать: из релиза"));
    }

    #[test]
    fn адрес_сайта_из_ввода() {
        assert_eq!(site_host("https://RuTracker.org/forum/index.php").unwrap(), "rutracker.org");
        assert!(site_host("просто текст").is_err());
        assert!(site_host("localhost").is_err());
    }

    #[test]
    fn добавить_и_убрать_в_своих_списках() {
        let dir = std::env::temp_dir().join(format!("klutz-lists-add-{}", std::process::id()));
        let lists = dir.join("lists");
        let _ = fs::create_dir_all(&lists);
        crate::maintenance::ensure_user_lists(&dir);

        assert_eq!(add(&dir, "https://rutracker.org/forum", false).unwrap(), "rutracker.org");
        let inc = fs::read_to_string(lists.join("list-general-user.txt")).unwrap();
        assert_eq!(inc, "rutracker.org\n", "заглушка ушла, сайт добавлен");

        // В «не трогать» — и из «обходить» он уходит.
        add(&dir, "rutracker.org", true).unwrap();
        assert!(!fs::read_to_string(lists.join("list-general-user.txt")).unwrap().contains("rutracker"));
        assert!(fs::read_to_string(lists.join("list-exclude-user.txt")).unwrap().contains("rutracker.org"));

        unexclude(&dir, "rutracker.org").unwrap();
        assert!(!fs::read_to_string(lists.join("list-exclude-user.txt")).unwrap().contains("rutracker"));
        let _ = fs::remove_dir_all(&dir);
    }
}
