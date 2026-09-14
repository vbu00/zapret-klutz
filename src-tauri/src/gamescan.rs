//! Сканирование трафика игры: куда она на самом деле ходит.
//!
//! Зачем. Списки zapret собраны под сайты, и игра в них попадает в лучшем
//! случае страницей авторизации. Адреса игровых серверов не публикуются,
//! меняются от региона к региону и от патча к патчу — узнать их можно
//! только одним способом: посмотреть, куда ходит сам процесс игры. В
//! сообществе это и делают руками через TCPView, выписывая адреса в
//! блокнот. Здесь то же самое, но само.
//!
//! Как. Пока игра запущена, раз в пару секунд снимается список соединений
//! с номерами процессов, из него берутся только строки нужной игры, а из
//! них — внешние адреса. Накопленное уходит в ipset-список рядом с
//! конфигами релиза, и обход начинает покрывать эти адреса.
//!
//! Чего этот способ НЕ может. Он видит адрес, только если соединение
//! состоялось. Игра, которую режут на подключении, часть своих серверов
//! так и не покажет — до них она не дошла. Поэтому сканировать полезно при
//! уже работающем обходе или хотя бы при включённом Game Filter: сперва
//! дать игре дотянуться, потом закрепить найденное.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Одно соединение из таблицы системы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conn {
    pub pid: u32,
    pub proto: Proto,
    pub ip: IpAddr,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Tcp,
    Udp,
}

/// Разбор строки `netstat -ano`.
///
/// Разбираем по СТРУКТУРЕ, а не по словам. Заголовки и состояния у netstat
/// переведены: на русской системе вместо `LISTENING` стоит «ПРОСЛУШИВАНИЕ»,
/// вместо `Active Connections` — «Активные подключения». Любая привязка к
/// тексту сломалась бы на первой же не-английской машине.
///
/// Устойчивая часть такая: первый столбец — протокол, второй — локальный
/// адрес, третий — удалённый, последний — номер процесса. У TCP между ними
/// есть состояние, у UDP его нет, поэтому на число столбцов не смотрим.
pub fn parse_netstat_line(line: &str) -> Option<Conn> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 4 {
        return None;
    }
    let proto = match f[0].to_ascii_uppercase().as_str() {
        "TCP" => Proto::Tcp,
        "UDP" => Proto::Udp,
        _ => return None,
    };
    let pid: u32 = f[f.len() - 1].parse().ok()?;
    let (ip, port) = split_addr(f[2])?;
    Some(Conn { pid, proto, ip, port })
}

/// `1.2.3.4:443` или `[2606:4700::1]:443`. У UDP на месте удалённого адреса
/// может стоять `*:*` — это «ни с кем», а не адрес.
fn split_addr(s: &str) -> Option<(IpAddr, u16)> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, p) = rest.split_once("]:")?;
        (h.to_string(), p)
    } else {
        let (h, p) = s.rsplit_once(':')?;
        (h.to_string(), p)
    };
    let port: u16 = port.parse().ok()?;
    let ip: IpAddr = host.parse().ok()?;
    Some((ip, port))
}

/// Стоит ли добавлять этот адрес в обход.
///
/// Отсеиваем всё, что не уходит к провайдеру: петлю, частные сети, link-local,
/// мультикаст и нули. Это не косметика — попади в ipset хотя бы `192.168.1.1`,
/// и обход начнёт заворачивать трафик к домашнему роутеру.
pub fn is_external(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            !v.is_loopback()
                && !v.is_private()
                && !v.is_link_local()
                && !v.is_multicast()
                && !v.is_broadcast()
                && !v.is_unspecified()
                // 100.64/10 — CGNAT: это ещё адреса провайдера, но свои, и
                // обходить их незачем.
                && !(v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]))
                // 198.18/15 — диапазон для замеров, так выглядит fake-IP прокси.
                && !(v.octets()[0] == 198 && (v.octets()[1] == 18 || v.octets()[1] == 19))
        }
        IpAddr::V6(v) => {
            !v.is_loopback() && !v.is_multicast() && !v.is_unspecified()
                // fe80::/10 link-local и fc00::/7 unique-local.
                && !matches!(v.segments()[0] & 0xffc0, 0xfe80)
                && (v.segments()[0] & 0xfe00) != 0xfc00
        }
    }
}

/// Полная таблица соединений системы.
pub fn connections() -> Vec<Conn> {
    crate::sys::run("netstat", &["-ano"])
        .lines()
        .filter_map(parse_netstat_line)
        .collect()
}

/// Номера процессов с таким именем образа. Пусто — процесс не запущен.
pub fn pids_of(image: &str) -> HashSet<u32> {
    // Формат CSV, потому что в имени процесса бывают пробелы, а колонки в
    // человекочитаемом выводе ещё и переведены.
    let out = crate::sys::run(
        "tasklist",
        &["/FI", &format!("IMAGENAME eq {image}"), "/FO", "CSV", "/NH"],
    );
    out.lines().filter_map(parse_tasklist_line).collect()
}

/// `"имя.exe","1234","Console","1","12 345 КБ"` — второе поле это PID.
fn parse_tasklist_line(line: &str) -> Option<u32> {
    parse_tasklist_row(line).map(|(_, pid)| pid)
}

/// Имя образа и номер процесса из строки CSV.
fn parse_tasklist_row(line: &str) -> Option<(String, u32)> {
    let mut fields = line.split("\",\"");
    let name = fields.next()?.trim_matches('"').to_string();
    let pid: u32 = fields.next()?.trim_matches('"').parse().ok()?;
    if name.is_empty() {
        return None;
    }
    Some((name, pid))
}

/// Номер процесса → имя образа, для всех процессов разом.
pub fn process_names() -> std::collections::HashMap<u32, String> {
    crate::sys::run("tasklist", &["/FO", "CSV", "/NH"])
        .lines()
        .filter_map(parse_tasklist_row)
        .map(|(n, p)| (p, n))
        .collect()
}

/// Известные НЕ-игровые порты вне веба. Без этого списка любая фоновая
/// служба выглядит игрой: у неё тоже «не 80 и не 443».
///
/// 5228 — Google FCM, на нём сидят уведомления половины программ; именно он
/// однажды и выдал службу HP за игру. Остальное — почта, DNS, время, SMB.
fn служебный_порт(port: u16) -> bool {
    matches!(
        port,
        53 | 67 | 68 | 88 | 123 | 135 | 137..=139 | 389 | 445 | 465 | 500
            | 514 | 587 | 636 | 993 | 995 | 1194 | 1701 | 1723 | 1900
            | 3389 | 5222 | 5223 | 5228..=5230 | 5353 | 5938 | 8080 | 8443 | 8883 | 9443
    )
}

/// Порт, на котором обычно живут игры.
///
/// Диапазоны взяты из того, что игры действительно используют: Battle.net
/// на 1119, Xbox на 3074, Riot около 5000-5500 и 7000-8000, Steam и
/// источники на 27000-27100. Всё прочее выше 1024 считаем возможным, но
/// слабым признаком — вес у него меньше.
fn игровой_порт(port: u16) -> bool {
    matches!(port, 1119 | 3074 | 3478..=3480 | 5000..=5500 | 6112..=6119 | 7000..=8000 | 27000..=27200)
}

/// Насколько соединение похоже на игровое. Ноль — не похоже вовсе.
fn вес(proto: Proto, port: u16) -> u32 {
    // Известный служебный порт перекрывает всё: диапазоны игр широкие и
    // задевают чужое. 5228 (уведомления Google) попадает в «риотовские»
    // 5000-5500, и именно на этом эвристика однажды приняла службу HP за
    // игру. Сначала отсекаем известное, потом смотрим на игровое.
    if port < 1024 || служебный_порт(port) {
        return 0;
    }
    match (proto, игровой_порт(port)) {
        // UDP на игровом порту — самый сильный признак: так ходит сам матч,
        // а фоновые службы этого почти не делают.
        (Proto::Udp, true) => 8,
        (Proto::Tcp, true) => 4,
        // UDP на произвольном высоком порту — слабее, но тоже довод.
        (Proto::Udp, false) => 3,
        // А вот TCP на случайном высоком порту не значит ничего: так ходит
        // половина фоновых программ.
        (Proto::Tcp, false) => 0,
    }
}

/// Настоящий ли это процесс.
///
/// netstat вешает на PID 0 соединения, которые закрываются или чьи владельцы
/// уже вышли, а PID 4 — это ядро. Оба выглядят как обычные строки с живыми
/// портами: на этой машине «System Idle Process» так и вышел в кандидаты с
/// портами 1119 и 27018. Адреса там настоящие, а владелец — нет, и следить
/// за таким процессом бессмысленно: он никогда ничего не откроет.
///
/// Отсекаем по НОМЕРУ, а не по имени: имя псевдопроцесса переведено на
/// русской Windows, и привязка к тексту сломалась бы там же, где и всё
/// остальное.
fn настоящий_процесс(pid: u32) -> bool {
    pid > 4
}

/// Кандидат в игру: процесс и чем он себя выдал.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    /// Сумма весов соединений — по ней и сортируем.
    pub score: u32,
    /// Сколько разных внешних адресов.
    pub addrs: usize,
    /// Порты, по которым его посчитали похожим на игру.
    pub ports: Vec<u16>,
}

/// Кто из работающих процессов похож на игру, лучшие первыми.
///
/// Считаем по ПОЛОЖИТЕЛЬНЫМ признакам, а не по отсутствию в чёрном списке.
/// Прежняя версия брала всё, что ходит не по 80 и 443, и однажды уверенно
/// назвала игрой службу HP: та стучалась на 5228, порт уведомлений Google.
/// Чёрным списком это не лечится — фоновых процессов сотни, и перечислить
/// их нельзя. А вот признаки игры перечислить можно: UDP на высоком порту и
/// известные игровые диапазоны.
///
/// Пусто — значит не нашли. Это честный ответ: лучше сказать «запусти игру»,
/// чем собрать адреса постороннего процесса и положить их в обход.
///
/// Чистая функция: соединения и имена собирает вызывающий.
pub fn candidates(
    conns: &[Conn],
    names: &std::collections::HashMap<u32, String>,
) -> Vec<Candidate> {
    use std::collections::HashMap;
    let mut acc: HashMap<&str, (u32, BTreeSet<String>, BTreeSet<u16>)> = HashMap::new();
    for c in conns {
        if !is_external(&c.ip) || !настоящий_процесс(c.pid) {
            continue;
        }
        let w = вес(c.proto, c.port);
        if w == 0 {
            continue;
        }
        let Some(name) = names.get(&c.pid) else { continue };
        let e = acc.entry(name).or_default();
        e.0 += w;
        e.1.insert(c.ip.to_string());
        e.2.insert(c.port);
    }
    let mut out: Vec<Candidate> = acc
        .into_iter()
        .map(|(name, (score, addrs, ports))| Candidate {
            name: name.to_string(),
            score,
            addrs: addrs.len(),
            ports: ports.into_iter().collect(),
        })
        .collect();
    // По убыванию веса, а при равенстве — по имени, чтобы порядок не плясал
    // от запуска к запуску.
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    out
}

/// Самый вероятный кандидат, если он есть.
pub fn guess_game(
    conns: &[Conn],
    names: &std::collections::HashMap<u32, String>,
) -> Option<String> {
    candidates(conns, names).into_iter().next().map(|c| c.name)
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    /// Нашёлся ли вообще процесс игры хоть раз за время скана.
    pub running: bool,
    /// Процесс, чьи адреса собраны. `None` — игру не нашли.
    pub process: Option<String>,
    /// Внешние адреса, отсортированные и без повторов.
    pub addrs: Vec<String>,
    /// Удалённые порты — по ним видно, TCP тут или UDP и какой диапазон.
    pub tcp_ports: Vec<u16>,
    pub udp_ports: Vec<u16>,
    /// Сколько раз успели снять таблицу.
    pub ticks: u32,
    pub note: String,
}

