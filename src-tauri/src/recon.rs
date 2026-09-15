//! «Как у меня режут»: разведка сети без обхода.
//!
//! Первый этап своего blockcheck. Прежде чем перебирать стратегии, надо
//! понять, против чего: подмену DNS и блок по адресу стратегии zapret не
//! лечат вовсе, блок по имени — их прямая работа, а обрыв по объёму
//! короткими тестами даже не виден. По каждой цели Discord и YouTube:
//!
//! - режут ли по имени или по адресу — проба с контролем, как в мониторинге;
//! - если по имени — проходит ли разрезанный ClientHello;
//! - если открылось — не обрывают ли поток после первых килобайт;
//! - если не открылось иначе — не подменяет ли провайдер DNS: сверяем с
//!   зашифрованным DNS и стучимся на настоящий адрес.
//!
//! Меряется сеть как есть, поэтому обход в это время выключен, а VPN
//! проверяется заранее (`vpncheck`).

use serde::Serialize;
use std::process::Command;
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::probe::{PathVerdict, VolumeVerdict};
use crate::targets::Target;
use crate::tlsprobe::FragVerdict;
use crate::udpprobe::{UdpResult, UdpVerdict};
use crate::vpncheck::VpnCheck;

/// Сервисы, которые разведываем, — по началу имени цели.
pub const GROUPS: &[&str] = &["Discord", "YouTube"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Clear,
    Dns,
    Ip,
    Sni,
    Cutoff,
    /// 451 или чужой сертификат: ответил не сервер, но и не DPI.
    NotDpi,
    Unknown,
}

