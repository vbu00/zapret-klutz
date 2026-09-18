use serde::{Deserialize, Serialize};

use crate::probe::{
    classify_path, http_probe_pinned, resolve_ips, tcp_probe, tunnel_hint, FailureCode,
    PathVerdict, NEUTRAL_SNI,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Target {
    pub name: String,
    pub host: String,
    pub port: u16,
}

pub fn default_targets() -> Vec<Target> {
    let raw: &[(&str, &str, u16)] = &[
        ("Discord Main", "discord.com", 443),
        ("Discord Gateway", "gateway.discord.gg", 443),
        ("Discord CDN", "cdn.discordapp.com", 443),
        ("Discord Updates", "updates.discord.com", 443),
        ("YouTube Web", "www.youtube.com", 443),
        // Не голый googlevideo.com: его сертификат выписан на *.googlevideo.com
        // и к самому домену не подходит, curl падает на проверке сертификата
        // при любом конфиге — YouTube всегда выглядел «не отвечающим». Этот
        // же хост проверяет и тест самого zapret.
        ("YouTube Video", "redirector.googlevideo.com", 443),
        ("YouTube Short", "youtu.be", 443),
        ("Rocket League", "api.rlpp.psynet.gg", 443),
        ("Epic Online", "api.epicgames.dev", 443),
        ("Steam", "api.steampowered.com", 443),
        // Отдельно от api.steampowered.com, и это принципиально. Тот —
        // веб-интерфейс на 443: он отвечает и тогда, когда игра не
        // подключается. А это адрес менеджера соединений, куда ходит сам
        // клиент Steam, и порт 27018 — уже игровой диапазон, тот самый,
        // который закрывает Game Filter. Первая цель, которая проверяет
        // игровой путь, а не страницу про игры.
        ("Steam (игровой порт)", "cmp1-vie1.steamserver.net", 27018),
        ("Riot", "auth.riotgames.com", 443),
        ("Battle.net", "us.actual.battle.net", 1119),
        ("Xbox Live", "title.mgt.xboxlive.com", 443),
    ];
    raw.iter()
        .map(|(n, h, p)| Target {
            name: (*n).to_string(),
            host: (*h).to_string(),
            port: *p,
        })
        .collect()
}

/// Чинит сохранённые списки целей от старых версий: там «YouTube Video»
/// указывал на голый googlevideo.com (см. default_targets).
pub fn migrate_hosts(list: &mut [Target]) {
    for t in list.iter_mut() {
        if t.host.eq_ignore_ascii_case("googlevideo.com") {
            t.host = "redirector.googlevideo.com".into();
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TargetResult {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub ok: bool,
    /// Пинг — TCP-рукопожатие (см. `ProbeResult::ms`).
    pub ms: u64,
    /// Полный ответ сервера — в подсказке рядом с пингом.
    #[serde(rename = "totalMs")]
    pub total_ms: u64,
    pub reason: Option<String>,
    /// Каким способом проверяли — в интерфейсе видно, чему верить.
    pub probe: &'static str,
    /// Код отказа по стадиям — по нему ветвится самолечение.
    pub code: FailureCode,
    /// Где блокируют: по имени, по адресу, или это вообще сам сервер.
    pub verdict: PathVerdict,
    /// Человеческое объяснение вердикта. Пусто, когда объяснять нечего.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// Сайты (80/443) проверяем настоящим HTTPS-запросом, всё остальное —
/// TCP-хендшейком: у игровых серверов на своих портах HTTP просто нет.
///
/// На провалившейся пробе делаем второй замер — тем же адресом, но с
/// заведомо чистым именем. Это единственный способ отличить «режут имя»
/// (десинхронизация поможет) от «режут адрес» (не поможет ничто).
fn probe_target(t: &Target) -> TargetResult {
    let (r, probe, verdict, why) = if t.port == 443 || t.port == 80 {
        // Основную пробу тоже прибиваем к адресу: сравнивать имена имеет
        // смысл только на ОДНОМ адресе, иначе разницу объясняют разные
        // серверы, а не блокировка.
        // Перебираем адреса, пока какой-нибудь не ответит. Молчат все —
        // берём последний замер и его же адрес для контроля: вывод о
        // блокировке делаем, только исчерпав список, а не на первом edge.
        let ips = resolve_ips(&t.host, t.port);
        let mut used: Option<String> = None;
        let mut main = http_probe_pinned(&t.host, t.port, None, 4);
        for ip in &ips {
            main = http_probe_pinned(&t.host, t.port, Some(ip), 4);
            used = Some(ip.clone());
            // Останавливаемся не только на успехе. Сертификат и 451 — это
            // тоже ОТВЕТ сервера, и он доказательнее, чем молчание
            // следующего edge. Раньше перебор шёл дальше и затирал такой
            // ответ таймаутом, а вердикт из «сервер жив» превращался в
            // «режут адрес».
            if main.ok || main.code.server_reachable() || main.code == FailureCode::HttpBlocked {
                break;
            }
        }
        let ip = used;

        let (verdict, why) = if main.code.needs_control() {
            // Адреса нет — контроль невозможен. Передаём None, а не «контроль
            // молчал»: вердикт тогда честно скажет «не измерено».
            let control = ip
                .as_deref()
                .map(|ip| http_probe_pinned(NEUTRAL_SNI, t.port, Some(ip), 4))
                .map(|c| (c.ok, c.code));
            classify_path(main.ok, main.code, control)
        } else {
            classify_path(main.ok, main.code, None)
        };
        // Если имя разрешилось во что-то местное, всё измеренное выше — про
        // туннель, а не про провайдера. Сказать это надо и на успехе:
        // «работает» через чужой туннель не означает, что работает обход.
        let why = match ip.as_deref().and_then(tunnel_hint) {
            Some(hint) if why.is_empty() => hint.to_string(),
            Some(hint) => format!("{why}. Причём {hint} — так что замер, возможно, не о провайдере"),
            None => why,
        };
        (main, "http", verdict, why)
    } else {
        // На своём порту имени в трафике нет вовсе: ни SNI, ни Host. Значит
        // блокировать могут только адрес или порт — спрашивать контроль не о чем.
        let r = tcp_probe(&t.host, t.port, 4000);
        let (verdict, why) = if r.ok {
            (PathVerdict::Ok, String::new())
        } else {
            (
                PathVerdict::Ip,
                // Прежний текст говорил «обходить нечего», и это была
                // неправда. Имени на этом порту действительно нет, значит
                // по имени и не режут — но zapret умеет работать и по
                // адресу, ровно для этого в нём Game Filter и появился.
                "порт не отвечает. Имени в трафике здесь нет, значит режут не имя, \
                 а адрес или порт. Против такого в zapret есть Game Filter — он \
                 пускает через обход и игровые порты"
                    .to_string(),
            )
        };
        (r, "tcp", verdict, why)
    };

    TargetResult {
        name: t.name.clone(),
        host: t.host.clone(),
        port: t.port,
        ok: r.ok,
        ms: r.ms,
        total_ms: r.total_ms,
        reason: r.reason,
        probe,
        code: r.code,
        verdict,
        why: if why.is_empty() { None } else { Some(why) },
    }
}

pub fn check_targets(targets: &[Target]) -> Vec<TargetResult> {
    let handles: Vec<_> = targets
        .iter()
        .cloned()
        .map(|t| std::thread::spawn(move || probe_target(&t)))
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}
