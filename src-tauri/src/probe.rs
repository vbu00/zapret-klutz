use serde::Serialize;
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Имя для контрольного замера. `example.com` зарезервирован IANA под
/// примеры, в блок-листы не попадает и у провайдеров не режется — именно
/// это здесь и нужно.
pub const NEUTRAL_SNI: &str = "example.com";

/// Почему проба не прошла. Раньше на этом месте была строка, куда падало то
/// число из curl, то текст ошибки, то «no-response»: показать можно, а
/// ветвиться нельзя. Код стабильный и разложен по стадиям — по нему
/// принимает решение самолечение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Ok,
    /// Имя не разрешается.
    Dns,
    /// До порта не достучались: отказано или нет маршрута.
    TcpRefused,
    /// Молчание на любой стадии — самый частый почерк блокировки.
    Timeout,
    /// TCP поднялся, а рукопожатие TLS не состоялось: сброс или обрыв сразу
    /// после ClientHello. Классическая подпись DPI по имени.
    TlsFailed,
    /// Сертификат не прошёл проверку — но он БЫЛ. Значит байты дошли до
    /// сервера и вернулись, путь живой.
    TlsCert,
    /// Соединение установилось и тут же закрылось без единого байта ответа.
    EmptyReply,
    /// HTTP 451 — «недоступно по юридическим причинам». Типизированный
    /// блок: путь для клиента непригоден, но и стратегия его не чинит.
    HttpBlocked,
    /// Сервер ответил, передача пошла — и оборвалась. Рукопожатие тут ни
    /// при чём: режут уже установленный поток.
    Cutoff,
    Unknown,
}

impl FailureCode {
    /// Сервер жив и ответил сам. Отличать это от блокировки критично: на
    /// собственную политику сервера (403 на HEAD, ошибка сертификата,
    /// требование клиентского сертификата) никакая стратегия обхода не
    /// влияет, и перебирать их бессмысленно.
    /// Отдельного кода «сервер ответил HTTP» здесь нет намеренно: удавшийся
    /// HTTP-обмен — это успех, а не «провал, но сервер жив». Остаётся один
    /// случай: рукопожатие не состоялось, а сертификат мы всё-таки получили.
    pub fn server_reachable(self) -> bool {
        matches!(self, FailureCode::TlsCert)
    }

    /// Дошли ли мы вообще до стадии TLS. Если нет — режут адрес или порт,
    /// и про имя говорить рано.
    fn reached_tls(self) -> bool {
        !matches!(self, FailureCode::Dns | FailureCode::TcpRefused)
    }

    /// Нужен ли контрольный замер. Лишнее рукопожатие платим только там,
    /// где оно что-то решает: имя ушло на провод, а ответа не было.
    pub fn needs_control(self) -> bool {
        // Ok, Cutoff и 451 контролем не уточняются: в первом случае нечего
        // выяснять, во втором имя уже проехало, в третьем блокировка
        // объявлена прямым текстом. Лишнее рукопожатие за них не платим.
        self != FailureCode::Ok
            && self != FailureCode::Cutoff
            && self != FailureCode::HttpBlocked
            && self.reached_tls()
            && !self.server_reachable()
    }

    /// Короткое имя для интерфейса и логов.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureCode::Ok => "ok",
            FailureCode::Dns => "dns",
            FailureCode::TcpRefused => "tcp_refused",
            FailureCode::Timeout => "timeout",
            FailureCode::TlsFailed => "tls_failed",
            FailureCode::TlsCert => "tls_cert",
            FailureCode::EmptyReply => "empty_reply",
            FailureCode::HttpBlocked => "http_451",
            FailureCode::Cutoff => "cutoff",
            FailureCode::Unknown => "unknown",
        }
    }
}

/// Где блокируют: по имени или по адресу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathVerdict {
    /// Проба прошла, классифицировать нечего.
    Ok,
    /// Не измерено. Отдельно от всего остального: «не удалось проверить» —
    /// это НЕ «всё хорошо» и НЕ «заблокировано».
    Unknown,
    /// Типизированный блок: сервер (или коробка от его имени) ответил 451.
    /// Смена стратегии такое не лечит.
    Legal,
    /// Путь до сервера живой, режут по имени. Наш случай: десинхронизация
    /// работает именно с этим, перебор стратегий осмыслен.
    Sni,
    /// До адреса не доходит ничего, либо доходит, но сервер молчит на любое
    /// имя. Пакетными техниками это не обходится в принципе — нужен туннель
    /// или другой адрес.
    Ip,
    /// Ответил сам сервер. Его политика, а не цензура.
    Server,
    /// Соединение поднялось и отдавало данные, а потом его оборвали. Про
    /// имя это ничего не говорит: имя уже проехало.
    Cutoff,
}