impl Kind {
    pub fn short(self) -> &'static str {
        match self {
            Kind::Clear => "не режут",
            Kind::Dns => "подмена DNS",
            Kind::Ip => "режут адрес",
            Kind::Sni => "режут по имени",
            Kind::Cutoff => "обрыв по объёму",
            Kind::NotDpi => "отвечает не сервер",
            Kind::Unknown => "не проверено",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetRecon {
    pub name: String,
    pub host: String,
    pub kind: Kind,
    pub label: &'static str,
    /// Проходит ли разрез — только для блока по имени.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<FragVerdict>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recon {
    /// `ok`, `vpn` — мешает VPN или прокси, `bypass_running` — обход не
    /// выключен, `busy` — идут тесты или сбор адресов игры.
    pub status: &'static str,
    pub vpn: VpnCheck,
    pub message: String,
    pub targets: Vec<TargetRecon>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp: Option<UdpResult>,
    pub verdict: String,
    pub advice: Vec<String>,
}

impl Recon {
    /// Разведка не началась — объяснить почему.
    pub fn stopped(status: &'static str, message: String, vpn: VpnCheck) -> Self {
        Recon { status, vpn, message, targets: Vec::new(), udp: None, verdict: String::new(), advice: Vec::new() }
    }
}

/// Всё измеренное по одной цели — вход чистого правила.
pub struct Facts {
    pub path: PathVerdict,
    pub why: Option<String>,
    pub dns_spoofed: bool,
    pub split: Option<(FragVerdict, String)>,
    pub volume: Option<(VolumeVerdict, String)>,
}

pub fn classify(f: &Facts) -> (Kind, String) {
    if f.dns_spoofed {
        return (
            Kind::Dns,
            "адрес от DNS провайдера не отвечает, а адрес от зашифрованного DNS отвечает и без обхода — \
             провайдер подменяет DNS"
                .into(),
        );
    }
    let why = f.why.clone().unwrap_or_default();
    match f.path {
        PathVerdict::Ok => match &f.volume {
            Some((VolumeVerdict::Cutoff, note)) => (Kind::Cutoff, note.clone()),
            _ => (Kind::Clear, "открывается без обхода".into()),
        },
        PathVerdict::Sni => {
            let detail = match (&f.split, why.is_empty()) {
                (Some((_, note)), true) => note.clone(),
                (Some((_, note)), false) => format!("{why}; {note}"),
                (None, _) => why,
            };
            (Kind::Sni, detail)
        }
        PathVerdict::Cutoff => (Kind::Cutoff, why),
        PathVerdict::Ip => (Kind::Ip, why),
        PathVerdict::Legal | PathVerdict::Server => (Kind::NotDpi, why),
        PathVerdict::Unknown => {
            (Kind::Unknown, if why.is_empty() { "проверить не удалось".into() } else { why })
        }
    }
}

/// Адреса из ответа DNS-over-HTTPS в формате JSON (Cloudflare и Google
/// отвечают одинаково). Берём только записи A: CNAME в ответе тоже есть.
pub fn parse_doh(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let Some(answers) = v.get("Answer").and_then(|a| a.as_array()) else { return Vec::new() };
    answers
        .iter()
        .filter(|r| r.get("type").and_then(|t| t.as_u64()) == Some(1))
        .filter_map(|r| r.get("data")?.as_str())
        .filter(|s| s.parse::<std::net::Ipv4Addr>().is_ok())
        .map(String::from)
        .collect()
}

/// Разошлись ли ответы. Само расхождение ещё не подмена: CDN раздают разные
/// адреса разным резолверам. Подменой его делает только проба на настоящий
/// адрес — это решает вызывающий.
pub fn dns_differs(system: &[String], doh: &[String]) -> bool {
    !doh.is_empty() && !system.iter().any(|s| doh.contains(s))
}

pub fn summarize(targets: &[TargetRecon], udp_blocked: bool) -> (String, Vec<String>) {
    let all_clear = !targets.is_empty() && targets.iter().all(|t| t.kind == Kind::Clear);

    let mut phrases = Vec::new();
    for g in GROUPS {
        let mine: Vec<&TargetRecon> = targets.iter().filter(|t| t.name.starts_with(g)).collect();
        if mine.is_empty() {
            continue;
        }
        let mut bad: Vec<Kind> = Vec::new();
        for t in &mine {
            if t.kind != Kind::Clear && !bad.contains(&t.kind) {
                bad.push(t.kind);
            }
        }
        if bad.is_empty() {
            phrases.push(format!("{g} не режут"));
            continue;
        }
        let list = bad.iter().map(|k| k.short()).collect::<Vec<_>>().join(", ");
        if mine.iter().any(|t| t.kind == Kind::Clear) {
            phrases.push(format!("{g} — частично: {list}"));
        } else {
            phrases.push(format!("{g} — {list}"));
        }
    }
    let verdict = if targets.is_empty() {
        "Проверить не удалось".to_string()
    } else if all_clear {
        "Discord и YouTube здесь не режут".to_string()
    } else {
        phrases.join("; ")
    };

    let has = |k: Kind| targets.iter().any(|t| t.kind == k);
    let mut advice: Vec<String> = Vec::new();
    if has(Kind::Dns) {
        advice.push(
            "Провайдер подменяет адреса в DNS. Помогает зашифрованный DNS: в параметрах сети Windows укажи \
             DNS 1.1.1.1 и включи для него шифрование (DNS over HTTPS). Klutz системные настройки сам не меняет."
                .into(),
        );
    }
    if has(Kind::Ip) {
        advice.push(
            "Часть адресов режут целиком — от этого стратегии zapret не спасают: пакеты до сервера не доходят \
             вовсе. Здесь поможет только туннель."
                .into(),
        );
    }
    if has(Kind::Sni) {
        let splits: Vec<FragVerdict> =
            targets.iter().filter(|t| t.kind == Kind::Sni).filter_map(|t| t.split).collect();
        advice.push(if splits.contains(&FragVerdict::Helps) {
            "Режут по имени сайта, и разрезанный запрос проходит — ровно то, что лечит zapret. Подбор стратегии \
             под этот ПК здесь имеет смысл."
                .into()
        } else if splits.contains(&FragVerdict::DoesNotHelp) {
            "Режут по имени сайта, но простой разрез запроса не проходит: провайдер собирает куски обратно. \
             Стратегии на одном разрезе не помогут — надежда на приёмы с поддельными пакетами (fake)."
                .into()
        } else {
            "Режут по имени сайта — против этого стратегии zapret и сделаны.".into()
        });
    }
    if has(Kind::Cutoff) {
        advice.push(
            "Соединение пускают, но обрывают после первых килобайт. Такое обходится хуже всего, и короткие \
             тесты его не видят: стратегию надо проверять на объёме."
                .into(),
        );
    }
    if has(Kind::NotDpi) {
        advice.push(
            "У части целей отвечает не сам сервер (451 или чужой сертификат) — это не DPI, и стратегия обхода \
             тут не поможет."
                .into(),
        );
    }
    if has(Kind::Unknown) {
        advice.push("Часть проверок не дала ответа — повтори чуть позже.".into());
    }
    if udp_blocked {
        advice.push("UDP наружу не выпускают — голос Discord и QUIC не заработают ни с каким конфигом.".into());
    }
    if all_clear && !udp_blocked {
        advice.push(
            "Похоже, обход здесь не нужен. Если Discord всё равно не запускается — дело не в блокировке: \
             нажми «Почему не запускается»."
                .into(),
        );
    }
    (verdict, advice)
}

// ─────────── сеть ───────────

/// DNS-over-HTTPS по адресу, а не по имени: имя резолвера провайдер тоже
/// может подменить. Сертификаты 1.1.1.1 и 8.8.8.8 выписаны и на сами адреса.
const DOH: &[&str] = &["https://1.1.1.1/dns-query", "https://8.8.8.8/resolve"];

fn doh_ips(host: &str) -> Vec<String> {
    for base in DOH {
        let mut cmd = Command::new(crate::sys::system_exe("curl.exe"));
        cmd.args(["-s", "--noproxy", "*", "--connect-timeout", "4", "-m", "6", "-H", "accept: application/dns-json"])
            .arg(format!("{base}?name={host}&type=A"));
        #[cfg(target_os = "windows")]
        cmd.creation_flags(crate::sys::CREATE_NO_WINDOW);
        let ips = cmd
            .output()
            .map(|o| parse_doh(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
        if !ips.is_empty() {
            return ips;
        }
    }
    Vec::new()
}

fn recon_target(t: &Target) -> TargetRecon {
    let res = crate::targets::check_targets(std::slice::from_ref(t)).into_iter().next();
    let Some(res) = res else {
        return TargetRecon {
            name: t.name.clone(),
            host: t.host.clone(),
            kind: Kind::Unknown,
            label: Kind::Unknown.short(),
            split: None,
            detail: "проверка не выполнилась".into(),
        };
    };

    let mut facts = Facts { path: res.verdict, why: res.why, dns_spoofed: false, split: None, volume: None };
    match res.verdict {
        PathVerdict::Ok => {
            let v = crate::probe::http_probe_volume(&t.host, t.port, None, 10);
            facts.volume = Some((v.verdict, v.note));
        }
        PathVerdict::Sni => {
            let ip = crate::probe::first_ip(&t.host, t.port).and_then(|ip| ip.parse().ok());
            if let Some(ip) = ip {
                let f = crate::tlsprobe::probe_fragmentation(ip, t.port, &t.host, Duration::from_secs(3));
                facts.split = Some((f.verdict, f.note));
            }
        }
        _ => {
            // Подменённый DNS выглядит как угодно — молчание, чужой
            // сертификат, 451 со страницы-заглушки. Поэтому проверяем его на
            // всех провалах, кроме блока по имени: там контроль уже доказал,
            // что адрес настоящий и живой.
            let system = crate::probe::resolve_ips(&t.host, t.port);
            let doh = doh_ips(&t.host);
            if dns_differs(&system, &doh) {
                if let Some(ip) = doh.first() {
                    facts.dns_spoofed = crate::probe::http_probe_pinned(&t.host, t.port, Some(ip), 4).ok;
                }
            }
        }
    }

    let (kind, detail) = classify(&facts);
    TargetRecon {
        name: t.name.clone(),
        host: t.host.clone(),
        kind,
        label: kind.short(),
        split: facts.split.map(|(v, _)| v),
        detail,
    }
}

pub fn run(vpn: VpnCheck) -> Recon {
    let targets: Vec<Target> = crate::targets::default_targets()
        .into_iter()
        .filter(|t| t.port == 443 && GROUPS.iter().any(|g| t.name.starts_with(g)))
        .collect();
    let udp = std::thread::spawn(|| crate::udpprobe::probe_udp(Duration::from_secs(3)));
    let handles: Vec<_> = targets.into_iter().map(|t| std::thread::spawn(move || recon_target(&t))).collect();
    let results: Vec<TargetRecon> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    let udp = udp.join().ok();

    let udp_blocked = udp.as_ref().is_some_and(|u| u.verdict == UdpVerdict::Blocked);
    let (verdict, advice) = summarize(&results, udp_blocked);
    Recon { status: "ok", vpn, message: String::new(), targets: results, udp, verdict, advice }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn facts(path: PathVerdict) -> Facts {
        Facts { path, why: None, dns_spoofed: false, split: None, volume: None }
    }

    fn tr(name: &str, kind: Kind, split: Option<FragVerdict>) -> TargetRecon {
        TargetRecon { name: name.into(), host: String::new(), kind, label: kind.short(), split, detail: String::new() }
    }

    #[test]
    fn ответ_doh_разбирается() {
        let json = r#"{"Status":0,"Answer":[
            {"name":"www.youtube.com","type":5,"TTL":300,"data":"youtube-ui.l.google.com."},
            {"name":"youtube-ui.l.google.com","type":1,"TTL":300,"data":"142.250.74.14"},
            {"name":"youtube-ui.l.google.com","type":1,"TTL":300,"data":"142.250.74.46"}]}"#;
        assert_eq!(parse_doh(json), vec!["142.250.74.14", "142.250.74.46"]);
        assert!(parse_doh(r#"{"Status":3}"#).is_empty());
        assert!(parse_doh("<html>").is_empty());
    }

    #[test]
    fn расхождение_dns() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(dns_differs(&s(&["10.10.34.35"]), &s(&["162.159.137.232"])));
        assert!(!dns_differs(&s(&["162.159.137.232", "162.159.138.232"]), &s(&["162.159.138.232"])));
        // Зашифрованный DNS не ответил — сравнивать не с чем.
        assert!(!dns_differs(&s(&["10.10.34.35"]), &[]));
    }

    #[test]
    fn вердикт_по_цели() {
        assert_eq!(classify(&facts(PathVerdict::Ok)).0, Kind::Clear);

        let mut f = facts(PathVerdict::Ok);
        f.volume = Some((VolumeVerdict::Cutoff, "отдал 16 КБ и замолчал".into()));
        assert_eq!(classify(&f), (Kind::Cutoff, "отдал 16 КБ и замолчал".into()));

        let mut f = facts(PathVerdict::Sni);
        f.why = Some("режут по имени".into());
        f.split = Some((FragVerdict::Helps, "разрезанный проходит".into()));
        assert_eq!(classify(&f), (Kind::Sni, "режут по имени; разрезанный проходит".into()));

        // Подмена DNS важнее того, как выглядел провал.
        let mut f = facts(PathVerdict::Server);
        f.dns_spoofed = true;
        assert_eq!(classify(&f).0, Kind::Dns);
        assert_eq!(classify(&facts(PathVerdict::Server)).0, Kind::NotDpi);
        assert_eq!(classify(&facts(PathVerdict::Unknown)).1, "проверить не удалось");
    }

    #[test]
    fn сводка() {
        let all_clear = [tr("Discord Main", Kind::Clear, None), tr("YouTube Web", Kind::Clear, None)];
        let (v, a) = summarize(&all_clear, false);
        assert_eq!(v, "Discord и YouTube здесь не режут");
        assert!(a[0].contains("обход здесь не нужен"));

        let mixed = [
            tr("Discord Main", Kind::Sni, Some(FragVerdict::Helps)),
            tr("Discord CDN", Kind::Sni, Some(FragVerdict::DoesNotHelp)),
            tr("YouTube Web", Kind::Clear, None),
            tr("YouTube Video", Kind::Cutoff, None),
        ];
        let (v, a) = summarize(&mixed, true);
        assert_eq!(v, "Discord — режут по имени; YouTube — частично: обрыв по объёму");
        // Хоть одна цель, где разрез проходит, — подбор имеет смысл.
        assert!(a.iter().any(|x| x.contains("Подбор стратегии")), "{a:?}");
        assert!(a.iter().any(|x| x.contains("после первых килобайт")));
        assert!(a.iter().any(|x| x.contains("UDP")));
        assert!(!a.iter().any(|x| x.contains("не нужен")));

        let (v, _) = summarize(&[], false);
        assert_eq!(v, "Проверить не удалось");
    }
}
