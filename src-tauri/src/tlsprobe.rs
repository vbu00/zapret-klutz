//! Предтест фрагментацией: «поможет ли обход здесь вообще».
//!
//! Оракул из `probe.rs` отвечает, ГДЕ режут — по имени или по адресу. Этот
//! отвечает на следующий вопрос: если режут по имени, лечится ли это тем
//! способом, которым лечит zapret.
//!
//! Приём тот же, что применяет сам zapret, и он публично описан: отправить
//! ClientHello, разрезанный по TCP-сегментам ВНУТРИ имени хоста. Коробка,
//! разбирающая каждый сегмент отдельно, имени целиком не увидит и пропустит.
//! Коробка, пересобирающая поток, увидит — и тогда весь класс стратегий с
//! разрезом бесполезен, сколько их ни перебирай.
//!
//! Зачем это Klutz. Сейчас единственный способ узнать, поможет ли
//! что-нибудь, — прогнать через PowerShell-скрипт все двадцать с лишним
//! конфигов, а это минуты. Здесь ответ за пару секунд и ДО прогона.
//!
//! ClientHello собираем свой, потому что curl такого не умеет: нужно
//! положить разрез в заранее известное место внутри имени. Рукопожатие мы
//! не доводим до конца — достаточно узнать, вернулось ли хоть что-то.

use serde::Serialize;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

/// Чем закончилась отправка одного ClientHello.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChOutcome {
    /// Пришли байты в ответ. Что именно — ServerHello, alert, что угодно —
    /// неважно: путь до сервера живой.
    Answered,
    /// Соединение сброшено. Классическая подпись коробки, увидевшей имя.
    Reset,
    /// Тишина до таймаута или закрытие без единого байта.
    Silent,
    /// Не удалось даже подключиться — мерить нечего.
    NoConnect,
}

