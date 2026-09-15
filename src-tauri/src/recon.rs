//! «Как у меня режут»: разведка сети без обхода.
//!
//! Первый этап своего blockcheck. Прежде чем перебирать стратегии, надо
//! понять, против чего: подмену DNS и блок по адресу стратегии zapret не
//! лечат вовсе, блок по имени — их прямая работа, а обрыв по объёму
//! короткими тестами даже не виден. По каждой цели Discord и YouTube:
//!
//! - режут ли по имени или по адресу — проба с контролем, как в мониторинге;
//! - если вышло «по адресу» — перепроверка сырым рукопожатием (см. `recheck`);
//! - если по имени — проходит ли простой разрез ClientHello;
//! - если открылось — не обрывают ли поток после первых килобайт;
//! - если не открылось — не подменяет ли провайдер DNS: сверяем с
//!   зашифрованным DNS и стучимся на настоящий адрес.
//!
//! Меряется сеть как есть, поэтому обход в это время выключен, а VPN
//! проверяется заранее (`vpncheck`).

use serde::Serialize;
use std::process::Command;
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::probe::{PathVerdict, VolumeVerdict, NEUTRAL_SNI};
use crate::targets::Target;
use crate::tlsprobe::{ChOutcome, FragVerdict};
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
    /// Проходит ли простой разрез — только для блока по имени.
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

/// Итог перепроверки «режут адрес» сырым рукопожатием по IPv4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recheck {
    /// Настоящее имя по IPv4 проходит — отказ был не от блокировки.
    Open,
    /// На нейтральное имя сервер отвечает, на настоящее молчит.
    Name,
    /// Адрес действительно недоступен — с объяснением.
    Address(&'static str),
}

/// Зачем перепроверять. Контроль в мониторинге — curl с именем example.com
/// на тот же адрес. Но Cloudflare и Google на незнакомое имя отвечают
/// ОТКАЗОМ в рукопожатии: curl видит ошибку, и вердикт выходит «адрес
/// молчит» при живом адресе. Первая же разведка так и назвала discord.com
/// «режут адрес», хотя обход на ALT его открывал. Сырой ClientHello считает
/// ответом любые байты, отказ тоже, — он отличает отказ сервера от молчания
/// коробки.
pub fn recheck(real: ChOutcome, neutral: ChOutcome) -> Recheck {
    if real == ChOutcome::Answered {
        return Recheck::Open;
    }
    match neutral {
        ChOutcome::Answered => Recheck::Name,
        ChOutcome::NoConnect => Recheck::Address("соединение с адресом не устанавливается вовсе — режут адрес"),
        _ => Recheck::Address("адрес молчит и на нейтральное имя — режут адрес, не имя"),
    }
}

/// Что значит итог пробы разреза — для человека.
///
/// Проба режет ClientHello на два сегмента, и только. «Не проходит» значит,
/// что провайдер собирает сегменты обратно, а не что zapret бессилен:
/// рабочие конфиги (ALT и подобные) обходят такое поддельными пакетами. В
/// первой версии здесь стояло «перебирать стратегии бесполезно» — при том
/// что Discord на ALT12 у того же человека открывался.
pub fn split_text(v: FragVerdict) -> &'static str {
    match v {
        FragVerdict::Helps => "разрезанный запрос проходит — хватит и простых стратегий с разрезом",
        FragVerdict::DoesNotHelp => {
            "простой разрез провайдер собирает обратно — нужны стратегии с поддельными пакетами (fake), как в ALT"
        }
        FragVerdict::NotBlocked => "целый запрос по этому адресу проходит",
        FragVerdict::Inconclusive => "проходит ли разрез, проверить не вышло",
    }
}

/// Всё измеренное по одной цели — вход чистого правила.
pub struct Facts {
    pub path: PathVerdict,
    pub why: Option<String>,
    pub dns_spoofed: bool,
    pub split: Option<FragVerdict>,
    pub volume: Option<(VolumeVerdict, String)>,
    pub recheck: Option<Recheck>,
}

