use once_cell::sync::Lazy;
use regex::Regex;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use tauri::{AppHandle, Emitter, Manager};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use crate::state::AppState;

struct GameFilterValues {
    game_filter: &'static str,
    game_filter_tcp: &'static str,
    game_filter_udp: &'static str,
}

/// Port of `getGameFilterValues()` — reads the same utils\game_filter.enabled
/// marker file the .bat scripts themselves read.
fn game_filter_values(root: &Path) -> GameFilterValues {
    let marker = root.join("utils").join("game_filter.enabled");
    let mode = fs::read_to_string(marker)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    match mode.as_str() {
        "tcp" => GameFilterValues {
            game_filter: "1024-65535",
            game_filter_tcp: "1024-65535",
            game_filter_udp: "12",
        },
        "udp" => GameFilterValues {
            game_filter: "1024-65535",
            game_filter_tcp: "12",
            game_filter_udp: "1024-65535",
        },
        "" => GameFilterValues {
            game_filter: "12",
            game_filter_tcp: "12",
            game_filter_udp: "12",
        },
        _ => GameFilterValues {
            game_filter: "1024-65535",
            game_filter_tcp: "1024-65535",
            game_filter_udp: "1024-65535",
        },
    }
}

// pub(crate): тем же разбором сравниваются конфиги двух релизов (configdiff) —
// свой разбор там разошёлся бы с тем, как .bat на самом деле запускается.
pub(crate) static LINE_CONTINUATION: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]*\^\r?\n[ \t]*").unwrap());
pub(crate) static EXE_MARKER: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)winws\.exe""#).unwrap());
pub(crate) static TOKEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?:[^\s"]+|"[^"]*")+"#).unwrap());

/// Токен строки запуска так, как его получит winws после cmd: кавычки
/// снимаются, а `^x` вне кавычек становится `x` — это экранирование cmd.
///
/// Раньше снимались только кавычки. В FAKE TLS AUTO у Flowseal стоит
/// `--dpi-desync-fake-tls=^!`: из .bat winws получает `!` (встроенная
/// подложка), а Klutz передавал `^!` как имя файла — winws сразу выходил,
/// и конфиг «не запускался», хотя в тестах (они запускают сам .bat) работал.
pub(crate) fn unescape_token(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut quoted = false;
    let mut chars = t.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '^' if !quoted => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Port of `extractWinwsArgs()` — reads a general*.bat and pulls out the
/// literal argv winws.exe gets launched with, so we can spawn it directly
/// (piped stdio) instead of via `start /min`, which would detach it into an
/// untracked console we can't read logs from. Returns None if the file
/// doesn't look like the expected shape.
pub fn extract_winws_args(root: &Path, file_name: &str) -> Option<Vec<String>> {
    let file_path = root.join(file_name);
    let raw = fs::read_to_string(&file_path).ok()?;
    let text = LINE_CONTINUATION.replace_all(&raw, " ");

    let line_with_exe = text.lines().find(|l| EXE_MARKER.is_match(l))?;
    let mat = EXE_MARKER.find(line_with_exe)?;
    let after_exe = &line_with_exe[mat.end()..];
    if after_exe.trim().is_empty() {
        return None;
    }

    let bin_path = format!("{}\\", root.join("bin").display());
    let lists_path = format!("{}\\", root.join("lists").display());
    let gf = game_filter_values(root);

    let args: Vec<String> = TOKEN_RE
        .find_iter(after_exe)
        .map(|m| unescape_token(m.as_str()))
        .map(|t| {
            t.replace("%BIN%", &bin_path)
                .replace("%LISTS%", &lists_path)
                .replace("%GameFilterTCP%", gf.game_filter_tcp)
                .replace("%GameFilterUDP%", gf.game_filter_udp)
                .replace("%GameFilter%", gf.game_filter)
        })
        .filter(|t| !t.is_empty() && t != "^")
        .collect();

    if args.is_empty() || !args.iter().any(|a| a.starts_with("--")) {
        return None;
    }
    Some(args)
}

/// `taskkill /IM winws.exe /F` — same as `stopWinws()`. Best-effort: a
/// missing process is not an error here, same as the JS version ignoring it.
pub fn stop_winws() {
    #[allow(unused_mut)]
    let mut cmd = Command::new(crate::sys::system_exe("taskkill.exe"));
    cmd.args(["/IM", "winws.exe", "/F"]);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.output();
}

pub fn is_winws_running() -> bool {
    crate::sys::proc_running("winws.exe")
}

const LOG_CAP: usize = 500;

fn push_log_lines(app: &AppHandle, state: &AppState, chunk: &str) {
    let lines: Vec<String> = chunk
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    if lines.is_empty() {
        return;
    }
    // Пока идёт сбор адресов игры, каждая строка проходит через копилку.
    // Дешевле некуда: если сбор не идёт, копилка сразу возвращается.
    for line in &lines {
        crate::gamescan::harvest_line(line);
    }
    {
        let mut buf = state.winws_log.lock().unwrap();
        for line in &lines {
            buf.push(line.clone());
        }
        let overflow = buf.len().saturating_sub(LOG_CAP);
        if overflow > 0 {
            buf.drain(0..overflow);
        }
    }
    let _ = app.emit("winws-log", &lines);
}

/// Port of `applyDirect()`'s spawn half — parses the config's real args and
/// spawns winws.exe directly with piped stdio so live logs work, same as the
/// Electron version. Falls back to nothing (caller decides what "no live
/// logs" means) when the args can't be extracted, matching the "returns
/// null, caller falls back to the .bat" contract upstream.
/// Был ли у последнего запуска живой лог. Глубокому сбору это знать
/// обязательно: без лога он смотрит в пустоту и сообщал бы, что игра
/// молчит, хотя молчим мы сами.
static LAST_RUN_HAD_LOGS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn last_run_had_logs() -> bool {
    LAST_RUN_HAD_LOGS.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn spawn_winws(app: &AppHandle, root: &Path, file_name: &str) -> Result<bool, String> {
    let state = app.state::<AppState>();
    // Во время сбора адресов winws запускается с `--debug`: только тогда он
    // печатает пакеты, которые видит, а вместе с ними адреса игрового UDP,
    // которых в таблице сокетов нет. Флаг живёт ровно на время сбора — вывод
    // с ним очень обильный, держать его постоянно незачем.
    let debug = crate::gamescan::harvest_active();

    // Без пользовательских списков winws не стартует вовсе: в строке запуска
    // стоят --hostlist на файлы, которых в поставке нет (их создаёт
    // service.bat, а мы запускаем winws напрямую).
    crate::maintenance::ensure_user_lists(root);

    // Kill whatever's running first — same "stop before start" as applyDirect().
    {
        let mut child_guard = state.winws_child.lock().unwrap();
        if let Some(mut child) = child_guard.take() {
            *state.winws_intentional_stop.lock().unwrap() = true;
            let _ = child.kill();
        }
    }
    stop_winws();
    std::thread::sleep(std::time::Duration::from_millis(400));

    state.winws_log.lock().unwrap().clear();

    let args = extract_winws_args(root, file_name);
    let winws_exe = root.join("bin").join("winws.exe");
    let live_logs = args.is_some() && winws_exe.exists();

    if let Some(mut args) = args.filter(|_| winws_exe.exists()) {
        if debug {
            // Именно со значением. У winws это `--debug=0|1|syslog|@<файл>`,
            // и голый флаг он не принимает — я передавал его без значения, и
            // подробный режим не включался вовсе. Сбор при этом «работал»:
            // разбирал обычный вывод и находил один-два адреса вместо
            // десятков, из-за чего выглядел рабочим, но бесполезным.
            args.push("--debug=1".into());
        }
        *state.winws_intentional_stop.lock().unwrap() = false;
        #[allow(unused_mut)]
        let mut cmd = Command::new(&winws_exe);
        cmd.args(&args)
            .current_dir(root.join("bin"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let pid = child.id();

        if let Some(stdout) = child.stdout.take() {
            let app2 = app.clone();
            std::thread::spawn(move || {
                let state = app2.state::<AppState>();
                crate::sys::for_each_line(stdout, |line| push_log_lines(&app2, &state, &line));
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let app2 = app.clone();
            std::thread::spawn(move || {
                let state = app2.state::<AppState>();
                crate::sys::for_each_line(stderr, |line| push_log_lines(&app2, &state, &line));
            });
        }

        *state.winws_child.lock().unwrap() = Some(child);
        watch_child(app, pid);
    } else {
        // Флаг «остановили намеренно» ставит kill_winws, и он остаётся
        // взведённым. Без сброса watch_by_poll принял бы падение резервного
        // конфига за нашу же остановку и промолчал.
        *state.winws_intentional_stop.lock().unwrap() = false;
        // Fallback: run the .bat itself via cmd — no live logs, but works
        // for any release shape, same tradeoff as the Electron fallback.
        #[allow(unused_mut)]
        let mut cmd = Command::new(crate::sys::system_exe("cmd.exe"));
        // `cmd /s /c ""имя""`: без /s cmd снимает кавычки, если между ними
        // есть ( или ) — а они в имени почти каждого конфига Flowseal, — и
        // «general (ALT).bat» превращался в команду «general» с аргументами.
        // С /s снимаются только внешние кавычки, внутренние остаются. Имя
        // уже проверено (checked_config): кавычек в нём нет.
        #[cfg(target_os = "windows")]
        cmd.raw_arg(format!("/s /c \"\"{file_name}\"\""));
        #[cfg(not(target_os = "windows"))]
        cmd.args(["/c", file_name]);
        cmd.current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.spawn().map_err(|e| e.to_string())?;
        watch_by_poll(app);
    }

    LAST_RUN_HAD_LOGS.store(live_logs, std::sync::atomic::Ordering::Relaxed);
    Ok(live_logs)
}

/// Обход упал сам. Чистим состояние и говорим об этом всем, кто слушает.
fn report_crash(app: &AppHandle) {
    let state = app.state::<AppState>();
    let crashed = state.persisted.lock().unwrap().active_config.clone();
    let Some(crashed) = crashed else { return };
    {
        let mut p = state.persisted.lock().unwrap();
        p.active_config = None;
        p.installed_as_service = false;
        p.started_at = None;
    }
    crate::state::save_state(app, &state);
    crate::notify::send_critical_from(
        app,
        "Обход упал",
        &format!("{} неожиданно остановилась.", crashed.trim_end_matches(".bat")),
    );
    let _ = app.emit("winws-crashed", crashed);
    crate::tray::refresh(app);
}

/// Следит за КОНКРЕТНЫМ процессом. Раньше поток смотрел просто на ячейку
/// `winws_child`: если он просыпался уже после того, как её занял следующий
/// запуск, то оставался жить и стерёг чужого ребёнка — по одному лишнему
/// потоку на каждое переключение самолечения.
fn watch_child(app: &AppHandle, pid: u32) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let mut guard = state.winws_child.lock().unwrap();
            let Some(child) = guard.as_mut() else { break };
            if child.id() != pid {
                break;
            }
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Пожинаем процесс и освобождаем ячейку.
                    *guard = None;
                    drop(guard);
                    let intentional = {
                        let mut f = state.winws_intentional_stop.lock().unwrap();
                        let was = *f;
                        *f = false;
                        was
                    };
                    if !intentional {
                        report_crash(&app);
                    }
                    break;
                }
                Ok(None) => continue,
                // Состояние процесса прочитать не вышло — держать в ячейке
                // handle, про который мы больше ничего не знаем, хуже, чем
                // честно её освободить.
                Err(_) => {
                    *guard = None;
                    break;
                }
            }
        }
    });
}

/// Резервный режим: winws поднимает сам .bat, своего `Child` у нас нет, и
/// падение обхода тут не замечал вообще никто — ни уведомления, ни события
/// `winws-crashed`. Следим опросом.
fn watch_by_poll(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        // Даём процессу подняться, иначе «ещё не стартовал» примем за падение.
        std::thread::sleep(std::time::Duration::from_secs(3));
        let state = app.state::<AppState>();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            // Появился свой Child — значит запустили заново обычным путём,
            // и за ним следит watch_child.
            if state.winws_child.lock().unwrap().is_some() {
                break;
            }
            if is_winws_running() {
                continue;
            }
            let intentional = {
                let mut f = state.winws_intentional_stop.lock().unwrap();
                let was = *f;
                *f = false;
                was
            };
            if !intentional {
                report_crash(&app);
            }
            break;
        }
    });
}