impl ChOutcome {
    fn passed(self) -> bool {
        self == ChOutcome::Answered
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FragVerdict {
    /// Целый ClientHello и так проходит — резать нечего.
    NotBlocked,
    /// Целый режут, разрезанный проходит. Стратегии со split применимы.
    Helps,
    /// Режут и разрезанный тоже: коробка пересобирает сегменты. Стратегии
    /// на одном разрезе не помогут, но это не приговор zapret: рабочие
    /// конфиги (ALT) обходят такое поддельными пакетами — на живой сети
    /// проба говорила «не проходит», а Discord на ALT12 открывался.
    DoesNotHelp,
    /// Подключиться не вышло или результат не воспроизвёлся.
    Inconclusive,
}

#[derive(Debug, Clone, Serialize)]
pub struct FragProbe {
    pub whole: ChOutcome,
    /// `None` — разрезанный не отправляли: целый и так прошёл, и второе
    /// соединение ответило бы на уже решённый вопрос. Раньше сюда клался
    /// `Answered`, и поле выдавало за замер то, чего не мерили.
    pub split: Option<ChOutcome>,
    pub verdict: FragVerdict,
    /// Фраза для человека — её же кладём в лог прогона тестов.
    pub note: String,
}

// ───────────────────────── сборка ClientHello ─────────────────────────

fn u16b(v: usize) -> [u8; 2] {
    [(v >> 8) as u8, v as u8]
}

/// Дописывает блок, длина которого стоит впереди двумя байтами.
fn push_len16(out: &mut Vec<u8>, body: &[u8]) {
    out.extend_from_slice(&u16b(body.len()));
    out.extend_from_slice(body);
}

/// Расширение TLS: тип, длина, тело.
fn ext(out: &mut Vec<u8>, kind: u16, body: &[u8]) {
    out.extend_from_slice(&u16b(kind as usize));
    push_len16(out, body);
}

/// Собранный ClientHello и позиция имени хоста внутри него.
pub struct ClientHello {
    pub bytes: Vec<u8>,
    /// Смещение первого байта имени хоста в `bytes`.
    pub sni_offset: usize,
    pub sni_len: usize,
}

impl ClientHello {
    /// Куда резать, чтобы разрыв пришёлся ВНУТРИ имени. Имя короче двух
    /// байт разрезать смысла нет — тогда режем сразу после его начала.
    pub fn split_at(&self) -> usize {
        self.sni_offset + (self.sni_len / 2).max(1)
    }
}

/// Собирает ClientHello с заданным именем.
///
/// Набор расширений подобран так, чтобы ответил и сервер TLS 1.2, и 1.3:
/// есть supported_versions и key_share, поэтому 1.3-сервер отвечает сразу
/// ServerHello, без HelloRetryRequest. Ключ в key_share — случайные 32
/// байта: любая такая строка годится как публичный ключ x25519, а
/// рукопожатие нам всё равно не доводить.
pub fn build_client_hello(sni: &str) -> ClientHello {
    build_client_hello_opts(sni, false)
}

/// То же, но с возможностью запретить TLS 1.3.
///
/// `tls12_only` убирает supported_versions, key_share и psk_key_exchange_modes
/// — все три расширения существуют только ради 1.3. Без них сервер
/// договаривается максимум на 1.2, а это и нужно: в 1.2 сертификат едет
/// ОТКРЫТЫМ ТЕКСТОМ и содержит имя домена, то есть именно там коробка может
/// резать ОТВЕТ. В 1.3 такого класса блокировки не существует — после
/// ServerHello всё зашифровано, и резать по имени в сертификате нечего.
pub fn build_client_hello_opts(sni: &str, tls12_only: bool) -> ClientHello {
    let mut rnd = [0u8; 64];
    if !crate::sys::os_random(&mut rnd) {
        // ГСЧ недоступен — на результат пробы это не влияет, важна лишь
        // непохожесть байтов на константу.
        for (i, b) in rnd.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
    }

    let mut body: Vec<u8> = Vec::with_capacity(512);
    body.extend_from_slice(&[0x03, 0x03]); // client_version = TLS 1.2
    body.extend_from_slice(&rnd[..32]); // random
    body.push(32); // session_id
    body.extend_from_slice(&rnd[32..64]);

    // Наборы шифров: три от TLS 1.3 плюс обычная современная выборка.
    let suites: [u16; 12] = [
        0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0x009c, 0x009d,
        0x002f,
    ];
    let mut s = Vec::with_capacity(suites.len() * 2);
    for c in suites {
        s.extend_from_slice(&u16b(c as usize));
    }
    push_len16(&mut body, &s);
    body.extend_from_slice(&[0x01, 0x00]); // compression: null

    // ── расширения ──
    let mut exts: Vec<u8> = Vec::with_capacity(256);

    // server_name. Запоминаем, где внутри ЭТОГО буфера легло имя, чтобы
    // потом пересчитать смещение в готовой записи.
    let name = sni.as_bytes();
    let mut sni_body = Vec::with_capacity(name.len() + 5);
    sni_body.extend_from_slice(&u16b(name.len() + 3)); // длина списка имён
    sni_body.push(0x00); // тип: host_name
    sni_body.extend_from_slice(&u16b(name.len()));
    let sni_at_in_sni_body = sni_body.len();
    sni_body.extend_from_slice(name);
    // +4 — заголовок расширения (тип и длина), который допишет ext().
    let sni_at_in_exts = exts.len() + 4 + sni_at_in_sni_body;
    ext(&mut exts, 0x0000, &sni_body);

    ext(&mut exts, 0x0017, &[]); // extended_master_secret
    ext(&mut exts, 0x0023, &[]); // session_ticket
    ext(&mut exts, 0x000b, &[0x01, 0x00]); // ec_point_formats: uncompressed
    ext(&mut exts, 0x000a, &[0x00, 0x06, 0x00, 0x1d, 0x00, 0x17, 0x00, 0x18]); // groups
    ext(
        &mut exts,
        0x000d, // signature_algorithms
        &[
            0x00, 0x10, 0x04, 0x03, 0x08, 0x04, 0x04, 0x01, 0x05, 0x03, 0x08, 0x05, 0x05, 0x01,
            0x08, 0x06, 0x06, 0x01,
        ],
    );
    if !tls12_only {
        ext(&mut exts, 0x002b, &[0x04, 0x03, 0x04, 0x03, 0x03]); // supported_versions: 1.3, 1.2
        ext(&mut exts, 0x002d, &[0x01, 0x01]); // psk_key_exchange_modes
    }
    // ALPN: h2, http/1.1
    ext(
        &mut exts,
        0x0010,
        &[
            0x00, 0x0c, 0x02, b'h', b'2', 0x08, b'h', b't', b't', b'p', b'/', b'1', b'.', b'1',
        ],
    );
    if !tls12_only {
        // key_share: x25519 со случайным ключом
        let mut ks = Vec::with_capacity(40);
        ks.extend_from_slice(&u16b(36)); // длина списка
        ks.extend_from_slice(&u16b(0x001d)); // x25519
        ks.extend_from_slice(&u16b(32));
        ks.extend_from_slice(&rnd[..32]);
        ext(&mut exts, 0x0033, &ks);
    }

    push_len16(&mut body, &exts);
    // Заголовок расширений — два байта длины, они уже перед exts.
    let sni_at_in_body = body.len() - exts.len() + sni_at_in_exts;

    // ── handshake ──
    let mut hs = Vec::with_capacity(body.len() + 4);
    hs.push(0x01); // client_hello
    hs.extend_from_slice(&[(body.len() >> 16) as u8, (body.len() >> 8) as u8, body.len() as u8]);
    hs.extend_from_slice(&body);
    let sni_at_in_hs = 4 + sni_at_in_body;

    // ── запись ──
    let mut rec = Vec::with_capacity(hs.len() + 5);
    rec.push(0x16); // handshake
    rec.extend_from_slice(&[0x03, 0x01]); // legacy record version
    rec.extend_from_slice(&u16b(hs.len()));
    rec.extend_from_slice(&hs);

    ClientHello { bytes: rec, sni_offset: 5 + sni_at_in_hs, sni_len: name.len() }
}

// ───────────────────────────── отправка ─────────────────────────────

/// Отправляет ClientHello и ждёт хоть какого-нибудь ответа.
///
/// При `fragment` запись уходит двумя порциями с разрывом внутри имени.
/// TCP_NODELAY обязателен: без него Nagle склеил бы обе записи в один
/// сегмент, и проверка потеряла бы смысл.
pub fn send_ch(ip: IpAddr, port: u16, sni: &str, fragment: bool, timeout: Duration) -> ChOutcome {
    let addr = SocketAddr::new(ip, port);
    let Ok(mut sock) = TcpStream::connect_timeout(&addr, timeout) else {
        return ChOutcome::NoConnect;
    };
    let _ = sock.set_nodelay(true);
    let _ = sock.set_write_timeout(Some(timeout));
    let _ = sock.set_read_timeout(Some(timeout));

    let ch = build_client_hello(sni);
    let write_res = if fragment {
        let at = ch.split_at().min(ch.bytes.len());
        match sock.write_all(&ch.bytes[..at]).and_then(|_| sock.flush()) {
            Ok(()) => {
                // Пауза, чтобы вторая часть точно уехала отдельным сегментом.
                std::thread::sleep(Duration::from_millis(40));
                sock.write_all(&ch.bytes[at..]).and_then(|_| sock.flush())
            }
            Err(e) => Err(e),
        }
    } else {
        sock.write_all(&ch.bytes).and_then(|_| sock.flush())
    };
    if let Err(e) = write_res {
        return outcome_from_err(&e);
    }

    let mut buf = [0u8; 64];
    match sock.read(&mut buf) {
        Ok(0) => ChOutcome::Silent,
        Ok(_) => ChOutcome::Answered,
        Err(e) => outcome_from_err(&e),
    }
}

fn outcome_from_err(e: &std::io::Error) -> ChOutcome {
    match e.kind() {
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted => {
            ChOutcome::Reset
        }
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ChOutcome::Silent,
        _ => ChOutcome::Silent,
    }
}

/// Чистое правило вердикта — сеть дёргает вызывающий.
pub fn classify_frag(whole: ChOutcome, split: Option<ChOutcome>) -> (FragVerdict, String) {
    let unreachable = (
        FragVerdict::Inconclusive,
        "до адреса не достучались — про фрагментацию сказать нечего".to_string(),
    );
    if whole == ChOutcome::NoConnect {
        return unreachable;
    }
    if whole.passed() {
        return (
            FragVerdict::NotBlocked,
            "целый ClientHello доходит — резать нечего, дело не в имени".into(),
        );
    }
    // Целый не прошёл — значит разрезанный обязаны были отправить. Если его
    // нет, вывода нет: «не мерили» это не «не помогает».
    let Some(split) = split else {
        return (
            FragVerdict::Inconclusive,
            "целый ClientHello не прошёл, а разрезанный не отправляли — сравнивать не с чем"
                .into(),
        );
    };
    if split == ChOutcome::NoConnect {
        return unreachable;
    }
    if split.passed() {
        (
            FragVerdict::Helps,
            "целый ClientHello режут, а разрезанный по сегментам проходит — \
             стратегии с разрезом здесь работают, подбор имеет смысл"
                .into(),
        )
    } else {
        (
            FragVerdict::DoesNotHelp,
            "режут и целый, и разрезанный: коробка собирает сегменты обратно — \
             простой разрез не поможет, нужны стратегии с поддельными пакетами (fake), как в ALT"
                .into(),
        )
    }
}

/// Полная проба: целый ClientHello, затем разрезанный.
pub fn probe_fragmentation(ip: IpAddr, port: u16, sni: &str, timeout: Duration) -> FragProbe {
    let whole = send_ch(ip, port, sni, false, timeout);
    // Разрезанный шлём, только если целый не прошёл: иначе платили бы вторым
    // соединением за вопрос, ответ на который уже известен.
    let split = if whole.passed() { None } else { Some(send_ch(ip, port, sni, true, timeout)) };
    let (verdict, note) = classify_frag(whole, split);
    FragProbe { whole, split, verdict, note }
}


/// Сводит вердикты по нескольким целям в один.
///
/// Достаточно ОДНОЙ цели, где фрагментация помогает, чтобы подбор имел
/// смысл: стратегия применяется ко всем сразу, и вытащить хотя бы часть
/// уже выигрыш. А вот «бесполезно» говорим только когда ни одна цель
/// надежды не подала.
pub fn aggregate(verdicts: &[FragVerdict]) -> (FragVerdict, String) {
    use FragVerdict::*;
    // Пустой список — это «проверять было нечего», а не «до целей не
    // достучались»: последнее утверждает сетевой отказ, которого не было.
    if verdicts.is_empty() {
        return (Inconclusive, "проверять было нечего — ни одной цели".into());
    }
    if verdicts.contains(&Helps) {
        return (
            Helps,
            "разрез ClientHello пробивает — стратегии zapret здесь применимы, подбор имеет смысл"
                .into(),
        );
    }
    // «Не пробивает ни одну» можно говорить, только если по каждой цели
    // измерение было. Одна неизмеренная — и утверждение уже шире данных.
    if verdicts.contains(&DoesNotHelp) && verdicts.contains(&Inconclusive) {
        return (
            Inconclusive,
            "по части целей разрез не пробил, по остальным измерить не вышло — \
             общего вывода нет"
                .into(),
        );
    }
    if verdicts.contains(&DoesNotHelp) {
        return (
            DoesNotHelp,
            "простой разрез ClientHello не пробивает ни одну цель: коробка собирает \
             сегменты обратно. Работать будут стратегии с поддельными пакетами \
             (fake), как в ALT, — одним разрезом тут не обойтись"
                .into(),
        );
    }
    if verdicts.iter().all(|v| *v == NotBlocked) {
        return (NotBlocked, "цели и так открываются — подбирать нечего".into());
    }
    // Осталась смесь «открывается» и «не достучались». Сказать «до целей не
    // достучались» здесь было бы неправдой: часть целей ответила.
    if verdicts.contains(&NotBlocked) {
        return (
            Inconclusive,
            "часть целей открывается, до остальных не достучались — общего вывода нет".into(),
        );
    }
    (Inconclusive, "проверить не удалось — до целей не достучались".into())
}


// ──────────────── ответное направление и версия TLS ────────────────

/// Какие сообщения рукопожатия успели прийти от сервера.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct HsSeen {
    pub server_hello: bool,
    /// Сервер договорился именно на TLS 1.3. Отличать важно: 1.2-only сервер
    /// отвечает на наш 1.3-способный ClientHello обычным ServerHello, и по
    /// одному факту ответа получилось бы «1.3 проходит» на сервере, который
    /// его вовсе не умеет.
    pub tls13: bool,
    /// Сертификат пришёл ЦЕЛИКОМ. Именно на нём режут ответ в TLS 1.2.
    pub certificate: bool,
    /// ServerHelloDone — сервер досказал свою часть.
    pub done: bool,
    pub alert: bool,
}

/// Разбирает поток TLS-записей и отмечает, что в нём встретилось.
///
/// Чистая функция: сокет читает вызывающий. Сообщения рукопожатия могут быть
/// разрезаны по записям, поэтому сначала склеиваем полезную нагрузку всех
/// записей типа handshake, а уже потом идём по сообщениям. Флаг ставим
/// только на СОБРАННОЕ целиком сообщение — иначе «сертификат обрезали на
/// середине» выглядело бы как «сертификат пришёл», а это ровно тот случай,
/// который мы и ловим.
pub fn scan_records(buf: &[u8]) -> HsSeen {
    let mut seen = HsSeen::default();
    let mut hs: Vec<u8> = Vec::new();

    let mut i = 0usize;
    while i + 5 <= buf.len() {
        let kind = buf[i];
        let len = u16::from_be_bytes([buf[i + 3], buf[i + 4]]) as usize;
        let start = i + 5;
        let Some(end) = start.checked_add(len).filter(|e| *e <= buf.len()) else {
            break; // запись не доехала целиком
        };
        match kind {
            // После alert поток рукопожатия оборван. Продолжать склейку
            // нельзя: коробка, обрубившая сертификат и приславшая alert,
            // могла дослать остаток — и обрезанное сообщение выглядело бы
            // пришедшим целиком, то есть ровно наоборот тому, что мы ловим.
            0x16 if !seen.alert => hs.extend_from_slice(&buf[start..end]),
            0x15 => seen.alert = true,
            // 0x17 — application_data: в TLS 1.3 всё после ServerHello уже
            // зашифровано, разбирать там нечего.
            _ => {}
        }
        i = end;
    }

    let mut j = 0usize;
    while j + 4 <= hs.len() {
        let t = hs[j];
        let l = ((hs[j + 1] as usize) << 16) | ((hs[j + 2] as usize) << 8) | hs[j + 3] as usize;
        let Some(next) = (j + 4).checked_add(l).filter(|n| *n <= hs.len()) else {
            break; // сообщение оборвано — целым его не считаем
        };
        match t {
            0x02 => {
                seen.server_hello = true;
                seen.tls13 = server_hello_is_tls13(&hs[j + 4..next]);
            }
            0x0b => seen.certificate = true,
            0x0e => seen.done = true,
            _ => {}
        }
        if next == j {
            break;
        }
        j = next;
    }
    seen
}

/// Договорился ли сервер на TLS 1.3, судя по телу его ServerHello.
///
/// В 1.3 поле версии осталось равным 0x0303 ради совместимости, а настоящая
/// версия лежит в расширении supported_versions (0x002b) значением 0x0304.
fn server_hello_is_tls13(body: &[u8]) -> bool {
    // legacy_version(2) random(32) session_id_len(1)
    let mut i = 34usize;
    let Some(&sid) = body.get(i) else { return false };
    i += 1 + sid as usize;
    i += 2 + 1; // cipher_suite(2) compression(1)
    if i + 2 > body.len() {
        return false;
    }
    let ext_len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
    i += 2;
    let end = (i + ext_len).min(body.len());
    while i + 4 <= end {
        let kind = u16::from_be_bytes([body[i], body[i + 1]]);
        let len = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        let val = i + 4;
        let Some(stop) = val.checked_add(len).filter(|s| *s <= end) else {
            return false;
        };
        if kind == 0x002b && len == 2 {
            return body[val] == 0x03 && body[val + 1] == 0x04;
        }
        i = stop;
    }
    false
}

/// Отправляет ClientHello и читает ответ, пока сервер не досказал своё или
/// не кончилось время.
pub fn handshake_probe(
    ip: IpAddr,
    port: u16,
    sni: &str,
    tls12_only: bool,
    timeout: Duration,
) -> (HsSeen, ChOutcome) {
    let addr = SocketAddr::new(ip, port);
    let Ok(mut sock) = TcpStream::connect_timeout(&addr, timeout) else {
        return (HsSeen::default(), ChOutcome::NoConnect);
    };
    let _ = sock.set_nodelay(true);
    let _ = sock.set_write_timeout(Some(timeout));
    let _ = sock.set_read_timeout(Some(timeout));

    let ch = build_client_hello_opts(sni, tls12_only);
    if let Err(e) = sock.write_all(&ch.bytes).and_then(|_| sock.flush()) {
        return (HsSeen::default(), outcome_from_err(&e));
    }

    let mut acc: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let mut outcome = ChOutcome::Silent;
    // Потолок на случай болтливого сервера: цепочка сертификатов длиннее
    // 32 КБ встречается разве что нарочно.
    while acc.len() < 32 * 1024 {
        match sock.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&chunk[..n]);
                outcome = ChOutcome::Answered;
                let seen = scan_records(&acc);
                if seen.done || seen.alert {
                    break;
                }
            }
            Err(e) => {
                if acc.is_empty() {
                    outcome = outcome_from_err(&e);
                }
                break;
            }
        }
    }
    (scan_records(&acc), outcome)
}