/// Копит адреса процессов игры, пока идёт время.
///
/// Опрос, а не подписка: событий о новых соединениях система бесплатно не
/// отдаёт, а таблица снимается дёшево.
///
/// Чего опрос НЕ умеет, и это стоит знать: соединение, открывшееся и
/// закрывшееся между двумя тиками, он пропустит. Раньше здесь было написано
/// ровно наоборот — будто шаг в пару секунд такие соединения ловит. Не
/// ловит, и уменьшать шаг до бесконечности смысла нет.
/// Пустой `images` означает «найди сам»: на первом же тике берём процесс,
/// больше всего похожий на игру, и дальше следим только за ним.
pub fn scan(
    images: &[String],
    total: Duration,
    step: Duration,
    mut progress: impl FnMut(&str, usize),
) -> ScanResult {
    let started = Instant::now();
    let mut images: Vec<String> = images.to_vec();
    let mut угадан: Option<String> = None;
    if images.is_empty() {
        let names = process_names();
        if let Some(g) = guess_game(&connections(), &names) {
            угадан = Some(g.clone());
            images.push(g);
        }
    }
    let images = images;
    // Искать нечего — не занимать полминуты молчанием. Раньше цикл честно
    // отрабатывал всё время, ничего не делая, и человек ждал впустую.
    if images.is_empty() {
        return ScanResult {
            running: false,
            process: None,
            addrs: Vec::new(),
            tcp_ports: Vec::new(),
            udp_ports: Vec::new(),
            ticks: 0,
            note: "не нашлось ни одного процесса, похожего на игру. Запусти игру, \
                   зайди в меню и попробуй снова"
                .into(),
        };
    }
    progress(images.first().map(|s| s.as_str()).unwrap_or(""), 0);
    let mut addrs: BTreeSet<String> = BTreeSet::new();
    let mut tcp: BTreeSet<u16> = BTreeSet::new();
    let mut udp: BTreeSet<u16> = BTreeSet::new();
    let mut running = false;
    let mut ticks = 0u32;

    while started.elapsed() < total {
        let pids: HashSet<u32> = images.iter().flat_map(|i| pids_of(i)).collect();
        if !pids.is_empty() {
            running = true;
            for c in connections() {
                if !pids.contains(&c.pid) || !is_external(&c.ip) {
                    continue;
                }
                // Тот же фильтр портов, что и у глубокого сбора. Здесь его не
                // было вовсе: адрес клался сразу после проверки «внешний ли»,
                // и веб-трафик игры уезжал в игровой список напрямую. У
                // Valorant весь TCP идёт в Cloudflare на 443, 5223 и 8443 —
                // именно так туда и попала чужая сеть 104.29.153.0/24, а
                // оттуда под десинхронизацию четырёх профилей конфига.
                //
                // Порты при этом записываем ДО фильтра: показать человеку,
                // куда ходит игра, полезно и для отброшенных.
                match c.proto {
                    Proto::Tcp => tcp.insert(c.port),
                    Proto::Udp => udp.insert(c.port),
                };
                if !стоит_собирать(c.proto, c.port) {
                    continue;
                }
                addrs.insert(c.ip.to_string());
            }
        }
        ticks += 1;
        progress(images.first().map(|s| s.as_str()).unwrap_or(""), addrs.len());
        let left = total.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(step.min(left));
    }

    let mut note = describe(running, addrs.len(), ticks);
    if let Some(g) = &угадан {
        note = format!("процесс: {g}. {note}");
    }
    ScanResult {
        running,
        process: угадан.clone(),
        addrs: addrs.into_iter().collect(),
        tcp_ports: tcp.into_iter().collect(),
        udp_ports: udp.into_iter().collect(),
        ticks,
        note,
    }
}

/// Что сказать человеку. Отдельно от сети, чтобы можно было проверить.
pub fn describe(running: bool, found: usize, ticks: u32) -> String {
    if ticks == 0 {
        return "сканирование не запускалось".into();
    }
    if !running {
        return "процесс игры не найден — запусти игру и сканируй, пока она работает".into();
    }
    if found == 0 {
        return "игра запущена, но наружу пока не ходила: зайди в меню, начни матч — \
                адреса появляются, когда игра реально подключается"
            .into();
    }
    format!(
        "поймано адресов: {found}. Это те, до которых игра ДОШЛА; если её режут на \
         подключении, часть серверов сюда не попадёт — сканируй при работающем обходе. \
         В список кладём их сети /24: игровые серверы обычно стоят в одном блоке, \
         так что соседи по пулу накроются заодно"
    )
}

// ─────────── сети оператора ───────────
//
// Пойманный адрес — один сервер из пула, и /24 вокруг него покрывает лишь
// соседей по стойке. У Riot объявлено 36 сетей, у Valve 45, и матч может
// уехать в любую: за два сбора подряд на Valorant поймались 162.249.72.0/24
// и 185.40.64.0/24 — разные сети одного оператора. Поэтому спрашиваем, чьи
// это адреса, и берём все его сети сразу.

/// Порог, выше которого оператор считается облаком, а не игровым.
///
/// Взято из замеров, а не с потолка: Riot объявляет 36 сетей, Valve — 45.
/// А Cloudflare 2395, Google 1233, Amazon 18020. Разница на два порядка,
/// и порог посередине разделяет их надёжнее любого списка имён — который
/// к тому же устарел бы на первом же операторе, о котором мы не слышали.
///
/// Зачем порог вообще: втащить в список тысячи сетей Cloudflare значило бы
/// направить обход на пол-интернета. Для игрового профиля это верный способ
/// сломать всё разом.
pub const MAX_OPERATOR_PREFIXES: usize = 256;

/// Стоит ли разворачивать адрес в сети оператора.
pub fn should_expand(prefix_count: usize) -> bool {
    prefix_count > 0 && prefix_count <= MAX_OPERATOR_PREFIXES
}

/// Номер оператора из ответа справочника о сети.
pub fn parse_asn(json: &str) -> Option<String> {
    let at = json.find("\"asns\"")?;
    let rest = &json[at..];
    let start = rest.find('[')?;
    let end = rest.find(']')?;
    // Скобки могут прийти в любом порядке: ответ сетевой, и портится он
    // не спрашивая. Срез по перевёрнутым границам — это паника.
    if end <= start {
        return None;
    }
    rest[start + 1..end]
        .split(',')
        .next()?
        .trim()
        .trim_matches('"')
        .parse::<u32>()
        .ok()
        .map(|n| n.to_string())
}

/// Сети IPv4 из ответа справочника о префиксах оператора.
pub fn parse_prefixes(json: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in json.split("\"prefix\"").skip(1) {
        let Some(start) = part.find('"') else { continue };
        let rest = &part[start + 1..];
        let Some(end) = rest.find('"') else { continue };
        let p = &rest[..end];
        // Только IPv4 и только похожее на сеть.
        if !p.contains(':') && p.contains('/') && p.split('.').count() == 4 && !out.contains(&p.to_string()) {
            out.push(p.to_string());
        }
    }
    out
}

/// Чей это адрес и что с ним делать.
///
/// Исходов ровно три, и раньше два из них были склеены в `None`: «не
/// выяснили» и «это облако» вели к одному и тому же — сети /24 вокруг
/// адреса. Для облака это неверно. Замерено на живом Valorant: в улов
/// попадает адрес Cloudflare, и /24 вокруг него уезжает в игровой список,
/// а оттуда под десинхронизацию. Именно эту строку — 104.29.153.0/24 —
/// пришлось вычищать руками, и следующий же сбор принёс её обратно.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operator {
    /// Сети оператора: их и кладём.
    Nets { asn: String, nets: Vec<String> },
    /// Оператор известен, но за его адресами стоит пол-интернета. Такой
    /// адрес не берём совсем — ни сетями, ни /24.
    Cloud { asn: String, prefixes: usize },
    /// Выяснить не вышло: справочник недоступен, ответ пуст или испорчен.
    Unknown,
}

/// Имя оператора из ответа справочника.
///
/// Справочник отдаёт его вместе с кодом реестра: «RIOT-NA1 - Riot Games,
/// Inc». Человеку нужна вторая половина — код реестра ему ничего не
/// говорит. Разделителя может и не быть, тогда берём как есть.
pub fn parse_holder(json: &str) -> Option<String> {
    let at = json.find("\"holder\"")?;
    let rest = &json[at + 8..];
    let start = rest.find('"')?;
    let rest = &rest[start + 1..];
    let end = rest.find('"')?;
    let whole = rest[..end].trim();
    if whole.is_empty() {
        return None;
    }
    Some(match whole.split_once(" - ") {
        Some((_, name)) if !name.trim().is_empty() => name.trim().to_string(),
        _ => whole.to_string(),
    })
}

/// Как зовут оператора. Пусто — справочник промолчал, обойдёмся номером.
pub fn holder_of(asn: &str) -> String {
    crate::maintenance::http_get(&format!(
        "https://stat.ripe.net/data/as-overview/data.json?resource=AS{asn}"
    ))
    .ok()
    .and_then(|j| parse_holder(&j))
    .unwrap_or_default()
}

/// Решение по числу объявленных сетей. Вынесено отдельно, чтобы
/// проверяться без сети.
fn decide(asn: &str, pfx: Vec<String>) -> Operator {
    if pfx.is_empty() {
        return Operator::Unknown;
    }
    if !should_expand(pfx.len()) {
        return Operator::Cloud { asn: asn.to_string(), prefixes: pfx.len() };
    }
    Operator::Nets { asn: asn.to_string(), nets: pfx }
}

/// Спрашивает справочник, чей адрес.
///
/// Best-effort целиком: любая осечка — `Unknown`, и вызывающий оставляет
/// /24. Сеть тут не обязана быть, сбор работает и без неё.
pub fn operator_of(ip: &str) -> Operator {
    let Ok(info) = crate::maintenance::http_get(&format!(
        "https://stat.ripe.net/data/network-info/data.json?resource={ip}"
    )) else {
        return Operator::Unknown;
    };
    let Some(asn) = parse_asn(&info) else {
        return Operator::Unknown;
    };
    let Ok(list) = crate::maintenance::http_get(&format!(
        "https://stat.ripe.net/data/announced-prefixes/data.json?resource=AS{asn}"
    )) else {
        return Operator::Unknown;
    };
    decide(&asn, parse_prefixes(&list))
}

/// Адрес сервера — это один из пула, а не единственный.
///
/// Матч подключается к ОДНОМУ серверу: за полминуты сбора их и набирается
/// один-два. Следующий матч даст соседний, и в списке его уже не будет —
/// собирать заново перед каждой игрой никто не станет.
///
/// Запасной путь, когда оператора выяснить не удалось: сеть /24. Провайдер держит игровые
/// серверы пачками в одном блоке: поймав 146.66.155.73 у Valve, мы
/// накрываем и остальные её relay в 146.66.155.0/24. Шире брать не
/// стоит — /24 это один узел присутствия, а не половина интернета.
///
/// IPv6 оставляем как есть: там адресов столько, что нарезать их по
/// подсетям наугад смысла нет, а игровой трафик по IPv6 пока редкость.
pub fn to_subnet(ip: &str) -> String {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V4(v)) => {
            let o = v.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        _ => ip.to_string(),
    }
}

/// Куда класть собранные адреса.
///
/// У одних и тех же адресов два противоположных применения, и выбирать
/// между ними должен человек, а не мы за него:
///
/// * `Bypass` — игра заблокирована, обход должен до неё дотянуться. Адреса
///   идут в `ipset-all.txt`, по которому игровой профиль и решает, к чему
///   применяться.
/// * `Skip` — игра работает, а обход ей мешает. Адреса идут в
///   `ipset-exclude-user.txt`, и winws оставляет этот трафик в покое. Это
///   и есть здешний аналог `--lua-desync=pass` из наборов zapret2, где
///   игровой UDP Riot помечен «не трогать».
///
/// Второй случай не теоретический: на живом Valorant добавление серверов
/// Riot в `ipset-all.txt` включило на них `fake` с двенадцатью повторами,
/// и игра показала «высокий пинг» и «проблема с сетью».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Bypass,
    Skip,
}

impl Target {
    fn file(self) -> &'static str {
        match self {
            Target::Bypass => "ipset-all.txt",
            Target::Skip => "ipset-exclude-user.txt",
        }
    }

    fn opposite(self) -> Target {
        match self {
            Target::Bypass => Target::Skip,
            Target::Skip => Target::Bypass,
        }
    }
}