fn by_name(split: Option<FragVerdict>, fallback: String) -> (Kind, String) {
    match split {
        Some(v) => (Kind::Sni, split_text(v).to_string()),
        None => (Kind::Sni, fallback),
    }
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
    match (f.path, f.recheck) {
        (PathVerdict::Ip, Some(Recheck::Open)) => {
            (Kind::Clear, "по IPv4 рукопожатие проходит — отказ был не от блокировки".into())
        }
        (PathVerdict::Ip, Some(Recheck::Name)) => by_name(
            f.split,
            "сервер отвечает на нейтральное имя и молчит на настоящее — режут по имени".into(),
        ),
        (PathVerdict::Ip, Some(Recheck::Address(w))) => (Kind::Ip, w.into()),
        (PathVerdict::Ip, None) => (Kind::Ip, why),
        (PathVerdict::Ok, _) => match &f.volume {
            Some((VolumeVerdict::Cutoff, note)) => (Kind::Cutoff, note.clone()),
            _ => (Kind::Clear, "открывается без обхода".into()),
        },
        (PathVerdict::Sni, _) => by_name(f.split, why),
        (PathVerdict::Cutoff, _) => (Kind::Cutoff, why),
        (PathVerdict::Legal | PathVerdict::Server, _) => (Kind::NotDpi, why),
        (PathVerdict::Unknown, _) => {
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
            "До части адресов не доходит ничего, и на нейтральное имя тоже. От такого стратегии zapret не \
             спасают — если эти цели и с обходом не открываются, поможет только туннель."
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
            "Режут по имени сайта, а простой разрез запроса провайдер собирает обратно. Это обычный случай: \
             рабочие конфиги zapret (ALT и подобные) обходят его поддельными пакетами (fake) — среди них и \
             надо подбирать."
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

    let timeout = Duration::from_secs(3);
    let split_on =
        |ip: std::net::IpAddr| crate::tlsprobe::probe_fragmentation(ip, t.port, &t.host, timeout).verdict;
    let mut facts =
        Facts { path: res.verdict, why: res.why, dns_spoofed: false, split: None, volume: None, recheck: None };
    match res.verdict {
        PathVerdict::Ok => {
            let v = crate::probe::http_probe_volume(&t.host, t.port, None, 10);
            facts.volume = Some((v.verdict, v.note));
        }
        PathVerdict::Sni => {
            let ip = crate::probe::first_ip(&t.host, t.port).and_then(|ip| ip.parse().ok());
            facts.split = ip.map(split_on);
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
            // Подмену исключили — перепроверяем «режут адрес» сырым
            // рукопожатием. По IPv4: curl мог уйти на IPv6, которого нет.
            if !facts.dns_spoofed && res.verdict == PathVerdict::Ip && t.port == 443 {
                let v4 = system.iter().find_map(|ip| ip.parse::<std::net::Ipv4Addr>().ok());
                if let Some(v4) = v4 {
                    let ip = std::net::IpAddr::V4(v4);
                    let r = recheck(
                        crate::tlsprobe::send_ch(ip, t.port, &t.host, false, timeout),
                        crate::tlsprobe::send_ch(ip, t.port, NEUTRAL_SNI, false, timeout),
                    );
                    if r == Recheck::Name {
                        facts.split = Some(split_on(ip));
                    }
                    facts.recheck = Some(r);
                }
            }
        }
    }

    let (kind, detail) = classify(&facts);
    TargetRecon { name: t.name.clone(), host: t.host.clone(), kind, label: kind.short(), split: facts.split, detail }
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
        Facts { path, why: None, dns_spoofed: false, split: None, volume: None, recheck: None }
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

        // Простой разрез не прошёл — это не «zapret бессилен», а «нужен fake».
        let mut f = facts(PathVerdict::Sni);
        f.why = Some("режут по имени".into());
        f.split = Some(FragVerdict::DoesNotHelp);
        let (kind, detail) = classify(&f);
        assert_eq!(kind, Kind::Sni);
        assert!(detail.contains("fake"), "{detail}");
        assert!(!detail.contains("бесполезно"), "{detail}");

        // Подмена DNS важнее того, как выглядел провал.
        let mut f = facts(PathVerdict::Server);
        f.dns_spoofed = true;
        assert_eq!(classify(&f).0, Kind::Dns);
        assert_eq!(classify(&facts(PathVerdict::Server)).0, Kind::NotDpi);
        assert_eq!(classify(&facts(PathVerdict::Unknown)).1, "проверить не удалось");
    }

    #[test]
    fn отказ_сервера_на_чужое_имя_не_блок_адреса() {
        use ChOutcome::*;
        // Настоящее имя режут, а на example.com сервер ответил хотя бы отказом.
        assert_eq!(recheck(Reset, Answered), Recheck::Name);
        assert_eq!(recheck(Silent, Answered), Recheck::Name);
        assert_eq!(recheck(Answered, Silent), Recheck::Open);
        assert!(matches!(recheck(Silent, Silent), Recheck::Address(_)));
        assert!(matches!(recheck(NoConnect, NoConnect), Recheck::Address(w) if w.contains("не устанавливается")));

        // Живой случай: curl сказал «адрес», перепроверка — «по имени».
        let mut f = facts(PathVerdict::Ip);
        f.why = Some("с нейтральным именем example.com тот же адрес тоже молчит — режут адрес, не имя".into());
        f.recheck = Some(Recheck::Name);
        f.split = Some(FragVerdict::DoesNotHelp);
        assert_eq!(classify(&f).0, Kind::Sni);
        f.recheck = Some(Recheck::Open);
        assert_eq!(classify(&f).0, Kind::Clear);
        f.recheck = None;
        assert_eq!(classify(&f).0, Kind::Ip);
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

        // Как у живого пользователя: разрез не проходит нигде — совет про fake.
        let (_, a) = summarize(&[tr("Discord CDN", Kind::Sni, Some(FragVerdict::DoesNotHelp))], false);
        assert!(a[0].contains("fake") && a[0].contains("ALT"), "{a:?}");

        let (v, _) = summarize(&[], false);
        assert_eq!(v, "Проверить не удалось");
    }
}