// ── 04: режут не запрос, а ответ ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RespVerdict {
    /// Проверить не удалось. Это НЕ «всё хорошо», это «не измерено».
    NotApplicable,
    /// Рукопожатие 1.2 доходит до конца — сертификат не режут.
    Clear,
    /// Запрос проходит, а ответ убивают.
    Blocked,
    /// Не воспроизводится.
    Flaky,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseResult {
    pub verdict: RespVerdict,
    pub reason: String,
    /// Сколько рукопожатий из `repeats` дошло до конца.
    pub target: u32,
    pub control: u32,
    pub repeats: u32,
}

/// Правило вердикта ответного направления — чистое, сеть снаружи.
///
/// Оракул тот же, что и везде: сравнение с контролем. Рукопожатие с
/// нейтральным именем на ТОТ ЖЕ адрес обязано завершаться. Не завершается —
/// значит сервер не умеет 1.2 или мешает что-то ещё, и вывода мы не делаем.
pub fn classify_response(target: u32, control: u32, repeats: u32) -> (RespVerdict, String) {
    // Ноль повторов — ноль замеров. Без этой проверки (0, 0, 0) давало
    // «чисто»: правило объявляло бы чистым то, чего не измеряло.
    if repeats == 0 {
        return (
            RespVerdict::NotApplicable,
            "рукопожатия не запускались — про ответное направление вывода нет".into(),
        );
    }
    // Контроль обязан завершиться ВСЕ разы, а не хоть раз: на одном успехе
    // из двух говорить «с нейтральным именем отвечает каждый раз» нельзя, а
    // вердикт «режут ответ» строится именно на этом утверждении.
    if control < repeats {
        return (
            RespVerdict::NotApplicable,
            "контрольное рукопожатие по TLS 1.2 завершается не каждый раз — сервер его не \
             поддерживает или мешает что-то ещё; про ответное направление вывода нет"
                .into(),
        );
    }
    if target == repeats {
        return (
            RespVerdict::Clear,
            "рукопожатие TLS 1.2 доходит до конца — сертификат не режут".into(),
        );
    }
    if target == 0 {
        return (
            RespVerdict::Blocked,
            format!(
                "запрос проходит, а рукопожатие не завершается ни разу, тогда как с именем {} \
                 на тот же адрес оно завершается каждый раз. Режут ОТВЕТ — в TLS 1.2 сертификат \
                 идёт открытым текстом и содержит имя домена",
                crate::probe::NEUTRAL_SNI
            ),
        );
    }
    (
        RespVerdict::Flaky,
        format!("рукопожатий дошло {target} из {repeats} — не воспроизводится"),
    )
}