/// Убирает перечисленные адреса из нашего блока. Возвращает, сколько убрал.
///
/// Чужие строки не трогает: в этих файлах бывает и не наше. Сам адрес никуда
/// не записывается — он только сверяется со строками блока, поэтому строка
/// из интерфейса ничего сюда не протащит: не совпала — ничего не произошло.
pub fn remove_from(
    root: &std::path::Path,
    target: Target,
    addrs: &[String],
) -> Result<usize, String> {
    let list = root.join("lists").join(target.file());
    let Ok(existing) = std::fs::read_to_string(&list) else {
        return Ok(0);
    };
    let (mut groups, skipped) = parse_groups(&existing);
    let было: usize = groups.iter().map(|g| g.nets.len()).sum();
    for g in groups.iter_mut() {
        g.nets.retain(|n| !addrs.contains(n));
    }
    // Опустевшая группа уходит вместе со своей строкой: оператор без сетей
    // человеку ни о чём не говорит.
    groups.retain(|g| !g.nets.is_empty());
    let убрано = было - groups.iter().map(|g| g.nets.len()).sum::<usize>();
    if убрано == 0 {
        return Ok(0);
    }
    // Пустой `ipset-all.txt` значит «применяться ко всему», поэтому там на
    // месте пустоты обязана остаться заглушка. Пустой список исключений
    // значит «ничего не исключать» — заглушка в нём только путала бы.
    let text = if groups.is_empty() && skipped.is_empty() && target == Target::Skip {
        without_block(&existing)
    } else {
        merge_groups(&existing, &groups, &skipped)
    };
    std::fs::write(&list, text).map_err(|e| e.to_string())?;
    Ok(убрано)
}

/// Когда список последний раз менялся, в миллисекундах эпохи.
///
/// Берём время файла, а не храним свою отметку: список пишет и сбор, и
/// удаление адреса. Поэтому это честно называется «последнее изменение», а
/// не «собрано»: тот же файл может тронуть обновление списка IPSet.
pub fn changed_at(root: &std::path::Path, target: Target) -> Option<u64> {
    let m = std::fs::metadata(root.join("lists").join(target.file())).ok()?;
    let t = m.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64)
}

/// Делит сети на те, что оператор объявляет, и все остальные.
///
/// Сверяем строкой, а не арифметикой по маске: сети в списке и взялись из
/// того же самого ответа справочника, поэтому совпадают дословно. Чужое
/// остаётся непривязанным — приписать его оператору было бы враньём.
pub fn partition_by(nets: &[String], announced: &[String]) -> (Vec<String>, Vec<String>) {
    nets.iter().cloned().partition(|n| announced.contains(n))
}

/// Добавляет группу к уже собранным.
///
/// Сеть, которая где-то уже лежит, второй раз не кладётся, а группа того же
/// оператора пополняется, а не заводится заново: иначе после трёх сборов
/// подряд Riot был бы тремя одинаковыми группами.
fn добавить_группу(groups: &mut Vec<Group>, asn: String, name: String, at: u64, nets: Vec<String>) {
    let занято: BTreeSet<String> = groups.iter().flat_map(|g| g.nets.iter().cloned()).collect();
    let mut свежие: Vec<String> = nets.into_iter().filter(|n| !занято.contains(n)).collect();
    свежие.sort();
    свежие.dedup();
    if свежие.is_empty() {
        return;
    }
    if let Some(g) = groups.iter_mut().find(|g| !g.asn.is_empty() && g.asn == asn) {
        g.nets.extend(свежие);
        g.nets.sort();
        g.nets.dedup();
        g.at = at;
        if g.name.is_empty() {
            g.name = name;
        }
        return;
    }
    groups.push(Group { asn, name, at, nets: свежие, legacy: false });
}

/// Подписывает набор сетей оператором, не меняя сам список.
///
/// Сети переезжают из безымянной группы в именованную. Всё, чего оператор
/// не объявляет, остаётся где лежало: приписать ему чужое было бы враньём.
///
/// Состав списка при этом не меняется — добавляются только строки-подписи,
/// поэтому перезапускать winws не нужно.
pub fn attribute(
    root: &std::path::Path,
    сети: &[String],
    asn: &str,
    name: &str,
) -> Result<(), String> {
    let list = root.join("lists").join(Target::Bypass.file());
    let existing = std::fs::read_to_string(&list).map_err(|e| e.to_string())?;
    let (mut groups, skipped) = parse_groups(&existing);
    for g in groups.iter_mut() {
        g.nets.retain(|n| !сети.contains(n));
    }
    groups.retain(|g| !g.nets.is_empty());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    добавить_группу(&mut groups, asn.to_string(), name.to_string(), now, сети.to_vec());
    std::fs::write(&list, merge_groups(&existing, &groups, &skipped)).map_err(|e| e.to_string())
}

/// Кладёт собранные адреса в список релиза, не тронув чужие строки.
///
/// Возвращает то, что в список НЕ пошло, готовыми к показу строками.
/// Пропущенное обязано доходить до человека: молча выброшенный адрес — это
/// «собрал, а игра всё равно не работает», и причину потом не найти.
pub fn save_ips_to(
    root: &std::path::Path,
    target: Target,
    addrs: &[String],
) -> Result<Vec<String>, String> {
    let list = root.join("lists").join(target.file());
    let existing = std::fs::read_to_string(&list).unwrap_or_default();
    // К уже собранному добавляем, а не заменяем: сканов бывает несколько —
    // отдельно меню, отдельно матч, отдельно голосовой чат.
    let (mut groups, mut skipped) = parse_groups(&existing);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // Только пропущенное ЭТОГО сбора: старое человек уже видел, и
    // повторять его в итоге очередного скана незачем.
    let mut свежие_пропуски = Vec::new();
    for a in addrs {
        if !адрес_или_сеть(a) {
            continue;
        }
        // Готовую сеть спрашивать не о чем: так приходит перенос из списка в
        // список, где адреса уже развёрнуты. Без этой проверки «Не трогать»
        // ходило в справочник по разу на каждую из 37 сетей — впустую.
        if a.contains('/') {
            добавить_группу(&mut groups, String::new(), String::new(), now, vec![a.clone()]);
            continue;
        }
        // Сети оператора, если удалось выяснить; иначе /24 вокруг адреса.
        // Один адрес одного оператора спрашиваем один раз: у пойманных
        // адресов оператор обычно общий.
        match operator_of(a) {
            Operator::Nets { asn, nets } => {
                let name = holder_of(&asn);
                добавить_группу(&mut groups, asn, name, now, nets);
            }
            // Облако не берём ни в каком виде: его /24 — это чужой трафик,
            // который обход начнёт ломать, а к игре он отношения не имеет.
            Operator::Cloud { asn, prefixes } => {
                if skipped.iter().any(|s| s.addr == *a) {
                    continue;
                }
                let name = holder_of(&asn);
                let человеку = format!(
                    "{a} — облако {}, у него {prefixes} сетей",
                    if name.is_empty() { format!("AS{asn}") } else { name.clone() }
                );
                skipped.push(Skipped { addr: a.clone(), asn, name, prefixes });
                свежие_пропуски.push(человеку);
            }
            Operator::Unknown => {
                добавить_группу(&mut groups, String::new(), String::new(), now, vec![to_subnet(a)]);
            }
        }
    }
    let all: Vec<String> = groups.iter().flat_map(|g| g.nets.iter().cloned()).collect();
    std::fs::write(&list, merge_groups(&existing, &groups, &skipped)).map_err(|e| e.to_string())?;
    // Один адрес не может значить «обходить» и «не трогать» одновременно.
    // Оба файла уходят в winws в ОДНУ группу аргументов: рядом с
    // `--ipset=ipset-all.txt` всегда стоит `--ipset-exclude=ipset-exclude-user.txt`,
    // в игровых группах тоже. Попавший в оба адрес просто выпадает из обхода.
    //
    // Замерено на живой машине: после «Не трогать», а следом «Собрать
    // адреса» все 37 сетей Riot лежали в обоих файлах разом. Интерфейс
    // показывал «37 адресов», а игровой профиль не применялся к ним вовсе.
    remove_from(root, target.opposite(), &all)?;
    Ok(свежие_пропуски)
}

/// Совместимость с прежними вызовами: по умолчанию — в обход.
pub fn save_ips(root: &std::path::Path, addrs: &[String]) -> Result<Vec<String>, String> {
    save_ips_to(root, Target::Bypass, addrs)
}

/// Какие наши адреса сейчас в списке.
pub fn saved_ips_in(root: &std::path::Path, target: Target) -> Vec<String> {
    std::fs::read_to_string(root.join("lists").join(target.file()))
        .map(|c| extract_block(&c))
        .unwrap_or_default()
}

pub fn saved_ips(root: &std::path::Path) -> Vec<String> {
    saved_ips_in(root, Target::Bypass)
}

/// Убирает наш блок целиком, оставив чужое как было.
pub fn clear_ips_in(root: &std::path::Path, target: Target) -> Result<(), String> {
    let list = root.join("lists").join(target.file());
    let existing = std::fs::read_to_string(&list).unwrap_or_default();
    std::fs::write(&list, merge_block(&existing, &[])).map_err(|e| e.to_string())
}

pub fn clear_ips(root: &std::path::Path) -> Result<(), String> {
    clear_ips_in(root, Target::Bypass)
}

// ─────────── сбор из лога самого winws ───────────
//
// Зачем это отдельно от netstat. Таблица сокетов показывает удалённый адрес
// только у СОЕДИНЁННЫХ сокетов. Игровой матч так не ходит: он шлёт пакеты
// через sendto, и в таблице напротив такого сокета стоит «*:*». Замерено на
// живой машине: из 77 строк UDP удалённый адрес был у двух, и одна из них
// петля. То есть главный трафик игры этим способом не увидеть в принципе.
//
// А winws сидит на WinDivert и видит сами пакеты. С `--debug=1` он печатает
// каждый, который попал под `--wf-*`, — включая тот самый несоединённый UDP.
// Klutz его вывод и так перехватывает, остаётся разобрать строки.
//
// Оговорка, которой тут сперва не было: печатает он их в своём формате, и
// пока разбор его не понимал, всё это не имело значения. Сбор «работал» и
// находил единицы адресов из редких строк другого вида.

/// Адрес назначения из строки лога winws, если он там есть.
///
/// Две формы, обе встречаются в его выводе:
///   `TCP [1.2.3.4]:52000 => [5.6.7.8]:27015 : ...`
///   `dpi desync src=1.2.3.4:52000 dst=5.6.7.8:27015`
/// Разбираем обе и не привязываемся к остальному тексту: он меняется от
/// версии к версии, а адрес — нет.
pub fn parse_log_addr(line: &str) -> Option<LogAddr> {
    let proto = if line.to_ascii_lowercase().contains("proto=udp") || line.contains("UDP") {
        Proto::Udp
    } else {
        Proto::Tcp
    };

    // Форма «dpi desync»: адрес с портом прямо в поле.
    if let Some(rest) = line.split("dst=").nth(1) {
        if let Some(token) = rest.split_whitespace().next() {
            if let Some((ip, port)) = split_addr(token) {
                return Some(LogAddr { ip, port, proto, sport: поле(line, "sport=") });
            }
        }
    }

    // Основная форма пакета. Собрана у winws из отдельных кусков и
    // выглядит так:
    //   IP4: 192.168.1.16 => 104.21.43.64 proto=udp ttl=128 sport=50282 dport=443
    // Порт тут ОТДЕЛЬНЫМ полем, а не после адреса. Я этого сперва не
    // учёл: разбор ждал «=> адрес:порт», на этих строках возвращал пусто,
    // и сбор работал только на редких строках «dpi desync». Отсюда и
    // выходили единицы адресов вместо десятков.
    if let Some(rest) = line.split("=> ").nth(1) {
        if let Some(token) = rest.split_whitespace().next() {
            // Сначала форма, где порт рядом с адресом: [1.2.3.4]:27015.
            // Разбираем ИСХОДНЫЙ токен: обрезав скобки заранее, мы
            let bare = token.trim_matches(|c| c == '[' || c == ']');
            if let Ok(ip) = bare.parse::<IpAddr>() {
                if let Some(port) = поле(line, "dport=") {
                    return Some(LogAddr { ip, port, proto, sport: поле(line, "sport=") });
                }
            }
            // И старая форма conntrack, где порт всё-таки рядом с адресом.
            if let Some((ip, port)) = split_addr(token) {
                return Some(LogAddr { ip, port, proto, sport: поле(line, "sport=") });
            }
        }
    }
    None
}