/// Решает, где блокируют, по результату основной пробы и контрольной.
///
/// Приём описан в мануале zapret («Проверка блока по IP») и одинаково
/// реализован в z2k (MIT, `z2k-detect/internal/prober`): к ТОМУ ЖЕ адресу
/// стучимся с заведомо не заблокированным именем. Отвечает — путь живой,
/// значит режут имя. Молчит и на нейтральное — режут адрес.
///
/// Контроль обязан быть ДРУГИМ именем на ТОМ ЖЕ адресе. Взяли бы то же
/// самое — молчали бы оба, и «блок по адресу» получился бы из собственной
/// ошибки ввода.
///
/// Чистая функция: сеть дёргает вызывающий и передаёт сюда результат.
pub fn classify_path(
    ok: bool,
    code: FailureCode,
    control: Option<(bool, FailureCode)>,
) -> (PathVerdict, String) {
    if ok && !code.server_reachable() {
        return (PathVerdict::Ok, String::new());
    }
    if code.server_reachable() {
        // Осторожнее в формулировке. Сертификат ПРИШЁЛ, значит байты дошли
        // до чего-то и вернулись — путь живой, и это главное. Но чей это
        // сертификат, мы не разбирали: это может быть и сам сервер со своей
        // политикой, и заглушка провайдера, и антивирус с подменой. Раньше
        // здесь стояло уверенное «это его политика», то есть один из трёх
        // вариантов подавался как измеренный факт.
        return (
            PathVerdict::Server,
            "на том конце ответили, но проверку сертификата ответ не прошёл. Путь живой — \
             значит, это не блокировка по имени. Чаще всего так выглядит политика самого \
             сервера, реже — подмена: заглушка провайдера или антивирус со своим \
             сертификатом"
                .into(),
        );
    }
    if code == FailureCode::Cutoff {
        return (
            PathVerdict::Cutoff,
            "сервер ответил, а поток оборвали на середине — режут не имя, \
             а уже установленное соединение"
                .into(),
        );
    }
    if code == FailureCode::HttpBlocked {
        return (
            PathVerdict::Legal,
            "ответ 451 «недоступно по юридическим причинам» — это не DPI, \
             и стратегия обхода такое не чинит"
                .into(),
        );
    }
    if code == FailureCode::Dns {
        return (
            PathVerdict::Ip,
            "имя не разрешается — проблема в DNS, а не в обходе".into(),
        );
    }
    if !code.reached_tls() {
        return (
            PathVerdict::Ip,
            "до порта не достучались — режут адрес или порт, не имя".into(),
        );
    }
    // Контроля нет — значит НЕ ИЗМЕРЕНО. Раньше отсутствие замера кодировалось
    // как «контроль молчал» и превращалось в вердикт «режут адрес»: программа
    // уверенно заявляла то, чего не проверяла.
    let Some((control_ok, control_code)) = control else {
        return (
            PathVerdict::Unknown,
            "контрольный замер не выполнялся — где именно режут, неизвестно".into(),
        );
    };
    // «Не смогли измерить» — это не «адрес молчит». Unknown у контроля
    // означает, что curl не запустился или ответил непонятным, и делать из
    // этого вывод о блокировке нельзя.
    if control_code == FailureCode::Unknown {
        return (
            PathVerdict::Unknown,
            "контрольный замер не удался — где именно режут, осталось неизвестным".into(),
        );
    }
    if control_ok || control_code.server_reachable() {
        (
            PathVerdict::Sni,
            format!("с нейтральным именем {NEUTRAL_SNI} тот же адрес отвечает — режут по имени"),
        )
    } else {
        (
            PathVerdict::Ip,
            format!("с нейтральным именем {NEUTRAL_SNI} тот же адрес тоже молчит — режут адрес, не имя"),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    /// Пинг: время TCP-рукопожатия, один круг до сервера. Без запуска curl,
    /// без DNS и без TLS. Раньше здесь стояло время всего запроса от запуска
    /// curl.exe до выхода — и Discord показывал «197 мс» при пинге в 40.
    pub ms: u64,
    /// Полный ответ: соединение, TLS и запрос — сколько ждёт приложение.
    #[serde(rename = "totalMs")]
    pub total_ms: u64,
    pub reason: Option<String>,
    pub code: FailureCode,
}

/// Ответил ли сервер вообще.
///
/// Здесь меряется ровно одно: пережил ли TLS ClientHello дорогу до сервера.
/// Блокировка выглядит как отсутствие ответа — curl возвращает «000», а не
/// какой-нибудь код. Поэтому ЛЮБОЙ разобранный HTTP-код значит «связь есть»:
/// он не мог приехать иначе как по уже установленному TLS.
///
/// Это не теория. Вот что наши цели отвечают на HEAD по корню при рабочем
/// обходе: gateway.discord.gg — 404, cdn.discordapp.com — 403,
/// updates.discord.com — 404, redirector.googlevideo.com — 404,
/// api.steampowered.com и auth.riotgames.com — 404. Это штатные ответы, а не
/// блокировка: ни один из этих хостов не обязан отдавать 200 на «/».
///
/// В 1.2.0 здесь стояло `(200..400)`, и четыре цели из семи горели «нет
/// связи» при полностью рабочем обходе. Не сужать этот диапазон.
///
/// Исключение одно — 451 Unavailable For Legal Reasons: единственный код,
/// которым блокировку объявляют прямо.
///
/// Чего проба по-прежнему НЕ видит: подмену на страницу провайдера с кодом
/// 200. Для этого нужен разбор тела или сертификата, а не код ответа.
pub fn code_is_answer(code: u16) -> bool {
    code >= 100 && code != 451
}

/// Прибиваем curl к прямому соединению. Иначе прокси из окружения или из
/// пользовательского `.curlrc` тихо подменяет то, что мы меряем.
fn no_proxy(cmd: &mut Command) {
    cmd.arg("--noproxy").arg("*").arg("-q");
    for var in [
        "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
        "http_proxy", "https_proxy", "all_proxy", "no_proxy",
    ] {
        cmd.env_remove(var);
    }
}

/// Коды выхода curl — стабильная и документированная таблица, так что
/// раскладывать их по стадиям надёжнее, чем разбирать текст ошибки.
fn code_from_curl(exit: i32, status: Option<u16>) -> (bool, FailureCode) {
    // Ненулевой выход при УЖЕ разобранном коде ответа — это не «не дошли».
    // Сервер ответил, а оборвалось то, что шло после: ровно подпись отсечки
    // потока. Раньше такой случай раскладывался по стадии обрыва (35/56 →
    // «рукопожатие не состоялось»), и разговор уходил на имя, которое к делу
    // уже не относится.
    if exit != 0 {
        if let Some(st) = status.filter(|s| code_is_answer(*s)) {
            let _ = st;
            return (false, FailureCode::Cutoff);
        }
    }
    match exit {
        0 => match status {
            // 451 — «недоступно по юридическим причинам». Единственный
            // HTTP-код, который сам по себе означает блокировку.
            Some(451) => (false, FailureCode::HttpBlocked),
            // Любой другой разобранный код доказывает, что сервер жив:
            // правило одно на весь модуль, и оно измерено на живых целях.
            Some(s) if code_is_answer(s) => (true, FailureCode::Ok),
            _ => (false, FailureCode::Unknown),
        },
        6 => (false, FailureCode::Dns),
        7 => (false, FailureCode::TcpRefused),
        28 => (false, FailureCode::Timeout),
        // 35 — обрыв при установке TLS, 56 — сброс при приёме данных.
        35 | 56 => (false, FailureCode::TlsFailed),
        52 => (false, FailureCode::EmptyReply),
        // 51/60 — сертификат не прошёл проверку. Сервер при этом ОТВЕТИЛ.
        51 | 60 => (false, FailureCode::TlsCert),
        _ => (false, FailureCode::Unknown),
    }
}

fn reason_text(code: FailureCode) -> Option<String> {
    match code {
        FailureCode::Ok => None,
        other => Some(other.as_str().to_string()),
    }
}

/// Настоящий HTTPS-запрос через curl.
///
/// Важно, почему не TCP-хендшейк: блокировка Discord/YouTube срабатывает на
/// TLS ClientHello (SNI), который уходит уже ПОСЛЕ того, как TCP-хендшейк
/// завершился. Голая TCP-проба поэтому возвращает «ОК» ровно до того места,
/// где соединение и убивают, и показывает зелёный статус при нерабочем
/// сервисе. Полный запрос доходит до TLS и видит реальную картину.
///
/// `pin_ip` прибивает запрос к конкретному адресу (`--resolve`). Это нужно
/// контрольному замеру: сравнивать имена имеет смысл только на ОДНОМ адресе,
/// иначе разница объясняется разными серверами, а не блокировкой.
pub fn http_probe_pinned(host: &str, port: u16, pin_ip: Option<&str>, timeout_sec: u64) -> ProbeResult {
    let started = Instant::now();
    let scheme = if port == 443 { "https" } else { "http" };
    // Порт обязан попасть в URL: иначе curl шёл бы на стандартный для схемы,
    // а --resolve прибивал совсем другой — и замер мерил бы не то.
    let url = if port == 443 || port == 80 {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    };

    #[allow(unused_mut)]
    let mut cmd = Command::new(crate::sys::system_exe("curl.exe"));
    // Окружение наследуется от того, кто нас запустил. Заданный там
    // HTTPS_PROXY увёл бы запрос через прокси, и мерили бы мы прокси, а не
    // путь до цели: вердикт «режут по имени» стал бы выдумкой.
    no_proxy(&mut cmd);
    cmd.args([
        "-s",
        "-o",
        "NUL",
        "-w",
        "%{http_code} %{time_namelookup} %{time_connect} %{time_total}",
        "-m",
        &timeout_sec.to_string(),
    ]);
    if let Some(ip) = pin_ip {
        cmd.args(["--resolve", &format!("{host}:{port}:{ip}")]);
    }
    cmd.args(["-I", &url]);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    match cmd.output() {
        Ok(out) => {
            let elapsed = started.elapsed().as_millis() as u64;
            let (status, rtt, total) = parse_timings(&String::from_utf8_lossy(&out.stdout));
            let exit = out.status.code().unwrap_or(-1);
            let (ok, code) = code_from_curl(exit, status);
            ProbeResult {
                ok,
                ms: rtt.unwrap_or(elapsed),
                total_ms: total.unwrap_or(elapsed),
                reason: reason_text(code),
                code,
            }
        }
        Err(e) => {
            let elapsed = started.elapsed().as_millis() as u64;
            ProbeResult {
                ok: false,
                ms: elapsed,
                total_ms: elapsed,
                reason: Some(e.to_string()),
                code: FailureCode::Unknown,
            }
        }
    }
}

/// Вывод `-w "%{http_code} %{time_namelookup} %{time_connect} %{time_total}"`:
/// код ответа, пинг и полный ответ в мс. Таймеры curl, а не часы вокруг
/// процесса: в те попадали запуск curl.exe и то, что все цели проверяются
/// разом и делят процессор. Пинг — соединение минус DNS; нулевое время
/// соединения значит, что соединения не было, и пинга нет.
pub fn parse_timings(raw: &str) -> (Option<u16>, Option<u64>, Option<u64>) {
    let mut f = raw.split_whitespace();
    let status = f.next().and_then(|v| v.parse::<u16>().ok()).filter(|s| *s >= 100);
    let secs = |v: Option<&str>| v.and_then(|v| v.parse::<f64>().ok());
    let lookup = secs(f.next()).unwrap_or(0.0);
    let connect = secs(f.next()).filter(|c| *c > 0.0);
    let total = secs(f.next()).filter(|t| *t > 0.0);
    let ms = |s: f64| (s * 1000.0).round() as u64;
    (status, connect.map(|c| ms((c - lookup).max(0.0))), total.map(ms))
}

/// Сколько байт надо получить, чтобы говорить, что поток пережил окно
/// отсечки. Полоса, в которой обрывают, по замерам соседей — 14–34 КБ;
/// 64 КБ заведомо за ней, и меньше брать нельзя: «скачалось 20 КБ целиком»
/// ничего не доказывает, через потолок надо реально перелезть.
pub const VOLUME_FLOOR: u64 = 64 * 1024;

/// Сколько байт просим у сервера. Ровно вдвое больше потолка: чтобы
/// доказать, что поток пережил окно отсечки, дальше качать нечего, а
/// страница ютуба — почти мегабайт за каждую проверку. На мобильном
/// интернете предпроверка перед прогоном это заметный трафик.
pub const VOLUME_FETCH: u64 = VOLUME_FLOOR * 2;

// Просить меньше потолка бессмысленно: «перелезли» тогда ничего не докажет.
// Проверка на сборке, а не в тесте: обе величины константные, и clippy
// справедливо ругался на утверждение с заранее известным значением.
const _: () = assert!(VOLUME_FETCH > VOLUME_FLOOR);

/// Что стало с потоком после того, как сервер ответил.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeVerdict {
    /// Мерить нечего: сервер не ответил вовсе — это вопрос к рукопожатию.
    NotApplicable,
    /// Перелезли через потолок — отсечки по объёму здесь нет.
    Clear,
    /// Поток оборвали, не дойдя до потолка.
    Cutoff,
    /// Ответ пришёл целиком, но он меньше потолка: доказательства нет.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct VolumeResult {
    pub verdict: VolumeVerdict,
    pub note: String,
    /// Сколько байт тела получили.
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

/// Чистое правило. `complete` — завершилась ли передача штатно.
pub fn classify_volume(status: Option<u16>, bytes: u64, complete: bool) -> (VolumeVerdict, String) {
    let Some(st) = status.filter(|s| code_is_answer(*s)) else {
        return (
            VolumeVerdict::NotApplicable,
            "сервер не ответил — про объём говорить рано, дело в рукопожатии".into(),
        );
    };
    let _ = st;
    if bytes >= VOLUME_FLOOR {
        return (
            VolumeVerdict::Clear,
            format!("получено {} КБ — поток пережил окно, в котором обрывают", bytes / 1024),
        );
    }
    if complete {
        return (
            VolumeVerdict::Unknown,
            format!(
                "ответ пришёл целиком, но в нём всего {} КБ — через потолок в {} КБ \
                 перелезть не вышло, и отсечку это не проверяет",
                bytes / 1024,
                VOLUME_FLOOR / 1024
            ),
        );
    }
    (
        VolumeVerdict::Cutoff,
        format!(
            "сервер ответил, отдал {} КБ и замолчал. Рукопожатие прошло, режут уже \
             установленный поток — сменой стратегии это обычно не лечится, нужен \
             другой маршрут или туннель",
            bytes / 1024
        ),
    )
}

/// Скачивает ответ целиком и смотрит, доехал ли он.
///
/// Зачем отдельно от основной пробы: та ходит методом HEAD и тела не
/// получает вовсе. Есть класс блокировок, где рукопожатие проходит
/// безупречно, первые килобайты идут, а поток умирает на втором десятке —
/// человек видит «ютуб открывается и не грузится», а проба показывает
/// зелёное. Дорого, поэтому зовём не в мониторинге, а перед прогоном.
pub fn http_probe_volume(
    host: &str,
    port: u16,
    pin_ip: Option<&str>,
    timeout_sec: u64,
) -> VolumeResult {
    let scheme = if port == 443 { "https" } else { "http" };
    let url = if port == 443 || port == 80 {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    };

    // Один запрос. `ranged` — просить ли только первые VOLUME_FETCH байт.
    let fetch = |ranged: bool| -> Option<(Option<u16>, u64, bool)> {
        #[allow(unused_mut)]
        let mut cmd = Command::new(crate::sys::system_exe("curl.exe"));
        no_proxy(&mut cmd);
        cmd.args([
            "-s",
            "-o",
            "NUL",
            "-w",
            "%{http_code} %{size_download}",
            "-m",
            &timeout_sec.to_string(),
            // Браузерный User-Agent: часть целей отдаёт обрезанную заглушку в
            // ответ на пустой, и мерили бы мы её, а не настоящую страницу.
            "-H",
            "User-Agent: Mozilla/5.0",
        ]);
        if ranged {
            cmd.args(["--range", &format!("0-{}", VOLUME_FETCH - 1)]);
        }
        if let Some(ip) = pin_ip {
            cmd.args(["--resolve", &format!("{host}:{port}:{ip}")]);
        }
        cmd.arg(&url);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let out = cmd.output().ok()?;
        let raw = String::from_utf8_lossy(&out.stdout);
        let mut parts = raw.split_whitespace();
        let status = parts.next().and_then(|v| v.parse::<u16>().ok()).filter(|s| *s >= 100);
        let bytes = parts.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        Some((status, bytes, out.status.success()))
    };

    // Сервер, не принимающий диапазон, отвечает 416: он «ответил», но тела
    // нет — без повтора это выглядело бы как «ответ целиком и он крошечный».
    let measured = match fetch(true) {
        Some((Some(416), _, _)) => fetch(false),
        other => other,
    };
    let Some((status, bytes, complete)) = measured else {
        return VolumeResult {
            verdict: VolumeVerdict::NotApplicable,
            note: "не удалось запустить curl".into(),
            bytes: 0,
            status: None,
        };
    };
    let (verdict, note) = classify_volume(status, bytes, complete);
    VolumeResult { verdict, note, bytes, status }
}

/// Похоже ли, что имя разрешилось не в настоящий сервер, а во что-то
/// местное. Тогда мы меряем не провайдера, а собственный туннель, и любой
/// вывод про блокировку будет не о том.
///
/// Что берём и чего НЕ берём. `198.18.0.0/15` — диапазон для замеров
/// пропускной способности, его же раздают fake-IP прокси; `240.0.0.0/4` —
/// зарезервированный класс E, в DNS ему взяться неоткуда; петля и частные
/// сети означают подмену в hosts или локальную заглушку. А вот
/// `100.64.0.0/10` не берём намеренно: это ещё и обычный CGNAT, на нём
/// сидят целые провайдеры, и вывод получился бы ложным у половины страны.
pub fn tunnel_hint(ip: &str) -> Option<&'static str> {
    let Ok(addr) = ip.parse::<std::net::IpAddr>() else { return None };
    let std::net::IpAddr::V4(v4) = addr else { return None };
    let o = v4.octets();
    if v4.is_loopback() {
        return Some("имя разрешилось в петлю — отвечает что-то на этой же машине");
    }
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return Some("адрес из диапазона для замеров (198.18/15) — так выглядит fake-IP прокси");
    }
    if o[0] >= 240 {
        return Some("адрес из зарезервированного класса E — настоящим он быть не может");
    }
    if v4.is_private() {
        return Some("имя разрешилось в частную сеть — подмена в hosts или локальная заглушка");
    }
    None
}