fn count_completed(ip: IpAddr, port: u16, sni: &str, repeats: u32, timeout: Duration) -> u32 {
    (0..repeats)
        // Именно сертификат И завершение, а не одно завершение. Проба
        // существует ради вопроса «не режут ли сертификат», и отвечать на
        // него по одному ServerHelloDone — значит не смотреть на то, что
        // как раз и меряем.
        .filter(|_| {
            let seen = handshake_probe(ip, port, sni, true, timeout).0;
            seen.certificate && seen.done
        })
        .count() as u32
}

/// Не режут ли ОТВЕТ сервера.
///
/// Зовётся только там, где запрос уже признан проходящим: если режут запрос,
/// про ответ говорить рано.
pub fn probe_response_direction(
    ip: IpAddr,
    port: u16,
    sni: &str,
    repeats: u32,
    timeout: Duration,
) -> ResponseResult {
    let repeats = repeats.max(1);
    let target = count_completed(ip, port, sni, repeats, timeout);
    // Контроль ОБЯЗАН быть другим именем на том же адресе.
    let control = count_completed(ip, port, crate::probe::NEUTRAL_SNI, repeats, timeout);
    let (verdict, reason) = classify_response(target, control, repeats);
    ResponseResult { verdict, reason, target, control, repeats }
}