/// Адрес из строки лога вместе с тем, что о нём известно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogAddr {
    pub ip: IpAddr,
    pub port: u16,
    pub proto: Proto,
    /// Исходящий порт. По нему пакет можно привязать к процессу: локальные
    /// порты у процесса система показывает даже для несоединённого UDP.
    pub sport: Option<u16>,
}

/// Числовое поле вида `имя=1234` из строки лога.
fn поле(line: &str, name: &str) -> Option<u16> {
    line.split(name)
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Что известно об адресе, пойманном в логе.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hit {
    pub udp: bool,
    pub tcp: bool,
    /// Протокол и исходящий порт пакетов к этому адресу. По порту находим
    /// процесс: локальные порты система показывает даже у несоединённого UDP.
    pub sports: BTreeSet<(Proto, u16)>,
}

/// Копилка адресов, пока идёт сбор из лога. `None` — сбор не идёт, и тогда
/// строки лога через неё просто пролетают.
pub static HARVEST: std::sync::Mutex<Option<BTreeMap<String, Hit>>> = std::sync::Mutex::new(None);

/// Идёт сбор адресов игры. Сбор длится до пяти минут и перезапускает обход:
/// самолечение, переключив в это время стратегию, было бы молча отменено
/// самим сбором на выходе, а прогон тестов крутил бы конфиги прямо под ним.
static SCAN_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn scan_busy() -> bool {
    SCAN_BUSY.load(std::sync::atomic::Ordering::SeqCst)
}

/// Право на сбор. Снимается само — на раннем `return` и по ошибке тоже.
pub struct ScanGuard(());

impl ScanGuard {
    pub fn acquire(state: &crate::state::AppState) -> Result<Self, String> {
        if *state.testing.lock().unwrap() {
            return Err("Сейчас идёт прогон тестов — дождись его окончания.".into());
        }
        SCAN_BUSY
            .compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst)
            .map_err(|_| "Сбор адресов уже идёт.".to_string())?;
        Ok(ScanGuard(()))
    }
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        SCAN_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Строка годится в справочник и в список, только если это адрес или сеть.
/// Наш блок в файле человек волен поправить руками, и оттуда строка ушла бы
/// и в запрос к справочнику, и обратно в список, который читает winws.
pub fn адрес_или_сеть(s: &str) -> bool {
    match s.split_once('/') {
        None => s.parse::<IpAddr>().is_ok(),
        Some((ip, len)) => match (ip.parse::<IpAddr>(), len.parse::<u8>()) {
            (Ok(IpAddr::V4(_)), Ok(l)) => l <= 32,
            (Ok(IpAddr::V6(_)), Ok(l)) => l <= 128,
            _ => false,
        },
    }
}

pub fn harvest_start() {
    *HARVEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(BTreeMap::new());
}

/// Останавливает сбор и отдаёт накопленное вместе с тем, откуда оно шло.
pub fn harvest_take() -> BTreeMap<String, Hit> {
    HARVEST.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default()
}

/// Останавливает сбор и отдаёт только адреса.
pub fn harvest_stop() -> Vec<String> {
    harvest_take().into_keys().collect()
}

/// Накопленное прямо сейчас, без остановки сбора, — для живого счётчика.
pub fn harvest_snapshot() -> BTreeMap<String, Hit> {
    HARVEST.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

pub fn harvest_active() -> bool {
    HARVEST.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Сколько адресов накопилось прямо сейчас. Живой счётчик сбора теперь
/// считает только адреса игры (`pick_game`), так что это нужно лишь тестам.
#[cfg(test)]
pub fn harvest_len() -> usize {
    HARVEST
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.len())
        .unwrap_or(0)
}

/// Скармливает строку лога копилке. Зовётся на каждой строке winws, поэтому
/// сперва самая дешёвая проверка — идёт ли сбор вообще.
pub fn harvest_line(line: &str) {
    let mut g = HARVEST.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = g.as_mut() else { return };
    if let Some(a) = parse_log_addr(line) {
        if is_external(&a.ip) && стоит_собирать(a.proto, a.port) {
            let hit = map.entry(a.ip.to_string()).or_default();
            match a.proto {
                Proto::Udp => hit.udp = true,
                Proto::Tcp => hit.tcp = true,
            }
            // Порты к одному адресу почти всегда одни и те же. Потолок — чтобы
            // шквал подробного лога не раздувал копилку.
            if let Some(sport) = a.sport {
                if hit.sports.len() < 32 {
                    hit.sports.insert((a.proto, sport));
                }
            }
        }
    }
}

/// Локальный сокет из строки `netstat -ano`: протокол, локальный порт, процесс.
///
/// У несоединённого UDP напротив стоит «*:*», и адреса собеседника в такой
/// строке нет. Зато есть ЛОКАЛЬНЫЙ порт — и его хватает, чтобы узнать, чей
/// пакет поймал обход: у пакета в логе тот же исходящий порт.
pub fn parse_local_socket(line: &str) -> Option<(Proto, u16, u32)> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 4 {
        return None;
    }
    let proto = match f[0].to_ascii_uppercase().as_str() {
        "TCP" => Proto::Tcp,
        "UDP" => Proto::Udp,
        _ => return None,
    };
    let pid: u32 = f[f.len() - 1].parse().ok()?;
    let (_, port) = split_addr(f[1])?;
    Some((proto, port, pid))
}

pub fn local_sockets() -> Vec<(Proto, u16, u32)> {
    crate::sys::run("netstat", &["-ano"])
        .lines()
        .filter_map(parse_local_socket)
        .collect()
}

/// Процессы, которые шлют UDP на высокие порты, но игрой не являются.
///
/// Без этого списка игрой назвали бы того, кто громче всех: голос Discord,
/// DHT торрента, браузер, VPN-клиент. Сравниваем имя образа без регистра.
fn не_игра(name: &str) -> bool {
    const ИМЕНА: &[&str] = &[
        "system", "svchost.exe", "lsass.exe", "services.exe", "wininit.exe",
        "winws.exe", "klutz.exe", "tgwsproxyheadless.exe",
        "discord.exe", "discordptb.exe", "discordcanary.exe",
        "chrome.exe", "msedge.exe", "msedgewebview2.exe", "firefox.exe", "opera.exe",
        "browser.exe", "brave.exe", "vivaldi.exe", "arc.exe",
        "telegram.exe", "ayugram.exe", "whatsapp.exe", "zoom.exe", "ms-teams.exe",
        "teams.exe", "skype.exe", "spotify.exe", "steamwebhelper.exe",
        "onedrive.exe", "dropbox.exe",
        "qbittorrent.exe", "utorrent.exe", "bittorrent.exe", "transmission-qt.exe",
        "hiddify.exe", "hiddifycli.exe", "sing-box.exe", "xray.exe", "v2rayn.exe",
        "nekoray.exe", "nekobox.exe", "clash-verge.exe", "happ.exe", "amneziavpn.exe",
        "wireguard.exe", "openvpn.exe",
        "anydesk.exe", "teamviewer.exe", "rustdesk.exe", "parsecd.exe",
    ];
    let n = name.to_ascii_lowercase();
    ИМЕНА.contains(&n.as_str())
}

/// Итог сбора: чей это трафик и что из него берём.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pick {
    /// Процесс игры. `None` — игру среди отправителей не нашли.
    pub process: Option<String>,
    pub addrs: Vec<String>,
    pub udp: bool,
    pub tcp: bool,
    /// Кого видели, но за игру не приняли, — чтобы сказать человеку, чьи
    /// пакеты шли вместо игровых.
    pub others: BTreeSet<String>,
    /// Пакеты были, но ни один не удалось привязать к процессу.
    pub unattributed: bool,
}

/// Кто из отправителей игра и какие адреса её.
///
/// Раньше в список шло всё, что увидел обход, от любого процесса. С Game
/// Filter на всех портах это значит и голос Discord, и торрент. Теперь адрес
/// берётся, только если пакет к нему отправил процесс игры, а игра — тот, у
/// кого больше всего разных адресов среди тех, кто не в списке «не игра».
///
/// Чистая функция: копилку, карту портов и имена собирает вызывающий.
pub fn pick_game(
    hits: &BTreeMap<String, Hit>,
    owners: &HashMap<(Proto, u16), u32>,
    names: &HashMap<u32, String>,
) -> Pick {
    let mut per: BTreeMap<&str, (BTreeSet<&str>, bool, bool)> = BTreeMap::new();
    let mut others = BTreeSet::new();
    let mut привязано = false;
    for (ip, hit) in hits {
        for (proto, sport) in &hit.sports {
            let Some(&pid) = owners.get(&(*proto, *sport)) else { continue };
            if !настоящий_процесс(pid) {
                continue;
            }
            let Some(name) = names.get(&pid) else { continue };
            привязано = true;
            if не_игра(name) {
                others.insert(name.clone());
                continue;
            }
            let e = per.entry(name.as_str()).or_default();
            e.0.insert(ip.as_str());
            match proto {
                Proto::Udp => e.1 = true,
                Proto::Tcp => e.2 = true,
            }
        }
    }
    // При равенстве — по имени, чтобы выбор не плясал от запуска к запуску.
    let best = per
        .into_iter()
        .max_by(|a, b| (a.1).0.len().cmp(&(b.1).0.len()).then_with(|| b.0.cmp(a.0)));
    match best {
        Some((name, (ips, udp, tcp))) => Pick {
            process: Some(name.to_string()),
            addrs: ips.into_iter().map(str::to_string).collect(),
            udp,
            tcp,
            others,
            unattributed: false,
        },
        None => Pick { others, unattributed: !привязано && !hits.is_empty(), ..Default::default() },
    }
}

/// Каким оставить Game Filter после удачного сбора.
///
/// Собранные адреса без фильтра лежат без дела: игровые профили конфигов
/// привязаны к его портам. Включаем то, по чему игра реально ходила, и не
/// отбираем того, что человек включал сам.
pub fn режим_фильтра(было: &str, udp: bool, tcp: bool) -> &'static str {
    let udp = udp || было == "udp" || было == "all";
    let tcp = tcp || было == "tcp" || было == "all";
    match (udp, tcp) {
        (true, true) => "all",
        (true, false) => "udp",
        (false, true) => "tcp",
        (false, false) => "off",
    }
}

/// Порты, по которым ходит веб помимо 80 и 443.
///
/// Это альтернативные HTTPS-порты Cloudflare, и они не выдуманы: ровно этот
/// список стоит в веб-фильтре конфигов релиза рядом с 80 и 443. По ним
/// ходит Discord, и на живом тесте оттуда в игровой список утёк
/// 104.29.153.0/24 — сеть Cloudflare, к играм отношения не имеющая.
///
/// Адресу сайта в игровом списке не место: свои профили к нему применяются
/// и так, а попав сюда, он получил бы вдобавок настройки игрового, которые
/// рассчитаны совсем на другой трафик.
fn веб_порт(port: u16) -> bool {
    // Полный набор альтернативных портов Cloudflare, а не только те, что
    // попались на тесте: HTTP — 8080, 8880, 2052, 2082, 2086, 2095;
    // HTTPS — 2053, 2083, 2087, 2096, 8443. Игровых среди них нет.
    matches!(
        port,
        80 | 443 | 2052 | 2053 | 2082 | 2083 | 2086 | 2087 | 2095 | 2096 | 8080 | 8443 | 8880
    )
}