pub fn kill_winws(app: &AppHandle) {
    let state = app.state::<AppState>();
    *state.winws_intentional_stop.lock().unwrap() = true;
    {
        let mut child_guard = state.winws_child.lock().unwrap();
        if let Some(mut child) = child_guard.take() {
            let _ = child.kill();
            // Ждём выхода: иначе процесс остаётся незажатым, а наблюдатель
            // уже ушёл по ветке «ячейка пуста».
            let _ = child.wait();
        }
    }
    stop_winws();
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn временный_релиз(bat: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("klutz-winws-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("lists")).unwrap();
        fs::write(dir.join("general.bat"), bat).unwrap();
        dir
    }

    #[test]
    fn экранирование_cmd_снимается_как_у_cmd() {
        assert_eq!(unescape_token("--dpi-desync-fake-tls=^!"), "--dpi-desync-fake-tls=!");
        assert_eq!(unescape_token("--hostlist=\"%LISTS%list.txt\""), "--hostlist=%LISTS%list.txt");
        // Внутри кавычек «^» — обычный символ, cmd его не трогает.
        assert_eq!(unescape_token("\"%BIN%a^b.bin\""), "%BIN%a^b.bin");
        assert_eq!(unescape_token("^^"), "^");
        assert_eq!(unescape_token("^"), "");
    }

    /// Все конфиги настоящего релиза: ни в одном аргументе не остаётся «^».
    /// Релиз Flowseal в репозиторий не кладём, поэтому тест запускается
    /// вручную: `KLUTZ_RELEASE=<папка релиза> cargo test -- --ignored все_конфиги`.
    #[test]
    #[ignore]
    fn все_конфиги_релиза_без_экранирования() {
        let root = std::path::PathBuf::from(std::env::var("KLUTZ_RELEASE").expect("KLUTZ_RELEASE"));
        let mut checked = 0;
        for entry in fs::read_dir(&root).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().starts_with("general") || !name.to_lowercase().ends_with(".bat") {
                continue;
            }
            let args = extract_winws_args(&root, &name).unwrap_or_else(|| panic!("{name}: не разобрался"));
            assert!(args.iter().all(|a| !a.contains('^')), "{name}: {args:?}");
            // Неподставленная переменная cmd ушла бы в winws буквально.
            assert!(args.iter().all(|a| !a.contains('%')), "{name}: {args:?}");
            // «^!» стоит не во всех FAKE TLS AUTO — сверяем по самому файлу.
            if fs::read_to_string(entry.path()).unwrap_or_default().contains("fake-tls=^!") {
                assert!(args.contains(&"--dpi-desync-fake-tls=!".to_string()), "{name}");
            }
            checked += 1;
        }
        println!("проверено конфигов: {checked}");
        assert!(checked > 0);
    }

    /// Живой случай: FAKE TLS AUTO из 1.10.2 не запускался из Klutz.
    #[test]
    fn fake_tls_auto_получает_встроенную_подложку() {
        let bat = [
            "@echo off",
            "set \"BIN=%~dp0bin\\\"",
            "start \"zapret: %~n0\" /min \"%BIN%winws.exe\" --wf-tcp=80,443 ^",
            "--filter-tcp=443 --dpi-desync=fake,multidisorder --dpi-desync-fake-tls=0x00000000 --dpi-desync-fake-tls=^! --dpi-desync-fake-tls-mod=rnd,dupsid,sni=www.google.com",
        ]
        .join("\r\n");
        let dir = временный_релиз(&bat);
        let args = extract_winws_args(&dir, "general.bat").unwrap();
        assert!(args.contains(&"--dpi-desync-fake-tls=!".to_string()), "{args:?}");
        assert!(args.iter().all(|a| !a.contains('^')), "{args:?}");
        assert!(args.contains(&"--dpi-desync-fake-tls-mod=rnd,dupsid,sni=www.google.com".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Собирает .bat по образцу настоящего релиза: многопрофильный запуск
    /// через `--new`, склейка строк по «^», пути в кавычках с %BIN%/%LISTS%,
    /// %GameFilterTCP% ВНУТРИ списка портов, а не отдельным словом.
    ///
    /// Форма сверена с zapret-discord-youtube 1.10.2: все 22 шипованных
    /// конфига разбираются этим же кодом, по 9 профилей каждый, без единой
    /// неподставленной переменной. Сами файлы Flowseal сюда не кладём —
    /// у релиза нет лицензии, разрешающей его перераспространять.
    fn многопрофильный_bat() -> String {
        [
            "@echo off",
            "chcp 65001 > nul",
            "cd /d \"%~dp0\"",
            "set \"BIN=%~dp0bin\\\"",
            "set \"LISTS=%~dp0lists\\\"",
            "start \"zapret: %~n0\" /min \"%BIN%winws.exe\" --wf-tcp=80,443,%GameFilterTCP% --wf-udp=443,%GameFilterUDP% ^",
            "--filter-udp=443 --hostlist=\"%LISTS%list-general.txt\" --dpi-desync=fake --dpi-desync-repeats=6 --new ^",
            "--filter-tcp=443 --dpi-desync=fake,multisplit --dpi-desync-split-pos=1,midsld --dpi-desync-fooling=ts ^",
            " --dpi-desync-fake-tls=\"%BIN%tls_clienthello_www_google_com.bin\"",
        ]
        .join("\r\n")
            + "\r\n"
    }

    #[test]
    fn разбирает_многопрофильный_конфиг_как_в_релизе() {
        let dir = временный_релиз(&многопрофильный_bat());
        let args = extract_winws_args(&dir, "general.bat").expect("должно разобраться");

        // Один --new = два профиля. В настоящих конфигах их девять, но
        // разделитель разбирается одинаково независимо от количества.
        assert_eq!(args.iter().filter(|a| *a == "--new").count(), 1, "профили: {args:?}");
        assert!(args.iter().any(|a| a == "--dpi-desync-split-pos=1,midsld"), "{args:?}");
        assert!(
            !args.iter().any(|a| a.contains('%')),
            "переменные обязаны быть подставлены: {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("--dpi-desync-fake-tls=") && a.ends_with(".bin")),
            "путь к fake-payload должен склеиться: {args:?}"
        );
        // Порт-лист с переменной внутри — самое хрупкое место подстановки.
        assert!(args.iter().any(|a| a.starts_with("--wf-tcp=80,443,")), "{args:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn разбирает_аргументы_и_подставляет_переменные() {
        let bat = "@echo off\r\nset BIN=%~dp0bin\\\r\nstart \"zapret\" /min \"%BIN%winws.exe\" --wf-tcp=80,443 ^\r\n --hostlist=\"%LISTS%list-general.txt\" ^\r\n --filter-udp=%GameFilterUDP%\r\n";
        let dir = временный_релиз(bat);
        let args = extract_winws_args(&dir, "general.bat").expect("должно разобраться");

        assert!(args.iter().any(|a| a == "--wf-tcp=80,443"), "{args:?}");
        assert!(args.iter().any(|a| a.starts_with("--hostlist=") && a.contains("list-general.txt")), "{args:?}");
        assert!(!args.iter().any(|a| a.contains("%LISTS%")), "переменные должны быть подставлены: {args:?}");
        assert!(!args.iter().any(|a| a.contains("%GameFilter")), "{args:?}");
        assert!(!args.iter().any(|a| a == "^"), "склейка строк не должна оставлять ^: {args:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn без_winws_в_строке_возвращает_none() {
        let dir = временный_релиз("@echo off\r\necho ничего интересного\r\n");
        assert!(extract_winws_args(&dir, "general.bat").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn без_ключей_возвращает_none() {
        let dir = временный_релиз("start \"z\" \"%BIN%winws.exe\" простотекст\r\n");
        assert!(extract_winws_args(&dir, "general.bat").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn несуществующий_файл_не_паникует() {
        let dir = временный_релиз("@echo off");
        assert!(extract_winws_args(&dir, "нет-такого.bat").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn game_filter_читается_из_маркера() {
        let dir = временный_релиз("@echo off");
        fs::create_dir_all(dir.join("utils")).unwrap();

        // Маркера нет — фильтр выключен, порт 12.
        assert_eq!(game_filter_values(&dir).game_filter, "12");

        fs::write(dir.join("utils").join("game_filter.enabled"), "tcp").unwrap();
        let gf = game_filter_values(&dir);
        assert_eq!((gf.game_filter_tcp, gf.game_filter_udp), ("1024-65535", "12"));

        fs::write(dir.join("utils").join("game_filter.enabled"), "udp").unwrap();
        let gf = game_filter_values(&dir);
        assert_eq!((gf.game_filter_tcp, gf.game_filter_udp), ("12", "1024-65535"));
        let _ = fs::remove_dir_all(&dir);
    }
}