// ── 05: блок, нацеленный на TLS 1.3 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tls13Verdict {
    /// 1.3 проходит — ничего особенного.
    Ok,
    /// 1.3 не проходит, а 1.2 проходит: коробка смотрит именно в
    /// ClientHello 1.3 (ECH/ESNI).
    Blocked,
    /// Не прошли обе версии — дело не в версии.
    NotApplicable,
}

/// `answered13` — ответил ли сервер хоть чем-нибудь на ClientHello, умеющий
/// 1.3. `negotiated13` — согласовал ли он при этом именно 1.3. `answered12` —
/// прошло ли рукопожатие с ClientHello, знающим только 1.2.
///
/// Решает ОТВЕТ, а не версия. Раньше здесь стояло «согласовал 1.3», и сервер,
/// умеющий только 1.2, отвечал согласованием 1.2 — то есть отвечал, — а его
/// объявляли заблокированным по 1.3. Заблокированный ClientHello не отвечает
/// вовсе; ответивший доехал, какую бы версию в нём ни выбрали.
pub fn classify_tls13(answered13: bool, negotiated13: bool, answered12: bool) -> (Tls13Verdict, String) {
    match (answered13, negotiated13, answered12) {
        (true, true, _) => (Tls13Verdict::Ok, "TLS 1.3 проходит".into()),
        (true, false, _) => (
            Tls13Verdict::Ok,
            "ClientHello с поддержкой 1.3 доехал, но сервер выбрал версию ниже — \
             это его свойство, а не блокировка"
                .into(),
        ),
        (false, _, true) => (
            Tls13Verdict::Blocked,
            "TLS 1.3 не проходит, а откат на 1.2 проходит — режут именно ClientHello 1.3. \
             Браузер по умолчанию говорит на 1.3, поэтому страница у человека не открывается, \
             хотя проверка, согласившаяся на 1.2, показала бы «работает»"
                .into(),
        ),
        (false, _, false) => (
            Tls13Verdict::NotApplicable,
            "не прошли ни 1.3, ни 1.2 — дело не в версии".into(),
        ),
    }
}