/// Стоит ли класть в игровой список адрес с этого порта и протокола.
///
/// Правило разное для TCP и UDP, и вот почему. Снятый с Valorant трафик
/// показал: по TCP он ходит ТОЛЬКО в веб — Cloudflare на 443, чат на 5223,
/// античит на 8443. Ни одного игрового адреса по TCP там нет вовсе, зато
/// пролезала чужая сеть Cloudflare. А у CS2 по TCP есть настоящий игровой
/// адрес — менеджер соединений Steam на 27018.
///
/// Значит для TCP нужен известный игровой диапазон, а не «всё, кроме
/// веба»: слишком много веба ходит по нестандартным портам, и каждый раз
/// он оказывается в игровом списке. Для UDP наоборот — там почти не бывает
/// ничего, кроме игр и QUIC, и ограничивать диапазоном значило бы
/// пропустить игру на неизвестном порту.
fn стоит_собирать(proto: Proto, port: u16) -> bool {
    if port < 1024 || веб_порт(port) || служебный_порт(port) {
        return false;
    }
    match proto {
        Proto::Udp => true,
        Proto::Tcp => игровой_порт(port),
    }
}

// ─────────── блок адресов игр внутри ipset-all.txt ───────────
//
// Список адресов у zapret один — `lists/ipset-all.txt`, и его же целиком
// перезаписывает кнопка «Обновить список IPSet». Держать свои адреса
// отдельным файлом нельзя: конфиги релиза про него не знают. Поэтому свои
// строки живут внутри общего файла, обрамлённые метками, и обе стороны —
// и сканер, и обновление списка — блок берегут.

pub const BLOCK_START: &str = "# ─── klutz: адреса игр, собрано сканированием ───";
pub const BLOCK_END: &str = "# ─── klutz: конец блока ───";

// ─────────── группы внутри блока ───────────
//
// Список для winws — это просто строки сетей. Но человеку нужно знать, чьи
// они и когда пойманы, иначе список из тридцати семи строк ничем не
// отличается от случайного набора, и решить, что из него убрать, нельзя.
//
// Храним это строками-комментариями прямо в блоке: winws их не видит
// (`extract_block` пропускает всё, что начинается с #), а мы собираем из
// них группы. Отдельный файл рядом рассыпался бы при первой же ручной
// правке списка.

const GROUP_TAG: &str = "# klutz-группа ";
const SKIP_TAG: &str = "# klutz-пропущено ";

/// Сети одного оператора, пойманные одним сбором.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct Group {
    /// Номер оператора без «AS». Пусто — оператор неизвестен.
    pub asn: String,
    /// Имя оператора. Пусто — справочник промолчал.
    pub name: String,
    /// Когда собрана, миллисекунды эпохи. 0 — неизвестно.
    pub at: u64,
    pub nets: Vec<String>,
    /// Сети лежали в файле без строки-маркера — так писала версия, где
    /// групп ещё не было. Это ВАЖНО отличать от «справочник промолчал»:
    /// там мы спрашивали и не узнали, а здесь не спрашивали вовсе, и
    /// сказать про такие сети «сеть вокруг пойманного адреса» — неправда.
    /// Выяснить оператора для них можно в любой момент, двумя запросами.
    pub legacy: bool,
}

/// Адрес, который в список не пошёл.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Skipped {
    pub addr: String,
    pub asn: String,
    pub name: String,
    pub prefixes: usize,
}

/// Разбирает блок на группы и пропущенное.
///
/// Сети без своей группы не теряются: они попадают в безымянную группу.
/// Так переживают и старые файлы, где групп ещё не было, и ручную правку.
pub fn parse_groups(text: &str) -> (Vec<Group>, Vec<Skipped>) {
    let mut groups: Vec<Group> = Vec::new();
    let mut skipped = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t == BLOCK_START {
            inside = true;
            continue;
        }
        if t == BLOCK_END {
            inside = false;
            continue;
        }
        if !inside || t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix(GROUP_TAG) {
            // AS<номер> <время> <имя через пробелы>
            let mut it = rest.splitn(3, ' ');
            let asn = it.next().unwrap_or("").trim_start_matches("AS").to_string();
            let at = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let name = it.next().unwrap_or("").trim().to_string();
            groups.push(Group { asn, name, at, nets: Vec::new(), legacy: false });
            continue;
        }
        if let Some(rest) = t.strip_prefix(SKIP_TAG) {
            // <адрес> AS<номер> <сколько сетей> <имя>
            let mut it = rest.splitn(4, ' ');
            let addr = it.next().unwrap_or("").to_string();
            let asn = it.next().unwrap_or("").trim_start_matches("AS").to_string();
            let prefixes = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let name = it.next().unwrap_or("").trim().to_string();
            if !addr.is_empty() {
                skipped.push(Skipped { addr, asn, name, prefixes });
            }
            continue;
        }
        if t.starts_with('#') {
            continue;
        }
        match groups.last_mut() {
            Some(g) => g.nets.push(t.to_string()),
            None => groups.push(Group {
                nets: vec![t.to_string()],
                legacy: true,
                ..Default::default()
            }),
        }
    }
    groups.retain(|g| !g.nets.is_empty());
    (groups, skipped)
}

/// Собирает файл заново из групп. Чужие строки остаются как были.
pub fn merge_groups(existing: &str, groups: &[Group], skipped: &[Skipped]) -> String {
    let сетей_нет = groups.iter().all(|g| g.nets.is_empty());
    if сетей_нет && skipped.is_empty() {
        // Тот же путь, что и у пустого набора: вернуть заглушку, а не
        // пустой файл, иначе профиль начнёт применяться ко всему подряд.
        return merge_block(existing, &[]);
    }
    let base: Vec<String> = without_block(existing)
        .lines()
        .filter(|l| !l.trim().starts_with("203.0.113.113"))
        .map(|l| l.to_string())
        .collect();
    let mut out = base.join("\r\n").trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\r\n");
    }
    out.push_str(BLOCK_START);
    out.push_str("\r\n");
    if сетей_нет {
        // Сетей не осталось, а пропущенное есть. Без этой строки в файле
        // были бы одни комментарии — для winws это пустой список, то есть
        // «без ограничения по адресу»: обход полез бы в каждый матч.
        out.push_str(EMPTY_STUB);
        out.push_str("\r\n");
    }
    // Сети без оператора идут первыми и БЕЗ строки-маркера: разбор
    // относит к безымянной группе всё, что лежит до первого маркера.
    // Припиши мы им маркер — они стали бы неотличимы от групп, где
    // оператора спрашивали и не узнали.
    for g in groups.iter().filter(|g| g.legacy && !g.nets.is_empty()) {
        for n in &g.nets {
            out.push_str(n);
            out.push_str("\r\n");
        }
    }
    for g in groups.iter().filter(|g| !g.legacy && !g.nets.is_empty()) {
        out.push_str(&format!("{GROUP_TAG}AS{} {} {}", g.asn, g.at, g.name));
        out.push_str("\r\n");
        for n in &g.nets {
            out.push_str(n);
            out.push_str("\r\n");
        }
    }
    for s in skipped {
        out.push_str(&format!(
            "{SKIP_TAG}{} AS{} {} {}",
            s.addr, s.asn, s.prefixes, s.name
        ));
        out.push_str("\r\n");
    }
    out.push_str(BLOCK_END);
    out.push_str("\r\n");
    out
}

/// Адреса из нашего блока. Пусто — блока нет.
pub fn extract_block(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t == BLOCK_START {
            inside = true;
            continue;
        }
        if t == BLOCK_END {
            inside = false;
            continue;
        }
        if inside && !t.is_empty() && !t.starts_with('#') {
            out.push(t.to_string());
        }
    }
    out
}

/// Текст без нашего блока — то, что принадлежит не нам.
pub fn without_block(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t == BLOCK_START {
            inside = true;
            continue;
        }
        if t == BLOCK_END {
            inside = false;
            continue;
        }
        if !inside {
            out.push(line);
        }
    }
    let mut s = out.join("\r\n");
    while s.ends_with("\r\n\r\n") {
        s.truncate(s.len() - 2);
    }
    s
}

/// Собирает файл заново: чужое как было, наш блок в конец.
///
/// Заглушку `203.0.113.113/32` выбрасываем: это «список загружен, но пуст»
/// из TEST-NET-3, и рядом с настоящими адресами она только мешает — режим
/// файла всё равно становится «loaded».
/// Заглушка «список загружен, но пуст». Адрес из TEST-NET-3 (RFC 5737),
/// который не ответит никогда.
///
/// Она не косметика. У winws ПУСТОЙ ipset означает «без ограничения по
/// адресу»: профиль начинает применяться ко всему подряд на своих портах.
/// Для игрового профиля это худший из возможных исходов — обход лезет в
/// каждый матч. Поэтому, убрав свои адреса, мы обязаны вернуть заглушку, а
/// не оставить файл пустым.
pub const EMPTY_STUB: &str = "203.0.113.113/32";