/// Сколько адресов цели проверяем, прежде чем объявить её недоступной.
/// У Discord и YouTube их десятки; выше трёх смысла нет — если три edge
/// подряд молчат, дело не в конкретном edge.
pub const MAX_IPS: usize = 3;

/// Адреса, в которые разрешается имя, — до `MAX_IPS` штук, без повторов.
///
/// Зачем не один: `first_ip` брал ровно первый, и одного мёртвого или
/// перегруженного edge хватало, чтобы цель загорелась «блокировка». У
/// крупных CDN это обычное дело, а вывод из этого делался громкий.
pub fn resolve_ips(host: &str, port: u16) -> Vec<String> {
    let Ok(addrs) = (host, port).to_socket_addrs() else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for a in addrs {
        let ip = a.ip().to_string();
        if !out.contains(&ip) {
            out.push(ip);
        }
        if out.len() == MAX_IPS {
            break;
        }
    }
    out
}

/// Первый адрес, в который разрешается имя. Контрольный замер обязан идти в
/// тот же самый — иначе сравнивать нечего.
pub fn first_ip(host: &str, port: u16) -> Option<String> {
    (host, port)
        .to_socket_addrs()
        .ok()?
        .next()
        .map(|a| a.ip().to_string())
}

/// TCP-хендшейк — для игровых серверов, где HTTP отсутствует как таковой.
/// Даёт осмысленную задержку, но НЕ видит блокировку на уровне TLS.
pub fn tcp_probe(host: &str, port: u16, timeout_ms: u64) -> ProbeResult {
    let started = Instant::now();
    let addr_iter = match (host, port).to_socket_addrs() {
        Ok(it) => it,
        Err(_) => {
            let elapsed = started.elapsed().as_millis() as u64;
            return ProbeResult {
                ok: false,
                ms: elapsed,
                total_ms: elapsed,
                reason: Some("dns".into()),
                code: FailureCode::Dns,
            };
        }
    };
    // Бюджет один на весь вызов. Раньше таймаут отсчитывался заново для
    // каждого адреса из DNS, и хост с восемью A-записями отваливался не за
    // 4 секунды, а за 32 — «Проверить связь» висла на полминуты.
    let budget = Duration::from_millis(timeout_ms);
    // Отказ и молчание — разные вещи, и раньше оба назывались таймаутом.
    // Немедленный отказ означает живой узел, который закрыл этот порт;
    // молчание — что до узла не доходит. Для игрового сервера это два
    // совершенно разных разговора с пользователем.
    let mut refused = false;
    for addr in addr_iter {
        let left = budget.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        // Пинг — только само соединение с ответившим адресом. Раньше часы
        // шли с начала вызова, и в «пинг» попадали DNS и адреса, которые
        // перед этим не ответили.
        let attempt = Instant::now();
        match TcpStream::connect_timeout(&addr, left) {
            Ok(_) => {
                return ProbeResult {
                    ok: true,
                    ms: attempt.elapsed().as_millis() as u64,
                    total_ms: started.elapsed().as_millis() as u64,
                    reason: None,
                    code: FailureCode::Ok,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => refused = true,
            Err(_) => {}
        }
    }
    let code = if refused { FailureCode::TcpRefused } else { FailureCode::Timeout };
    let elapsed = started.elapsed().as_millis() as u64;
    ProbeResult {
        ok: false,
        ms: elapsed,
        total_ms: elapsed,
        reason: Some(code.as_str().to_string()),
        code,
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn пинг_это_соединение_а_не_весь_запрос() {
        // Живые цифры: соединение 45 мс, DNS 3 мс, весь запрос 197 мс.
        let (status, rtt, total) = parse_timings("404 0.003120 0.048210 0.197400");
        assert_eq!(status, Some(404));
        assert_eq!(rtt, Some(45));
        assert_eq!(total, Some(197));
        // С --resolve DNS нулевой — пинг равен соединению.
        assert_eq!(parse_timings("200 0.000000 0.041000 0.150000").1, Some(41));
        // Соединения не было — пинга нет, код тоже.
        assert_eq!(parse_timings("000 0.002000 0.000000 4.001000"), (None, None, Some(4001)));
        assert_eq!(parse_timings(""), (None, None, None));
    }

    #[test]
    fn контроль_отвечает_значит_режут_имя() {
        let (v, why) = classify_path(false, FailureCode::TlsFailed, Some((true, FailureCode::Ok)));
        assert_eq!(v, PathVerdict::Sni);
        assert!(why.contains(NEUTRAL_SNI));
    }

    #[test]
    fn контроль_тоже_молчит_значит_режут_адрес() {
        let (v, _) = classify_path(false, FailureCode::TlsFailed, Some((false, FailureCode::Timeout)));
        assert_eq!(v, PathVerdict::Ip);
    }

    #[test]
    fn ответ_сервера_не_блокировка() {
        // Сертификат не прошёл проверку — но он БЫЛ, значит сервер жив.
        let (v, _) = classify_path(false, FailureCode::TlsCert, Some((false, FailureCode::Timeout)));
        assert_eq!(v, PathVerdict::Server);
    }

    #[test]
    fn до_порта_не_дошли_контроль_не_спрашиваем() {
        // Контроль тут неинформативен: имени на проводе ещё не было.
        for code in [FailureCode::TcpRefused, FailureCode::Dns] {
            let (v, _) = classify_path(false, code, Some((true, FailureCode::Ok)));
            assert_eq!(v, PathVerdict::Ip, "{code:?}");
        }
    }

    #[test]
    fn успешная_проба_не_классифицируется() {
        let (v, why) = classify_path(true, FailureCode::Ok, Some((false, FailureCode::Timeout)));
        assert_eq!(v, PathVerdict::Ok);
        assert!(why.is_empty());
    }

    #[test]
    fn контроль_с_чужим_сертификатом_считается_ответом() {
        // Нейтральное имя прибито к чужому адресу, сертификат не совпадёт —
        // но ответ TLS-уровня получен, значит путь живой.
        let (v, _) = classify_path(false, FailureCode::TlsFailed, Some((false, FailureCode::TlsCert)));
        assert_eq!(v, PathVerdict::Sni);
    }

    #[test]
    fn без_контроля_вердикт_не_измерено() {
        // Раньше отсутствие замера кодировалось как «контроль молчал» и
        // превращалось в уверенное «режут адрес» — программа заявляла то,
        // чего не проверяла.
        let (v, why) = classify_path(false, FailureCode::TlsFailed, None);
        assert_eq!(v, PathVerdict::Unknown);
        assert!(why.contains("не выполнялся"), "{why}");
    }

    #[test]
    fn неудавшийся_контроль_это_не_блок_по_адресу() {
        // Контроль не измерился (curl не запустился, ответил непонятным) —
        // это «неизвестно», а не «адрес молчит». Раньше любой контроль,
        // кроме успеха и сертификата, давал уверенное «режут адрес».
        let (v, why) = classify_path(false, FailureCode::TlsFailed, Some((false, FailureCode::Unknown)));
        assert_eq!(v, PathVerdict::Unknown, "{why}");
        assert!(why.contains("не удался"), "{why}");
    }

    #[test]
    fn за_451_контроль_не_платим() {
        // Блокировка объявлена прямым текстом — уточнять нечего.
        assert!(!FailureCode::HttpBlocked.needs_control());
        assert!(!FailureCode::Cutoff.needs_control());
        assert!(!FailureCode::Ok.needs_control());
        // А вот обрыв TLS без ответа уточнять надо.
        assert!(FailureCode::TlsFailed.needs_control());
        assert!(FailureCode::Timeout.needs_control());
    }

    #[test]
    fn код_451_не_повод_перебирать_стратегии() {
        // Контроль на example.com ответит, и по общему правилу вышло бы
        // «режут по имени» — то есть бесполезный перебор.
        let (v, why) = classify_path(false, FailureCode::HttpBlocked, Some((true, FailureCode::Ok)));
        assert_eq!(v, PathVerdict::Legal, "{why}");
        assert!(why.contains("451"), "{why}");
    }

    #[test]
    fn коды_curl_раскладываются_по_стадиям() {
        assert_eq!(code_from_curl(0, Some(200)), (true, FailureCode::Ok));
        assert_eq!(code_from_curl(0, Some(403)), (true, FailureCode::Ok));
        assert_eq!(code_from_curl(0, Some(451)), (false, FailureCode::HttpBlocked));
        assert_eq!(code_from_curl(6, None), (false, FailureCode::Dns));
        assert_eq!(code_from_curl(7, None), (false, FailureCode::TcpRefused));
        assert_eq!(code_from_curl(28, None), (false, FailureCode::Timeout));
        assert_eq!(code_from_curl(35, None), (false, FailureCode::TlsFailed));
        assert_eq!(code_from_curl(56, None), (false, FailureCode::TlsFailed));
        assert_eq!(code_from_curl(60, None), (false, FailureCode::TlsCert));
    }

    #[test]
    fn отсечка_потока_это_не_провал_рукопожатия() {
        // Сервер ответил 200, а потом передачу оборвали (curl 18 —
        // «передана часть файла», 56 — обрыв при приёме). Раньше это
        // раскладывалось по стадии обрыва и уходило в разговор про имя,
        // которое к тому моменту давно проехало.
        for exit in [18, 56, 92] {
            assert_eq!(code_from_curl(exit, Some(200)), (false, FailureCode::Cutoff), "{exit}");
        }
        // Без разобранного кода ответа всё по-прежнему: это стадия обрыва.
        assert_eq!(code_from_curl(56, None), (false, FailureCode::TlsFailed));
        assert_eq!(code_from_curl(28, Some(0)), (false, FailureCode::Timeout));
    }

    #[test]
    fn отсечка_не_спрашивает_контроль_и_не_путается_с_именем() {
        // Контроль нейтральным именем тут бесполезен: имя уже проехало.
        assert!(!FailureCode::Cutoff.needs_control());
        let (v, why) = classify_path(false, FailureCode::Cutoff, None);
        assert_eq!(v, PathVerdict::Cutoff);
        assert!(why.contains("установленное соединение"), "{why}");
    }

    #[test]
    fn отказ_порта_не_называется_таймаутом() {
        // Порт, который заведомо никто не слушает на петле, отвечает
        // отказом. Раньше это называлось таймаутом — то есть «до узла не
        // доходит», хотя узел как раз ответил.
        //
        // Бюджет не жалеем: Windows сообщает об отказе не мгновенно, а
        // примерно через две секунды — столько она переспрашивает SYN.
        // Боевой вызов идёт с теми же четырьмя секундами (targets.rs).
        let r = tcp_probe("127.0.0.1", 1, 4000);
        assert!(!r.ok);
        assert_eq!(r.code, FailureCode::TcpRefused, "{:?}", r.reason);
    }

    #[test]
    fn туннель_узнаётся_по_диапазону() {
        assert!(tunnel_hint("198.18.0.7").is_some(), "fake-IP прокси");
        assert!(tunnel_hint("198.19.255.1").is_some(), "вторая половина /15");
        assert!(tunnel_hint("240.0.0.1").is_some(), "класс E");
        assert!(tunnel_hint("127.0.0.1").is_some(), "петля");
        assert!(tunnel_hint("192.168.1.1").is_some(), "частная сеть");
        assert!(tunnel_hint("10.0.0.5").is_some(), "частная сеть");
    }

    #[test]
    fn обычные_адреса_за_туннель_не_принимаются() {
        for ip in [
            "162.159.135.232", // Discord
            "142.250.74.110",  // Google
            "198.17.0.1",      // рядом с 198.18/15, но не он
            "198.20.0.1",
            // CGNAT берём отдельной строкой: на нём сидят настоящие
            // провайдеры, и объявлять его туннелем нельзя.
            "100.64.0.1",
            "100.127.255.254",
        ] {
            assert!(tunnel_hint(ip).is_none(), "{ip} принят за туннель");
        }
        // IPv6 и мусор молчат, а не паникуют.
        assert!(tunnel_hint("2606:4700::1").is_none());
        assert!(tunnel_hint("не адрес").is_none());
    }

    #[test]
    #[ignore]
    fn живой_список_адресов_без_повторов() {
        for host in ["www.youtube.com", "discord.com", "cdn.discordapp.com"] {
            let ips = resolve_ips(host, 443);
            println!("{host}: {ips:?}");
            assert!(!ips.is_empty(), "{host}: ни одного адреса");
            assert!(ips.len() <= MAX_IPS, "{host}: больше потолка");
            let mut uniq = ips.clone();
            uniq.sort();
            uniq.dedup();
            assert_eq!(uniq.len(), ips.len(), "{host}: есть повторы");
        }
    }

    /// Диапазонный запрос получает 206, и это полноценный ответ сервера:
    /// иначе экономия трафика превратила бы «поток чист» в «сервер не
    /// ответил».
    #[test]
    fn частичный_ответ_считается_ответом() {
        assert!(code_is_answer(206), "206 — ответ на диапазонный запрос");
        let (verdict, _) = classify_volume(Some(206), VOLUME_FETCH, true);
        assert_eq!(verdict, VolumeVerdict::Clear);
    }

    /// Живая проверка самой функции, а не правила: строится ли командная
    /// строка, разбирается ли «код пробел байты». Сети в CI нет, поэтому
    /// вручную: `cargo test -- --ignored живой_объём --nocapture`.
    #[test]
    #[ignore]
    fn живой_объём_на_настоящих_целях() {
        for host in ["www.youtube.com", "discord.com", "cdn.discordapp.com"] {
            let r = http_probe_volume(host, 443, None, 20);
            println!("{host}: {:?} {} байт, статус {:?}", r.verdict, r.bytes, r.status);
            assert!(r.status.is_some(), "{host}: код ответа не разобран");
        }
    }

    #[test]
    fn объём_перелезли_через_потолок() {
        let (v, why) = classify_volume(Some(200), VOLUME_FLOOR, true);
        assert_eq!(v, VolumeVerdict::Clear, "{why}");
    }

    #[test]
    fn объём_маленькая_страница_ничего_не_доказывает() {
        // Целиком скачанные 20 КБ не проверяют потолок в 64 КБ.
        let (v, why) = classify_volume(Some(200), 20 * 1024, true);
        assert_eq!(v, VolumeVerdict::Unknown, "{why}");
        assert!(why.contains("перелезть"), "{why}");
    }

    #[test]
    fn объём_оборвали_на_середине() {
        // Ровно подпись класса: ответ есть, до потолка не дошли, поток умер.
        let (v, why) = classify_volume(Some(200), 18 * 1024, false);
        assert_eq!(v, VolumeVerdict::Cutoff, "{why}");
        // И обрыв сразу после заголовков — тоже отсечка, а не «нет связи».
        assert_eq!(classify_volume(Some(200), 0, false).0, VolumeVerdict::Cutoff);
    }

    #[test]
    fn объём_без_ответа_не_измеряется() {
        for st in [None, Some(451)] {
            let (v, why) = classify_volume(st, 0, false);
            assert_eq!(v, VolumeVerdict::NotApplicable, "{st:?}");
            assert!(why.contains("рукопожатии"), "{why}");
        }
    }

    #[test]
    fn код_ноль_ноль_ноль_больше_не_успех() {
        // Старое правило «три цифры и не начинается с нуля» пропускало
        // только 000; теперь опираемся на код выхода curl, а не на текст.
        assert!(!code_from_curl(35, Some(0)).0);
        assert!(!code_from_curl(0, None).0);
    }

    /// Настоящие ответы наших целей на HEAD по «/» при РАБОЧЕМ обходе,
    /// снятые curl-ом 12.09.2026. Ни один из этих хостов не обязан отдавать
    /// 200 на корень, и все эти коды приехали по установленному TLS.
    const ЖИВЫЕ_ЦЕЛИ: &[(&str, u16)] = &[
        ("discord.com", 200),
        ("gateway.discord.gg", 404),
        ("cdn.discordapp.com", 403),
        ("updates.discord.com", 404),
        ("www.youtube.com", 200),
        ("redirector.googlevideo.com", 404),
        ("youtu.be", 303),
        ("api.steampowered.com", 404),
        ("auth.riotgames.com", 404),
        ("api.epicgames.dev", 404),
    ];

    #[test]
    fn все_стандартные_цели_при_рабочем_обходе_считаются_живыми() {
        // В 1.2.0 здесь стояло (200..400) — и четыре цели из семи показывали
        // «нет связи», хотя обход работал. Этот тест не даст сузить диапазон
        // снова: он перечисляет коды, которые эти хосты отдают на самом деле.
        for (host, code) in ЖИВЫЕ_ЦЕЛИ {
            assert!(
                code_is_answer(*code),
                "{host} отвечает {code} при рабочем обходе — это связь, а не блокировка"
            );
        }
    }

    #[test]
    fn блокировка_это_отсутствие_ответа_а_не_код_ошибки() {
        // Сервер ответил — значит TLS прошёл, каким бы ни был код.
        for c in [400, 403, 404, 429, 500, 502, 503] {
            assert!(code_is_answer(c), "{c} — ответ сервера, TLS до него доехал");
        }
        // А вот 451 объявляет блокировку прямым текстом.
        assert!(!code_is_answer(451));
        // Ноль curl пишет, когда ответа не было вовсе; до code_is_answer он
        // не доходит (http_probe_pinned отсеивает всё ниже 100), но правило
        // должно быть верным и там.
        assert!(!code_is_answer(0));
    }
}