/// Сравнивает 1.3 и 1.2 к одному адресу.
pub fn probe_tls13_block(
    ip: IpAddr,
    port: u16,
    sni: &str,
    timeout: Duration,
) -> (Tls13Verdict, String) {
    let (seen13, out13) = handshake_probe(ip, port, sni, false, timeout);
    let answered13 = out13 == ChOutcome::Answered;
    // Лишний заход платим, только когда на 1.3-совместимый ClientHello не
    // ответили вовсе: только тогда вопрос «а на 1.2 ответят?» вообще стоит.
    let answered12 = if answered13 {
        true
    } else {
        // Именно завершённое рукопожатие, а не один ServerHello. Иначе
        // сервер, который ответил на 1.2 и тут же оборвался, объявлял бы
        // 1.3 заблокированным — хотя оборвались обе версии.
        {
            let seen = handshake_probe(ip, port, sni, true, timeout).0;
            seen.server_hello && seen.done
        }
    };
    classify_tls13(answered13, seen13.tls13, answered12)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn проверить_рамки(ch: &ClientHello) {
        let b = &ch.bytes;
        assert_eq!(b[0], 0x16, "тип записи handshake");
        assert_eq!(&b[1..3], &[0x03, 0x01], "legacy-версия записи");
        let rec_len = u16::from_be_bytes([b[3], b[4]]) as usize;
        assert_eq!(rec_len, b.len() - 5, "длина записи должна совпадать с телом");

        assert_eq!(b[5], 0x01, "client_hello");
        let hs_len = ((b[6] as usize) << 16) | ((b[7] as usize) << 8) | b[8] as usize;
        assert_eq!(hs_len, b.len() - 9, "длина handshake должна совпадать");
    }

    #[test]
    fn длины_сходятся_на_любом_имени() {
        for sni in ["a", "discord.com", "redirector.googlevideo.com", "очень-длинное-имя.example"] {
            let ch = build_client_hello(sni);
            проверить_рамки(&ch);
        }
    }

    #[test]
    fn имя_лежит_там_где_обещано() {
        for sni in ["discord.com", "www.youtube.com", "a.b"] {
            let ch = build_client_hello(sni);
            let got = &ch.bytes[ch.sni_offset..ch.sni_offset + ch.sni_len];
            assert_eq!(got, sni.as_bytes(), "смещение имени неверно для {sni}");
        }
    }

    #[test]
    fn имя_встречается_в_записи_ровно_один_раз() {
        // Иначе разрез мог бы прийтись не на то вхождение.
        let sni = "discord.com";
        let ch = build_client_hello(sni);
        let n = ch.bytes.windows(sni.len()).filter(|w| *w == sni.as_bytes()).count();
        assert_eq!(n, 1, "имя должно быть в ClientHello один раз");
    }

    #[test]
    fn разрез_приходится_внутрь_имени() {
        for sni in ["discord.com", "www.youtube.com", "ab", "a"] {
            let ch = build_client_hello(sni);
            let at = ch.split_at();
            assert!(at > ch.sni_offset, "{sni}: разрез не должен быть до имени");
            assert!(
                at < ch.sni_offset + ch.sni_len || ch.sni_len <= 1,
                "{sni}: разрез не должен быть после имени"
            );
            assert!(at < ch.bytes.len(), "{sni}: разрез внутри буфера");
        }
    }

    #[test]
    fn вердикт_целый_прошёл() {
        // Разрезанный в этом случае не отправляют вовсе.
        let (v, _) = classify_frag(ChOutcome::Answered, None);
        assert_eq!(v, FragVerdict::NotBlocked);
    }

    #[test]
    fn вердикт_фрагментация_помогает() {
        for whole in [ChOutcome::Reset, ChOutcome::Silent] {
            let (v, why) = classify_frag(whole, Some(ChOutcome::Answered));
            assert_eq!(v, FragVerdict::Helps, "{whole:?}");
            assert!(why.contains("подбор имеет смысл"));
        }
    }

    #[test]
    fn вердикт_фрагментация_не_помогает() {
        for split in [ChOutcome::Reset, ChOutcome::Silent] {
            let (v, why) = classify_frag(ChOutcome::Reset, Some(split));
            assert_eq!(v, FragVerdict::DoesNotHelp, "{split:?}");
            // Не «бесполезно»: простой разрез не прошёл, а fake-стратегии
            // на той же сети работали.
            assert!(why.contains("fake"), "{why}");
        }
    }

    #[test]
    fn без_подключения_вывода_нет() {
        for (w, s) in [
            (ChOutcome::NoConnect, Some(ChOutcome::NoConnect)),
            (ChOutcome::NoConnect, None),
            (ChOutcome::Reset, Some(ChOutcome::NoConnect)),
        ] {
            let (v, _) = classify_frag(w, s);
            assert_eq!(v, FragVerdict::Inconclusive);
        }
    }

    #[test]
    fn одна_неизмеренная_цель_снимает_общий_приговор() {
        use FragVerdict::*;
        // «Не пробивает НИ ОДНУ» — утверждение про все цели. Если по одной
        // измерения не было, оно шире данных.
        let (v, why) = aggregate(&[DoesNotHelp, Inconclusive]);
        assert_eq!(v, Inconclusive, "{why}");
        assert!(why.contains("общего вывода нет"), "{why}");
        // А когда измерены все — приговор остаётся.
        assert_eq!(aggregate(&[DoesNotHelp, NotBlocked]).0, DoesNotHelp);
        assert_eq!(aggregate(&[DoesNotHelp, DoesNotHelp]).0, DoesNotHelp);
    }

    #[test]
    fn неотправленный_разрез_это_не_не_помогает() {
        // Целый не прошёл, разрезанный не слали — сравнивать не с чем.
        // Раньше вызывающий подставлял сюда выдуманный `Answered`, и
        // неизмеренное уходило в интерфейс как замер.
        let (v, why) = classify_frag(ChOutcome::Reset, None);
        assert_eq!(v, FragVerdict::Inconclusive);
        assert!(why.contains("сравнивать не с чем"), "{why}");
    }

    #[test]
    fn сводный_вердикт_хватает_одной_надежды() {
        use FragVerdict::*;
        let (v, _) = aggregate(&[DoesNotHelp, NotBlocked, Helps]);
        assert_eq!(v, Helps, "одной пробившей цели достаточно");
    }

    #[test]
    fn сводный_вердикт_бесполезно_только_когда_надежды_нет() {
        use FragVerdict::*;
        assert_eq!(aggregate(&[DoesNotHelp, NotBlocked]).0, DoesNotHelp);
        assert_eq!(aggregate(&[NotBlocked, NotBlocked]).0, NotBlocked);
        assert_eq!(aggregate(&[Inconclusive, NotBlocked]).0, Inconclusive);
        assert_eq!(aggregate(&[]).0, Inconclusive);
    }

    #[test]
    fn два_вызова_дают_разные_random() {
        // Иначе ClientHello был бы константой и сам стал бы отпечатком.
        let a = build_client_hello("discord.com");
        let b = build_client_hello("discord.com");
        assert_ne!(a.bytes, b.bytes, "random должен отличаться");
        assert_eq!(a.sni_offset, b.sni_offset, "а смещение имени — нет");
    }

    /// Запись TLS: тип, версия, длина, тело.
    fn запись(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![kind, 0x03, 0x03];
        v.extend_from_slice(&u16b(body.len()));
        v.extend_from_slice(body);
        v
    }

    /// Сообщение рукопожатия: тип, длина в трёх байтах, тело.
    fn сообщение(t: u8, len: usize) -> Vec<u8> {
        let mut v = vec![t, (len >> 16) as u8, (len >> 8) as u8, len as u8];
        v.extend(std::iter::repeat_n(0xAB, len));
        v
    }

    #[test]
    fn полное_рукопожатие_разбирается() {
        let mut hs = сообщение(0x02, 70); // ServerHello
        hs.extend(сообщение(0x0b, 900)); // Certificate
        hs.extend(сообщение(0x0e, 0)); // ServerHelloDone
        let seen = scan_records(&запись(0x16, &hs));
        assert!(seen.server_hello && seen.certificate && seen.done, "{seen:?}");
        assert!(!seen.alert);
    }

    #[test]
    fn обрезанный_сертификат_не_считается_пришедшим() {
        // Ровно тот случай, ради которого всё и затевалось: ServerHello
        // вернулся, а сертификат обрубили на середине.
        let mut hs = сообщение(0x02, 70);
        let cert = сообщение(0x0b, 900);
        hs.extend_from_slice(&cert[..400]); // обрыв внутри сертификата
        let seen = scan_records(&запись(0x16, &hs));
        assert!(seen.server_hello, "ServerHello пришёл целиком");
        assert!(!seen.certificate, "сертификат неполон — считать пришедшим нельзя");
        assert!(!seen.done);
    }

    #[test]
    fn сообщение_разрезанное_по_записям_собирается() {
        // Сообщения рукопожатия не обязаны укладываться в одну запись.
        let hs = сообщение(0x0b, 600);
        let mut поток = запись(0x16, &hs[..200]);
        поток.extend(запись(0x16, &hs[200..]));
        let seen = scan_records(&поток);
        assert!(seen.certificate, "склейка записей не сработала");
    }

    #[test]
    fn alert_замечается() {
        let seen = scan_records(&запись(0x15, &[0x02, 0x28]));
        assert!(seen.alert);
        assert!(!seen.server_hello);
    }

    #[test]
    fn мусор_и_обрывки_не_роняют_разбор() {
        for buf in [
            vec![],
            vec![0x16],
            vec![0x16, 0x03, 0x03, 0xff, 0xff], // длина больше, чем данных
            vec![0x16, 0x03, 0x03, 0x00, 0x04, 0x0b, 0xff, 0xff, 0xff], // длина сообщения врёт
            vec![0xAB; 64],
        ] {
            let _ = scan_records(&buf);
        }
    }

    #[test]
    fn вердикт_ответного_направления() {
        use RespVerdict::*;
        assert_eq!(classify_response(3, 3, 3).0, Clear);
        assert_eq!(classify_response(0, 3, 3).0, Blocked);
        assert_eq!(classify_response(1, 3, 3).0, Flaky);
        // Контроль молчит — вывода нет, и это НЕ «всё хорошо».
        assert_eq!(classify_response(0, 0, 3).0, NotApplicable);
        assert_eq!(classify_response(3, 0, 3).0, NotApplicable);
    }

    #[test]
    fn нестабильный_контроль_не_даёт_вердикта() {
        // Контроль прошёл раз из двух: утверждать «с нейтральным именем
        // отвечает каждый раз» нельзя, а вердикт «режут ответ» держится
        // именно на этом.
        assert_eq!(classify_response(0, 1, 2).0, RespVerdict::NotApplicable);
        assert_eq!(classify_response(0, 2, 2).0, RespVerdict::Blocked);
    }

    #[test]
    fn после_alert_поток_рукопожатия_не_склеивается() {
        // Коробка обрубает сертификат, шлёт alert и досылает хвост. Без
        // проверки хвост дописался бы к обрубку, и сообщение выглядело бы
        // пришедшим целиком — ровно наоборот тому, что мы ловим.
        let cert = сообщение(0x0b, 600);
        let mut поток = запись(0x16, &сообщение(0x02, 70));
        поток.extend(запись(0x16, &cert[..200]));
        поток.extend(запись(0x15, &[0x02, 0x28]));
        поток.extend(запись(0x16, &cert[200..]));
        let seen = scan_records(&поток);
        assert!(seen.server_hello);
        assert!(seen.alert);
        assert!(!seen.certificate, "хвост после alert склеивать нельзя");
    }

    #[test]
    fn версия_берётся_из_расширения_а_не_из_факта_ответа() {
        // ServerHello 1.2: поле версии 0x0303, расширения пустые.
        let mut sh12 = vec![0x03, 0x03];
        sh12.extend(std::iter::repeat_n(0x11, 32)); // random
        sh12.push(0); // session_id_len
        sh12.extend_from_slice(&[0xc0, 0x2f]); // cipher
        sh12.push(0); // compression
        sh12.extend_from_slice(&[0x00, 0x00]); // ext_len = 0
        assert!(!server_hello_is_tls13(&sh12), "это 1.2");

        // ServerHello 1.3: то же, но с supported_versions = 0x0304.
        let mut sh13 = sh12.clone();
        sh13.truncate(sh13.len() - 2);
        sh13.extend_from_slice(&[0x00, 0x06]); // ext_len
        sh13.extend_from_slice(&[0x00, 0x2b, 0x00, 0x02, 0x03, 0x04]);
        assert!(server_hello_is_tls13(&sh13), "это 1.3");

        // Обрывки не должны ронять разбор.
        for n in 0..sh13.len() {
            let _ = server_hello_is_tls13(&sh13[..n]);
        }
    }

    #[test]
    fn вердикт_по_версии_tls() {
        use Tls13Verdict::*;
        // Ответил и согласовал 1.3 — всё хорошо.
        assert_eq!(classify_tls13(true, true, true).0, Ok);
        // Не ответил на 1.3, ответил на 1.2 — вот это блок.
        assert_eq!(classify_tls13(false, false, true).0, Blocked);
        // Молчит на обе — дело не в версии.
        assert_eq!(classify_tls13(false, false, false).0, NotApplicable);
    }

    #[test]
    fn без_повторов_ответное_направление_не_измерено() {
        // Раньше (0, 0, 0) давало «чисто»: правило объявляло чистым то,
        // чего не измеряло. Снаружи защищал вызывающий, но правило должно
        // быть верным и само по себе.
        let (v, why) = classify_response(0, 0, 0);
        assert_eq!(v, RespVerdict::NotApplicable);
        assert!(why.contains("не запускались"), "{why}");
    }

    #[test]
    fn сервер_без_tls13_не_объявляется_заблокированным() {
        // Сервер умеет только 1.2. На наш 1.3-способный ClientHello он
        // ОТВЕТИТ, просто согласует 1.2. Раньше вердикт строился на
        // «согласовал 1.3», и такой сервер получал «режут ClientHello 1.3».
        let (v, why) = classify_tls13(true, false, true);
        assert_eq!(v, Tls13Verdict::Ok, "{why}");
        assert!(why.contains("свойство"), "{why}");
    }

    #[test]
    fn запрет_tls13_убирает_его_расширения() {
        let v13 = build_client_hello_opts("discord.com", false).bytes;
        let v12 = build_client_hello_opts("discord.com", true).bytes;
        // 0x002b supported_versions и 0x0033 key_share существуют только ради 1.3.
        let есть = |b: &[u8], ext: [u8; 2]| b.windows(2).any(|w| w == ext);
        assert!(есть(&v13, [0x00, 0x2b]), "в 1.3-варианте supported_versions обязан быть");
        assert!(v12.len() < v13.len(), "1.2-вариант должен быть короче");
        // Имя на месте в обоих.
        let ch12 = build_client_hello_opts("discord.com", true);
        assert_eq!(
            &ch12.bytes[ch12.sni_offset..ch12.sni_offset + ch12.sni_len],
            b"discord.com"
        );
    }

    #[test]
    fn расширение_имени_объявлено_первым() {
        // Порядок сам по себе не критичен, но имя ближе к началу означает,
        // что разрез попадёт в первые сегменты — туда, куда смотрит коробка.
        let ch = build_client_hello("discord.com");
        assert!(ch.sni_offset < 120, "имя слишком глубоко: {}", ch.sni_offset);
    }
}