pub fn merge_block(existing: &str, addrs: &[String]) -> String {
    // Заглушку выбрасываем, только когда есть чем её заменить: рядом с
    // настоящими адресами она бессмысленна, а вместо них — необходима.
    let base: Vec<String> = without_block(existing)
        .lines()
        .filter(|l| addrs.is_empty() || !l.trim().starts_with("203.0.113.113"))
        .map(|l| l.to_string())
        .collect();
    let mut out = base.join("\r\n").trim_end().to_string();
    if addrs.is_empty() {
        // Своих адресов нет и чужих строк не осталось — возвращаем заглушку.
        // Пустой файл тут значит «применяться ко всему», и кнопка «Убрать»
        // молча включала бы обход на весь игровой трафик.
        if out.is_empty() {
            return format!("{EMPTY_STUB}\r\n");
        }
        out.push_str("\r\n");
        return out;
    }
    if !out.is_empty() {
        out.push_str("\r\n");
    }
    out.push_str(BLOCK_START);
    out.push_str("\r\n");
    for a in addrs {
        out.push_str(a);
        out.push_str("\r\n");
    }
    out.push_str(BLOCK_END);
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn строка_из_списка_проверяется_до_справочника() {
        for ok in ["162.249.72.1", "162.249.72.0/24", "2001:db8::1", "2001:db8::/32"] {
            assert!(адрес_или_сеть(ok), "{ok} — нормальный адрес или сеть");
        }
        for bad in ["", "foo", "1.2.3.4&x=1", "1.2.3.0/33", "2001:db8::/129", "1.2.3.0/", "/24", "1.2.3.4 5.6.7.8"] {
            assert!(!адрес_или_сеть(bad), "{bad:?} не должно уходить ни в справочник, ни в список");
        }
    }

    #[test]
    fn сбор_и_тесты_не_идут_одновременно() {
        let state = crate::state::AppState::new();
        let first = ScanGuard::acquire(&state).expect("первый сбор должен начаться");
        assert!(scan_busy());
        assert!(ScanGuard::acquire(&state).is_err(), "второй сбор поверх первого");
        drop(first);
        assert!(!scan_busy(), "признак снимается сам");

        *state.testing.lock().unwrap() = true;
        assert!(ScanGuard::acquire(&state).is_err(), "сбор во время прогона тестов");
        assert!(!scan_busy(), "отказ не должен оставлять признак");
    }

    #[test]
    fn разбор_строк_netstat_не_зависит_от_языка() {
        // Английская система: у TCP есть состояние.
        let c = parse_netstat_line("  TCP    192.168.1.5:51000    104.16.0.1:443    ESTABLISHED    4242").unwrap();
        assert_eq!(c.pid, 4242);
        assert_eq!(c.proto, Proto::Tcp);
        assert_eq!(c.ip.to_string(), "104.16.0.1");
        assert_eq!(c.port, 443);

        // Русская: то же самое, состояние переведено — разбор не должен
        // этого замечать.
        let c = parse_netstat_line("  TCP    192.168.1.5:51001    162.159.135.232:27018    УСТАНОВЛЕНО    777").unwrap();
        assert_eq!((c.pid, c.port), (777, 27018));

        // UDP: столбца состояния нет вовсе.
        let c = parse_netstat_line("  UDP    0.0.0.0:50000    8.8.8.8:27015    1234").unwrap();
        assert_eq!(c.proto, Proto::Udp);
        assert_eq!((c.pid, c.port), (1234, 27015));
    }

    #[test]
    fn ipv6_и_мусор_разбираются_без_паники() {
        let c = parse_netstat_line("  TCP    [::1]:1000    [2606:4700::1]:443    ESTABLISHED    9").unwrap();
        assert_eq!(c.ip.to_string(), "2606:4700::1");
        assert_eq!(c.port, 443);

        // «Ни с кем» — это не адрес.
        assert!(parse_netstat_line("  UDP    0.0.0.0:500    *:*    900").is_none());
        // Заголовки и пустые строки.
        assert!(parse_netstat_line("Active Connections").is_none());
        assert!(parse_netstat_line("  Proto  Local Address  Foreign Address  State  PID").is_none());
        assert!(parse_netstat_line("").is_none());
        assert!(parse_netstat_line("   ").is_none());
        // Номер процесса не число.
        assert!(parse_netstat_line("  TCP  1.1.1.1:1  2.2.2.2:2  ESTABLISHED  нет").is_none());
    }

    #[test]
    fn в_список_попадают_только_чужие_адреса() {
        let внешние = ["104.16.0.1", "162.159.135.232", "8.8.8.8", "2606:4700::1"];
        for ip in внешние {
            assert!(is_external(&ip.parse().unwrap()), "{ip} должен пройти");
        }
        // Попади сюда адрес роутера — обход начал бы заворачивать трафик
        // внутрь домашней сети.
        let свои = [
            "127.0.0.1", "192.168.1.1", "10.0.0.5", "172.16.0.1",
            "169.254.1.1", "224.0.0.1", "0.0.0.0",
            "100.64.0.1",      // CGNAT — адреса провайдера
            "198.18.0.1",      // fake-IP прокси
            "::1", "fe80::1", "fc00::1",
        ];
        for ip in свои {
            assert!(!is_external(&ip.parse().unwrap()), "{ip} не должен пройти");
        }
    }

    #[test]
    fn разбор_строки_tasklist() {
        assert_eq!(parse_tasklist_line(r#""cs2.exe","4242","Console","1","1 234 КБ""#), Some(4242));
        // Имя с пробелом — ради этого и CSV.
        assert_eq!(parse_tasklist_line(r#""Rocket League.exe","77","Console","1","10 КБ""#), Some(77));
        // Сообщение «нет задач» в любом переводе.
        assert_eq!(parse_tasklist_line("INFO: No tasks are running which match the specified criteria."), None);
        assert_eq!(parse_tasklist_line(""), None);
    }

    #[test]
    fn игру_узнаём_по_нестандартным_портам() {
        use std::collections::HashMap;
        let names: HashMap<u32, String> = [
            (101, "chrome.exe".to_string()),
            (202, "cs2.exe".to_string()),
            (303, "svchost.exe".to_string()),
        ]
        .into_iter()
        .collect();
        let c = |pid, ip: &str, port| Conn {
            pid,
            proto: Proto::Tcp,
            ip: ip.parse().unwrap(),
            port,
        };
        let conns = vec![
            // Браузер держит внешних соединений больше всех — и всё равно
            // не должен выигрывать.
            c(101, "104.16.0.1", 443),
            c(101, "104.16.0.2", 443),
            c(101, "104.16.0.3", 443),
            c(101, "104.16.0.4", 443),
            c(303, "20.1.1.1", 443),
            // А у игры свой порт.
            c(202, "162.159.135.232", 27018),
        ];
        assert_eq!(guess_game(&conns, &names).as_deref(), Some("cs2.exe"));
    }

    #[test]
    fn без_признаков_игры_честно_отвечаем_что_не_нашли() {
        use std::collections::HashMap;
        // Никто не ходит по игровым портам: только веб. Прежняя версия
        // выбирала «самого активного», и это было выдумкой — теперь
        // ответ «не нашли», а интерфейс попросит запустить игру.
        let names: HashMap<u32, String> = [
            (101, "chrome.exe".to_string()),
            (202, "SomeApp.exe".to_string()),
        ]
        .into_iter()
        .collect();
        let c = |pid, ip: &str, port| Conn { pid, proto: Proto::Tcp, ip: ip.parse().unwrap(), port };
        let conns = vec![c(101, "1.1.1.1", 443), c(101, "1.1.1.2", 80), c(202, "8.8.8.8", 443)];
        assert_eq!(guess_game(&conns, &names), None);
        assert!(candidates(&conns, &names).is_empty());
    }

    #[test]
    fn псевдопроцессы_в_кандидаты_не_идут() {
        use std::collections::HashMap;
        // Ровно то, что вылезло на тесте: netstat отдал закрывающиеся
        // соединения на игровых портах, повесив их на PID 0, и он вышел
        // в кандидаты впереди Steam.
        let names: HashMap<u32, String> = [
            (0, "System Idle Process".to_string()),
            (4, "System".to_string()),
            (100, "steam.exe".to_string()),
        ]
        .into_iter()
        .collect();
        let c = |pid, ip: &str, port| Conn { pid, proto: Proto::Tcp, ip: ip.parse().unwrap(), port };
        let conns = vec![
            c(0, "104.16.0.1", 1119),
            c(0, "104.16.0.2", 27018),
            c(4, "104.16.0.3", 27015),
            c(100, "155.133.226.76", 27023),
        ];
        let got = candidates(&conns, &names);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].name, "steam.exe");
    }

    #[test]
    fn служебный_порт_за_игру_не_сходит() {
        use std::collections::HashMap;
        // Ровно тот случай, на котором эвристика однажды и попалась:
        // служба HP стучалась на 5228 — это уведомления Google, не игра.
        let names: HashMap<u32, String> = [(404, "happd.exe".to_string())].into_iter().collect();
        let conns = vec![Conn {
            pid: 404,
            proto: Proto::Tcp,
            ip: "142.250.153.188".parse().unwrap(),
            port: 5228,
        }];
        assert_eq!(guess_game(&conns, &names), None);
    }

    #[test]
    fn udp_на_высоком_порту_весит_больше_веба() {
        use std::collections::HashMap;
        let names: HashMap<u32, String> = [
            (101, "launcher.exe".to_string()),
            (202, "VALORANT-Win64-Shipping.exe".to_string()),
        ]
        .into_iter()
        .collect();
        // Лаунчер держит кучу TCP на игровом порту, игра — один UDP.
        let mut conns: Vec<Conn> = (0..3)
            .map(|i| Conn {
                pid: 101,
                proto: Proto::Tcp,
                ip: format!("104.16.0.{i}").parse().unwrap(),
                port: 7000,
            })
            .collect();
        conns.push(Conn {
            pid: 202,
            proto: Proto::Udp,
            ip: "162.159.1.1".parse().unwrap(),
            port: 5060,
        });
        let c = candidates(&conns, &names);
        // Оба попали в кандидаты — выбор остаётся за человеком.
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(c.iter().any(|x| x.name.contains("VALORANT")), "{c:?}");
        assert!(c.iter().any(|x| x.name == "launcher.exe"), "{c:?}");
    }

    #[test]
    fn внутренние_адреса_в_догадку_не_идут() {
        use std::collections::HashMap;
        let names: HashMap<u32, String> = [(202, "game.exe".to_string())].into_iter().collect();
        let conns = vec![Conn {
            pid: 202,
            proto: Proto::Udp,
            ip: "192.168.1.1".parse().unwrap(),
            port: 27015,
        }];
        assert_eq!(guess_game(&conns, &names), None);
    }

    #[test]
    fn пояснение_не_выдаёт_пустой_скан_за_результат() {
        assert!(describe(false, 0, 5).contains("не найден"));
        assert!(describe(true, 0, 5).contains("наружу пока не ходила"));
        assert!(describe(true, 12, 5).contains("12"));
        // И честно говорит, что список неполон, если игру режут.
        assert!(describe(true, 12, 5).contains("ДОШЛА"));
        assert!(describe(false, 0, 0).contains("не запускалось"));
    }

    #[test]
    fn блок_выделяется_и_не_задевает_чужое() {
        let текст = [
            "1.1.1.1",
            "2.2.2.2",
            BLOCK_START,
            "104.16.0.1",
            "162.159.135.232",
            BLOCK_END,
            "3.3.3.3",
        ]
        .join("\r\n");
        assert_eq!(extract_block(&текст), vec!["104.16.0.1", "162.159.135.232"]);
        let без = without_block(&текст);
        assert!(без.contains("1.1.1.1") && без.contains("3.3.3.3"));
        assert!(!без.contains("104.16.0.1"), "{без}");
        assert!(!без.contains("klutz"), "метки тоже наши: {без}");
    }

    #[test]
    fn обновление_списка_не_стирает_собранное() {
        // Ровно тот случай, ради которого метки и появились: скачанный
        // список кладётся вместо чужого, а наш блок переносится.
        let старое = [BLOCK_START, "104.16.0.1", BLOCK_END].join("\r\n");
        let свои = extract_block(&старое);
        let скачанное = ["8.8.8.8", "9.9.9.9"].join("\r\n");
        let новое = merge_block(&скачанное, &свои);
        assert!(новое.contains("8.8.8.8") && новое.contains("9.9.9.9"), "{новое}");
        assert_eq!(extract_block(&новое), vec!["104.16.0.1"], "{новое}");
    }

    #[test]
    fn заглушка_пустого_списка_уступает_настоящим_адресам() {
        // 203.0.113.113/32 означает «список загружен, но пуст». Рядом с
        // настоящими адресами ей делать нечего.
        let было = "203.0.113.113/32";
        let стало = merge_block(было, &["104.16.0.1".to_string()]);
        assert!(!стало.contains("203.0.113"), "{стало}");
        assert!(стало.contains("104.16.0.1"), "{стало}");
    }

    #[test]
    fn убрав_адреса_возвращаем_заглушку_а_не_пустоту() {
        // Пустой ipset у winws значит «применяться ко всему». Если кнопка
        // «Убрать» оставит файл пустым, обход полезет в каждый матч — то
        // есть станет хуже, чем было до сбора.
        let было = [BLOCK_START, "146.66.155.0/24", BLOCK_END].join("\r\n");
        let стало = merge_block(&было, &[]);
        assert!(стало.contains(EMPTY_STUB), "{стало:?}");
        assert!(!стало.contains("146.66.155"), "{стало:?}");
        // А когда есть настоящие адреса, заглушка не нужна.
        let стало = merge_block(EMPTY_STUB, &["146.66.155.0/24".to_string()]);
        assert!(!стало.contains(EMPTY_STUB), "{стало:?}");
    }

    #[test]
    fn пустой_набор_убирает_блок_целиком() {
        let текст = ["7.7.7.7", BLOCK_START, "1.2.3.4", BLOCK_END].join("\r\n");
        let стало = merge_block(&текст, &[]);
        assert!(стало.contains("7.7.7.7"), "{стало}");
        assert!(!стало.contains("1.2.3.4"), "{стало}");
        assert!(!стало.contains("klutz"), "{стало}");
        // И на совсем пустом входе получаем заглушку, а не пустоту:
        // пустой список у winws означает «применяться ко всему».
        assert_eq!(merge_block("", &[]), format!("{EMPTY_STUB}\r\n"));
    }

    #[test]
    fn адрес_из_строки_лога_winws() {
        // Форма conntrack.
        let a = parse_log_addr("UDP [192.168.1.5]:52000 => [162.159.135.232]:27015 : t0=1").unwrap();
        let (ip, port) = (a.ip, a.port);
        assert_eq!(ip.to_string(), "162.159.135.232");
        assert_eq!(port, 27015);

        // Форма «dpi desync». Она важнее: у неё dst стоит явно, и её мы
        // проверяем первой.
        let a = parse_log_addr("dpi desync src=192.168.1.5:52000 dst=104.16.0.1:443").unwrap();
        let (ip, port) = (a.ip, a.port);
        assert_eq!(ip.to_string(), "104.16.0.1");
        assert_eq!(port, 443);

        // IPv6 в скобках.
        let a = parse_log_addr("TCP [fe80::1]:1 => [2606:4700::1]:443 : x").unwrap();
        let (ip, _) = (a.ip, a.port);
        assert_eq!(ip.to_string(), "2606:4700::1");
    }

    #[test]
    fn настоящая_строка_пакета_winws_разбирается() {
        // Ровно так winws собирает её из своих кусков: «IP4: %s», «%s => %s»,
        // «%s proto=%s ttl=%u», «sport=%u dport=%u». Порт стоит ОТДЕЛЬНЫМ
        // полем, и на этом разбор сперва и спотыкался — а это основная
        // форма, ради которой всё затевалось.
        let a = parse_log_addr(
            "IP4: 192.168.1.16 => 104.21.43.64 proto=udp ttl=128 sport=50282 dport=27015",
        )
        .unwrap();
        assert_eq!(a.ip.to_string(), "104.21.43.64");
        assert_eq!(a.port, 27015);
        assert_eq!(a.proto, Proto::Udp);
        assert_eq!(a.sport, Some(50282), "исходящий порт нужен для привязки к процессу");

        // TCP-вариант с флагами.
        let a = parse_log_addr(
            "IP4: 192.168.1.16 => 146.66.155.73 proto=tcp ttl=128 sport=51000 dport=27018 flags=S seq=1 ack_seq=0",
        )
        .unwrap();
        assert_eq!(a.port, 27018);
        assert_eq!(a.proto, Proto::Tcp);
    }

    #[test]
    fn посторонние_строки_лога_не_дают_адресов() {
        for line in [
            "",
            "packet contains TLS ClientHello",
            "Window size change 64240 => 512",
            "rewrite original packet ttl 128 => 64",
            "sending 6 dups with ttl rewrite 128 => 64",
            "hostname: discord.com",
        ] {
            assert_eq!(parse_log_addr(line), None, "{line:?}");
        }
    }

    /// Копилка одна на процесс, а тестов, которые её трогают, несколько.
    /// Cargo гоняет их параллельно, и без этого замка они отбирали бы
    /// накопленное друг у друга — тест мигал бы через раз.
    static ТЕСТ_КОПИЛКИ: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Живая проверка цепочки: адрес -> оператор -> все его сети.
    /// cargo test -- --ignored живой_оператор --nocapture
    #[test]
    #[ignore]
    fn живой_оператор_по_адресу() {
        // Пойманные на этой машине адреса Riot и Valve.
        for ip in ["185.40.64.5", "162.249.72.10", "146.66.155.73"] {
            let Operator::Nets { nets: pfx, .. } = operator_of(ip) else {
                panic!("{ip}: оператор не определился");
            };
            println!("{ip}: {} сетей", pfx.len());
            assert!(!pfx.is_empty());
            assert!(pfx.iter().all(|p| p.contains('/')));
        }
        // А облако разворачивать нельзя — у него тысячи сетей.
        let cf = operator_of("104.29.153.1");
        println!("Cloudflare: {cf:?}");
        assert!(matches!(cf, Operator::Cloud { .. }), "облако должно опознаваться");
    }

    #[test]
    fn группы_переживают_запись_и_чтение() {
        // Список для winws — просто строки сетей, а человеку нужно знать,
        // чьи они и когда пойманы. Держим это комментариями внутри блока:
        // winws их не видит, а мы собираем из них группы.
        let groups = vec![
            Group {
                asn: "6507".into(),
                name: "Riot Games, Inc".into(),
                at: 1_757_712_345_000,
                nets: vec!["162.249.72.0/21".into(), "185.40.64.0/22".into()],
                legacy: false,
            },
            Group {
                asn: String::new(),
                name: String::new(),
                at: 0,
                nets: vec!["1.2.3.0/24".into()],
                legacy: false,
            },
        ];
        let skipped = vec![Skipped {
            addr: "104.29.153.1".into(),
            asn: "13335".into(),
            name: "Cloudflare, Inc.".into(),
            prefixes: 2395,
        }];

        let текст = merge_groups("чужая строка", &groups, &skipped);
        assert!(текст.starts_with("чужая строка"), "чужое сохраняется: {текст:?}");
        // winws читает только сети: комментарии он пропускает, как и мы.
        assert_eq!(
            extract_block(&текст),
            vec!["162.249.72.0/21", "185.40.64.0/22", "1.2.3.0/24"]
        );

        let (назад, пропуск) = parse_groups(&текст);
        assert_eq!(назад, groups, "группы вернулись как были");
        assert_eq!(пропуск, skipped, "пропущенное вернулось как было");
    }

    #[test]
    fn старый_список_отличается_от_неузнанного_оператора() {
        // Файл прежней версии: сети лежат сразу под заголовком блока, без
        // строки-маркера. Оператор там не «не определился» — его никогда и
        // не спрашивали, и подпись про сеть вокруг адреса для них ложь.
        let старый = [BLOCK_START, "103.219.128.0/22", "185.40.64.0/22", BLOCK_END].join("\r\n");
        let (g, _) = parse_groups(&старый);
        assert_eq!(g.len(), 1);
        assert!(g[0].legacy, "нет маркера — значит запись прежней версии");
        assert_eq!(g[0].nets.len(), 2);

        // Признак обязан пережить перезапись файла: иначе после первой же
        // правки списка эти сети станут неотличимы от неузнанных.
        let снова = merge_groups("", &g, &[]);
        assert!(!снова.contains(GROUP_TAG), "маркера у таких сетей быть не должно");
        assert!(parse_groups(&снова).0[0].legacy, "признак потерян");

        // А группа, записанная нами, маркер имеет и старой не считается.
        let наша = vec![Group {
            asn: "6507".into(),
            name: "Riot Games, Inc".into(),
            at: 1,
            nets: vec!["1.2.3.0/24".into()],
            legacy: false,
        }];
        assert!(!parse_groups(&merge_groups("", &наша, &[])).0[0].legacy);
    }

    #[test]
    fn чужую_сеть_оператору_не_приписываем() {
        // Определяем оператора по одной сети, но подписать им можно только
        // то, что он действительно объявляет.
        let наши = vec![
            "103.219.128.0/22".to_string(),
            "185.40.64.0/22".to_string(),
            "8.8.8.0/24".to_string(),
        ];
        let объявлено = vec!["103.219.128.0/22".to_string(), "185.40.64.0/22".to_string()];
        let (его, чужие) = partition_by(&наши, &объявлено);
        assert_eq!(его, ["103.219.128.0/22", "185.40.64.0/22"]);
        assert_eq!(чужие, ["8.8.8.0/24"], "чужое остаётся непривязанным");
    }

    #[test]
    fn без_сетей_но_с_пропущенным_заглушка_остаётся() {
        // Иначе в файле оказались бы одни комментарии, а пустой ipset у
        // winws значит «без ограничения по адресу» — обход полез бы в
        // каждый матч.
        let skipped = vec![Skipped {
            addr: "104.29.153.1".into(),
            asn: "13335".into(),
            name: "Cloudflare, Inc.".into(),
            prefixes: 2395,
        }];
        let текст = merge_groups("", &[], &skipped);
        assert_eq!(extract_block(&текст), vec![EMPTY_STUB], "{текст:?}");
        assert_eq!(parse_groups(&текст).1, skipped, "пропущенное на месте");
    }

    #[test]
    fn имя_оператора_без_кода_реестра() {
        // Справочник отдаёт «RIOT-NA1 - Riot Games, Inc». Код реестра
        // человеку ничего не говорит — показываем вторую половину.
        let j = r#"{"data":{"holder":"RIOT-NA1 - Riot Games, Inc"}}"#;
        assert_eq!(parse_holder(j).as_deref(), Some("Riot Games, Inc"));
        let c = r#"{"data":{"holder":"CLOUDFLARENET - Cloudflare, Inc."}}"#;
        assert_eq!(parse_holder(c).as_deref(), Some("Cloudflare, Inc."));
        // Разделителя может не быть — тогда как есть.
        let o = r#"{"data":{"holder":"SOMEISP"}}"#;
        assert_eq!(parse_holder(o).as_deref(), Some("SOMEISP"));
        assert_eq!(parse_holder("{}"), None);
        assert_eq!(parse_holder(r#"{"holder":""}"#), None);
    }

    #[test]
    fn лишний_адрес_убирается_поштучно() {
        // Сбор берёт адреса пачкой и иногда прихватывает чужое. Сбрасывать
        // весь список ради одной строки — значит играть ещё один матч.
        let root = std::env::temp_dir().join(format!("klutz-one-{}", std::process::id()));
        let lists = root.join("lists");
        std::fs::create_dir_all(&lists).unwrap();
        std::fs::write(lists.join("ipset-all.txt"), "").unwrap();

        let nets = vec!["162.249.72.0/21".to_string(), "104.29.153.0/24".to_string()];
        save_ips_to(&root, Target::Bypass, &nets).unwrap();

        // Убираем ровно одну строку, соседняя остаётся.
        let убрано = remove_from(&root, Target::Bypass, &["104.29.153.0/24".to_string()]).unwrap();
        assert_eq!(убрано, 1);
        assert_eq!(saved_ips_in(&root, Target::Bypass), vec!["162.249.72.0/21".to_string()]);

        // Чего в списке нет, то и не убирается — и это не ошибка записи.
        assert_eq!(remove_from(&root, Target::Bypass, &["8.8.8.0/24".to_string()]).unwrap(), 0);

        // Последний адрес уходит — возвращается заглушка, иначе пустой
        // ipset-all означал бы «применяться ко всему».
        remove_from(&root, Target::Bypass, &["162.249.72.0/21".to_string()]).unwrap();
        assert!(saved_ips_in(&root, Target::Bypass).is_empty());
        let текст = std::fs::read_to_string(lists.join("ipset-all.txt")).unwrap();
        assert!(текст.contains(EMPTY_STUB), "{текст:?}");

        assert!(changed_at(&root, Target::Bypass).is_some(), "время файла читается");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn адрес_не_лежит_в_обоих_списках_сразу() {
        // Списки противоположны по смыслу, и winws читает их вместе. Адрес,
        // попавший в оба, не обходится — а человек видит его в собранных и
        // ждёт обратного.
        let root = std::env::temp_dir().join(format!("klutz-both-{}", std::process::id()));
        let lists = root.join("lists");
        std::fs::create_dir_all(&lists).unwrap();
        std::fs::write(lists.join("ipset-all.txt"), format!("{EMPTY_STUB}\r\n")).unwrap();
        std::fs::write(lists.join("ipset-exclude-user.txt"), "").unwrap();

        // Сети приходят готовыми, справочник для них не нужен — тест офлайн.
        let nets = vec!["162.249.72.0/21".to_string(), "185.40.64.0/22".to_string()];

        save_ips_to(&root, Target::Bypass, &nets).unwrap();
        assert_eq!(saved_ips_in(&root, Target::Bypass), nets);
        assert!(saved_ips_in(&root, Target::Skip).is_empty());

        // «Не трогать»: сети переезжают, и в обходе их не остаётся.
        save_ips_to(&root, Target::Skip, &nets).unwrap();
        assert_eq!(saved_ips_in(&root, Target::Skip), nets);
        assert!(saved_ips_in(&root, Target::Bypass).is_empty(), "остались в обходе");
        // Пустой ipset-all значит «применяться ко всему» — заглушка обязана
        // вернуться на место.
        let all = std::fs::read_to_string(lists.join("ipset-all.txt")).unwrap();
        assert!(all.contains(EMPTY_STUB), "{all:?}");
        // А пустой список исключений заглушки не требует.
        let skip = std::fs::read_to_string(lists.join("ipset-exclude-user.txt")).unwrap();
        assert!(!skip.contains(EMPTY_STUB), "{skip:?}");

        // И обратно: сбор забирает их из исключений.
        save_ips_to(&root, Target::Bypass, &nets).unwrap();
        assert!(saved_ips_in(&root, Target::Skip).is_empty(), "остались в исключениях");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn облачный_адрес_не_попадает_в_список_вообще() {
        // Раньше «не выяснили» и «это облако» вели к одному исходу — /24
        // вокруг адреса. Для облака это чужая сеть в игровом списке: ровно
        // так 104.29.153.0/24 (Cloudflare) уезжала под десинхронизацию,
        // возвращаясь после каждой ручной чистки.
        let сети: Vec<String> = (0..36).map(|i| format!("10.{i}.0.0/24")).collect();
        assert!(matches!(decide("6507", сети), Operator::Nets { .. }), "Riot берём");

        let облако: Vec<String> = (0..2395).map(|i| format!("10.{}.{}.0/24", i / 256, i % 256)).collect();
        match decide("13335", облако) {
            Operator::Cloud { asn, prefixes } => {
                assert_eq!(asn, "13335");
                assert_eq!(prefixes, 2395);
            }
            иное => panic!("облако должно опознаваться, а не {иное:?}"),
        }

        // Пустой ответ — это «не выяснили», и тогда /24 остаётся: адрес
        // настоящий, просто справочник промолчал.
        assert_eq!(decide("6507", Vec::new()), Operator::Unknown);
    }

    #[test]
    fn облако_целиком_в_список_не_тащим() {
        // Замерено: Riot объявляет 36 сетей, Valve 45 — их берём целиком.
        assert!(should_expand(36), "Riot");
        assert!(should_expand(45), "Valve");
        // А Cloudflare 2395, Google 1233, Amazon 18020 — это облака, и
        // затащить их в игровой список значит направить обход на
        // пол-интернета.
        assert!(!should_expand(2395), "Cloudflare");
        assert!(!should_expand(1233), "Google");
        assert!(!should_expand(18020), "Amazon");
        // Пустой ответ — не повод ничего разворачивать.
        assert!(!should_expand(0));
    }

    #[test]
    fn разбор_ответов_справочника() {
        // Форма ответа про сеть.
        let j = r#"{"data":{"prefix":"185.40.64.0/24","asns":["6507"]}}"#;
        assert_eq!(parse_asn(j).as_deref(), Some("6507"));
        // Несколько операторов — берём первого.
        let j2 = r#"{"data":{"asns":["32590","1234"]}}"#;
        assert_eq!(parse_asn(j2).as_deref(), Some("32590"));
        // Мусор не должен ломать.
        assert_eq!(parse_asn("{}"), None);
        assert_eq!(parse_asn(""), None);
        assert_eq!(parse_asn(r#"{"asns":[]}"#), None);

        // Форма ответа про сети оператора.
        let p = r#"{"data":{"prefixes":[{"prefix":"162.249.72.0/21"},{"prefix":"2a04::/32"},{"prefix":"185.40.64.0/24"},{"prefix":"162.249.72.0/21"}]}}"#;
        let got = parse_prefixes(p);
        assert_eq!(got, vec!["162.249.72.0/21", "185.40.64.0/24"], "IPv6 и повторы отсеяны");
        assert!(parse_prefixes("{}").is_empty());
    }

    #[test]
    fn адрес_расширяется_до_подсети() {
        // Пойманный сервер — один из пула: следующий матч даст соседний.
        assert_eq!(to_subnet("146.66.155.73"), "146.66.155.0/24");
        assert_eq!(to_subnet("155.133.226.68"), "155.133.226.0/24");
        // Уже сеть или IPv6 — оставляем как есть.
        assert_eq!(to_subnet("2606:4700::1"), "2606:4700::1");
        assert_eq!(to_subnet("не адрес"), "не адрес");
    }

    #[test]
    fn по_tcp_берём_только_игровые_порты_а_по_udp_всё() {
        // Снято с живого Valorant: по TCP он ходит ТОЛЬКО в веб, и оттуда
        // в игровой список лезла чужая сеть Cloudflare. Игрового адреса по
        // TCP у него нет вовсе.
        assert!(!стоит_собирать(Proto::Tcp, 443));
        assert!(!стоит_собирать(Proto::Tcp, 5223), "чат Riot — это не игра");
        assert!(!стоит_собирать(Proto::Tcp, 8443), "античит по HTTPS");
        assert!(!стоит_собирать(Proto::Tcp, 49152), "случайный высокий порт");
        // А у CS2 по TCP игровой адрес есть — менеджер соединений Steam.
        assert!(стоит_собирать(Proto::Tcp, 27018));
        assert!(стоит_собирать(Proto::Tcp, 1119), "Battle.net");
        // По UDP берём широко: там почти не бывает ничего, кроме игр, и
        // ограничив диапазоном, мы пропустили бы игру на чужом порту.
        assert!(стоит_собирать(Proto::Udp, 27015));
        assert!(стоит_собирать(Proto::Udp, 7000));
        assert!(стоит_собирать(Proto::Udp, 61337), "неизвестный порт — всё равно берём");
        // Но и там веб со служебным не нужны.
        assert!(!стоит_собирать(Proto::Udp, 443), "QUIC — это веб");
        assert!(!стоит_собирать(Proto::Udp, 53), "DNS");
    }

    #[test]
    fn обычный_сбор_тоже_отсеивает_веб() {
        // Проверка была только на пути глубокого сбора, а обычный фильтр
        // портов не звал вовсе — и веб-трафик игры уезжал в игровой список.
        // Ровно так туда попала сеть Cloudflare с TCP/443 у Valorant.
        assert!(!стоит_собирать(Proto::Tcp, 443), "веб игры — не игровой адрес");
        assert!(!стоит_собирать(Proto::Tcp, 5223));
        assert!(!стоит_собирать(Proto::Tcp, 8443));
        assert!(стоит_собирать(Proto::Tcp, 27018), "а менеджер соединений Steam — да");
        assert!(стоит_собирать(Proto::Udp, 27015));
    }

    #[test]
    fn веб_в_игровой_список_не_попадает() {
        let _g = ТЕСТ_КОПИЛКИ.lock().unwrap_or_else(|e| e.into_inner());
        harvest_start();
        // Игровой порт — берём.
        harvest_line("UDP [1.1.1.1]:1 => [162.159.135.232]:27015 : x");
        // Веб — нет: у сайтов свои профили, и чужие настройки им не нужны.
        harvest_line("TCP [1.1.1.1]:1 => [104.16.0.1]:443 : x");
        harvest_line("TCP [1.1.1.1]:1 => [104.16.0.2]:80 : x");
        // И альтернативные HTTPS-порты Cloudflare — на живом тесте через
        // них в игровой список утёк Discord.
        for p in [2053, 2083, 2087, 2096, 8443] {
            harvest_line(&format!("TCP [1.1.1.1]:1 => [104.29.153.5]:{p} : x"));
        }
        // Служебное — тоже нет.
        harvest_line("TCP [1.1.1.1]:1 => [142.250.153.188]:5228 : x");
        let got = harvest_stop();
        assert_eq!(got, vec!["162.159.135.232"], "{got:?}");
    }

    #[test]
    fn копилка_берёт_только_внешние_и_только_когда_включена() {
        let _g = ТЕСТ_КОПИЛКИ.lock().unwrap_or_else(|e| e.into_inner());
        // Выключена — строки пролетают мимо.
        assert!(!harvest_active());
        harvest_line("UDP [1.1.1.1]:1 => [104.16.0.1]:27015 : x");
        assert_eq!(harvest_len(), 0);

        harvest_start();
        assert!(harvest_active());
        harvest_line("UDP [1.1.1.1]:1 => [104.16.0.1]:27015 : x");
        // Домашний роутер в обход попасть не должен.
        harvest_line("UDP [1.1.1.1]:1 => [192.168.1.1]:27015 : x");
        harvest_line("мусор");
        assert_eq!(harvest_len(), 1);

        let got = harvest_stop();
        assert_eq!(got, vec!["104.16.0.1"]);
        assert!(!harvest_active(), "после остановки сбор не идёт");
    }

    #[test]
    fn локальный_порт_из_строки_netstat() {
        assert_eq!(
            parse_local_socket("  UDP    0.0.0.0:50000          *:*                    1234"),
            Some((Proto::Udp, 50000, 1234)),
            "у несоединённого UDP локальный порт есть, и он-то нам и нужен"
        );
        assert_eq!(
            parse_local_socket("  TCP    192.168.1.5:51000      104.16.0.1:443         ESTABLISHED     4242"),
            Some((Proto::Tcp, 51000, 4242))
        );
        assert_eq!(
            parse_local_socket("  UDP    [::]:3074              *:*                    99"),
            Some((Proto::Udp, 3074, 99))
        );
        assert_eq!(parse_local_socket("  Proto  Local Address  Foreign Address  State  PID"), None);
        assert_eq!(parse_local_socket(""), None);
    }

    #[test]
    fn копилка_помнит_исходящий_порт() {
        let _g = ТЕСТ_КОПИЛКИ.lock().unwrap_or_else(|e| e.into_inner());
        harvest_start();
        harvest_line("IP4: 192.168.1.16 => 146.66.155.73 proto=udp ttl=128 sport=50282 dport=27015");
        harvest_line("IP4: 192.168.1.16 => 146.66.155.73 proto=udp ttl=128 sport=50282 dport=27016");
        let got = harvest_take();
        let hit = &got["146.66.155.73"];
        assert!(hit.udp && !hit.tcp);
        assert_eq!(hit.sports.iter().copied().collect::<Vec<_>>(), vec![(Proto::Udp, 50282)]);
        assert!(!harvest_active());
    }

    fn попадание(proto: Proto, sport: u16) -> Hit {
        let mut h = Hit::default();
        match proto {
            Proto::Udp => h.udp = true,
            Proto::Tcp => h.tcp = true,
        }
        h.sports.insert((proto, sport));
        h
    }

    #[test]
    fn игра_узнаётся_по_исходящему_порту_а_чужое_не_берётся() {
        let hits: BTreeMap<String, Hit> = [
            ("146.66.155.73".to_string(), попадание(Proto::Udp, 50282)),
            ("155.133.226.76".to_string(), попадание(Proto::Udp, 50282)),
            // Голос Discord на высоком порту — с Game Filter на всё он тоже
            // идёт через обход и раньше уезжал в игровой список.
            ("66.22.196.10".to_string(), попадание(Proto::Udp, 61000)),
            // Порт без владельца: чей пакет — неизвестно, брать нельзя.
            ("5.6.7.8".to_string(), попадание(Proto::Udp, 40000)),
        ]
        .into_iter()
        .collect();
        let owners: HashMap<(Proto, u16), u32> =
            [((Proto::Udp, 50282), 700), ((Proto::Udp, 61000), 800)].into_iter().collect();
        let names: HashMap<u32, String> =
            [(700, "cs2.exe".to_string()), (800, "Discord.exe".to_string())].into_iter().collect();
        let p = pick_game(&hits, &owners, &names);
        assert_eq!(p.process.as_deref(), Some("cs2.exe"));
        assert_eq!(p.addrs, vec!["146.66.155.73", "155.133.226.76"]);
        assert!(p.udp && !p.tcp);
        assert!(p.others.contains("Discord.exe"), "{:?}", p.others);
    }

    #[test]
    fn без_игры_ничего_не_кладём_и_объясняем_почему() {
        let hits: BTreeMap<String, Hit> =
            [("66.22.196.10".to_string(), попадание(Proto::Udp, 61000))].into_iter().collect();
        let owners: HashMap<(Proto, u16), u32> = [((Proto::Udp, 61000), 800)].into_iter().collect();
        let names: HashMap<u32, String> = [(800, "Discord.exe".to_string())].into_iter().collect();
        let p = pick_game(&hits, &owners, &names);
        assert_eq!(p.process, None);
        assert!(p.addrs.is_empty());
        assert!(!p.unattributed);
        assert!(p.others.contains("Discord.exe"));

        // Пакеты были, но чьи — неизвестно: в список ничего, и сказать надо иначе.
        let p = pick_game(&hits, &HashMap::new(), &names);
        assert_eq!(p.process, None);
        assert!(p.addrs.is_empty());
        assert!(p.unattributed);
    }

    #[test]
    fn game_filter_после_сбора() {
        assert_eq!(режим_фильтра("off", true, false), "udp");
        assert_eq!(режим_фильтра("off", false, true), "tcp");
        assert_eq!(режим_фильтра("tcp", true, false), "all", "включённое человеком не отбираем");
        assert_eq!(режим_фильтра("all", true, false), "all");
        assert_eq!(режим_фильтра("off", false, false), "off");
    }

    /// Живая проверка: `cargo test -- --ignored живой_скан --nocapture`.
    #[test]
    #[ignore]
    fn живой_скан_таблицы_соединений() {
        let all = connections();
        let ext = all.iter().filter(|c| is_external(&c.ip)).count();
        println!("соединений: {}, из них внешних: {ext}", all.len());
        let names = process_names();
        for c in candidates(&all, &names) {
            println!("кандидат: {} вес={} адресов={} порты={:?}", c.name, c.score, c.addrs, c.ports);
        }
        assert!(!all.is_empty(), "таблица соединений пуста — netstat не отработал");
    }
}
