//! Тонкие обёртки над системными утилитами Windows (sc/net/tasklist/reg/schtasks).
//! Абсолютных путей намеренно не берём — но и окон не показываем.

use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Системный ГСЧ. `RandomState` для этого не годился: std засевает ключи
/// SipHash из ОС ОДИН раз на поток, а дальше просто инкрементирует счётчик —
/// два блока подряд получали связанные ключи, и вся энтропия сводилась к
/// одному посеву плюс метке времени, а не к 128 битам, как выглядело.
#[cfg(target_os = "windows")]
pub fn os_random(buf: &mut [u8]) -> bool {
    use windows_sys::Win32::Security::Cryptography::ProcessPrng;
    unsafe { ProcessPrng(buf.as_mut_ptr(), buf.len()) != 0 }
}

#[cfg(not(target_os = "windows"))]
pub fn os_random(buf: &mut [u8]) -> bool {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .is_ok()
}

/// Абсолютный путь к системной утилите.
///
/// `Command::new("cmd.exe")` ищет файл в том числе в ТЕКУЩЕМ каталоге, а
/// часть команд мы запускаем с `current_dir` в папке релиза — то есть в
/// каталоге, который пользователь мог распаковать из чужого архива.
/// Подложенный туда `cmd.exe` исполнился бы с правами администратора.
/// Каталог Windows. Спрашиваем у самой системы, а не у переменной
/// окружения: `SystemRoot` наследуется от того, кто нас запустил, и её
/// значение — просто строка в нашем же процессе. Подставив туда свой
/// каталог, можно было подсунуть свой `sc.exe` или `reg.exe`, а запускаем
/// мы их с правами администратора. Ровно ту дыру, ради которой появился
/// `system_exe`, переменная и оставляла открытой.
#[cfg(target_os = "windows")]
fn windows_dir() -> std::path::PathBuf {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buf = [0u16; 260];
    // Возвращает System32; нам нужен каталог уровнем выше.
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return std::path::PathBuf::from("C:\\Windows");
    }
    let sys32 = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    sys32.parent().map(|p| p.to_path_buf()).unwrap_or(sys32)
}

#[cfg(not(target_os = "windows"))]
fn windows_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("C:\\Windows")
}

/// Системный файл hosts — от каталога Windows, а не зашитым `C:\Windows`.
pub fn hosts_path() -> std::path::PathBuf {
    windows_dir().join("System32").join("drivers").join("etc").join("hosts")
}

pub fn system_exe(name: &str) -> std::path::PathBuf {
    let root = windows_dir();
    let root = root.as_path();
    // powershell лежит не в корне System32, explorer — не в System32 вовсе.
    for candidate in [
        root.join("System32").join(name),
        root.join("System32").join("WindowsPowerShell").join("v1.0").join(name),
        root.join(name),
    ] {
        if candidate.exists() {
            return candidate;
        }
    }
    // Ничего не нашли — всё равно отдаём АБСОЛЮТНЫЙ путь. Вернуть голое имя
    // значило бы вернуть поиск по текущему каталогу и PATH, то есть ровно ту
    // дыру, ради которой эта функция и появилась. Пусть лучше запуск честно
    // провалится.
    root.join("System32").join(name)
}

/// Декодирует вывод консольной утилиты. Сначала UTF-8, а если не вышло —
/// кодовая страница OEM: на русской Windows sc, net и netsh пишут в CP866, и
/// `from_utf8_lossy` превращал их сообщения в ромбики — включая текст ошибки,
/// который потом показывали пользователю.
pub fn decode_console(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    #[cfg(target_os = "windows")]
    if let Some(s) = decode_oem(bytes) {
        return s;
    }
    String::from_utf8_lossy(bytes).to_string()
}

#[cfg(target_os = "windows")]
fn decode_oem(bytes: &[u8]) -> Option<String> {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    const CP_OEMCP: u32 = 1;
    if bytes.is_empty() {
        return Some(String::new());
    }
    unsafe {
        let need = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0);
        if need <= 0 {
            return None;
        }
        let mut buf = vec![0u16; need as usize];
        let got = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), bytes.len() as i32, buf.as_mut_ptr(), need);
        if got <= 0 {
            return None;
        }
        String::from_utf16(&buf[..got as usize]).ok()
    }
}

/// Читает поток построчно и отдаёт уже декодированные строки.
///
/// `lines()` здесь не годится: он строгий UTF-8, а `.flatten()` МОЛЧА
/// выбрасывает каждую строку, которую не удалось разобрать, — на русской
/// Windows это все строки с кириллицей, и они просто исчезали из живого лога.
pub fn for_each_line<R: std::io::Read>(r: R, mut f: impl FnMut(String)) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = decode_console(&buf);
        f(line.trim_end_matches(['\r', '\n']).to_string());
    }
}

/// Запускает команду, отдаёт stdout. Ошибку не считаем фатальной — многие
/// из этих утилит возвращают ненулевой код на «ничего не найдено».
pub fn run(program: &str, args: &[&str]) -> String {
    #[allow(unused_mut)]
    let mut cmd = Command::new(system_exe(program));
    cmd.args(args);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    match cmd.output() {
        Ok(out) => {
            let mut s = decode_console(&out.stdout);
            s.push_str(&decode_console(&out.stderr));
            s
        }
        Err(_) => String::new(),
    }
}

pub fn run_ok(program: &str, args: &[&str]) -> bool {
    #[allow(unused_mut)]
    let mut cmd = Command::new(system_exe(program));
    cmd.args(args);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

pub struct SvcState {
    pub exists: bool,
    pub state: Option<String>,
}

/// sc.exe локализует названия полей (STATE/TYPE), но само значение состояния
/// остаётся английским — поэтому ищем именно значение, а не подпись.
pub fn svc_query(name: &str) -> SvcState {
    let out = run("sc", &["query", name]);
    for token in [
        "RUNNING",
        "STOP_PENDING",
        "START_PENDING",
        "CONTINUE_PENDING",
        "PAUSE_PENDING",
        "PAUSED",
        "STOPPED",
    ] {
        if out.contains(token) {
            return SvcState { exists: true, state: Some(token.to_string()) };
        }
    }
    SvcState { exists: false, state: None }
}

/// Запущен ли процесс с таким именем образа.
///
/// Снимком процессов Windows, а не запуском tasklist.exe: этот вопрос задают
/// самолечение, трей и почти каждая кнопка («работает ли winws»), и на каждый
/// раз уходил отдельный процесс с разбором его вывода. tasklist остался
/// запасным путём — на случай, если снимок не создался.
#[cfg(target_os = "windows")]
pub fn proc_running(image: &str) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return proc_running_tasklist(image);
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case(image) {
                found = true;
                break;
            }
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(not(target_os = "windows"))]
pub fn proc_running(image: &str) -> bool {
    proc_running_tasklist(image)
}

#[allow(dead_code)]
fn proc_running_tasklist(image: &str) -> bool {
    run("tasklist", &["/FI", &format!("IMAGENAME eq {image}")])
        .to_lowercase()
        .contains(&image.to_lowercase())
}

/// SHA-256 файла в шестнадцатеричном виде — средствами Windows (BCrypt),
/// без сторонних библиотек.
#[cfg(target_os = "windows")]
pub fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    use windows_sys::Win32::Security::Cryptography::{
        BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData,
        BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALGORITHM,
    };
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    unsafe {
        let mut alg: BCRYPT_ALG_HANDLE = std::ptr::null_mut();
        if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), 0) < 0 {
            return Err("SHA-256 в Windows недоступен".into());
        }
        let mut hash: BCRYPT_HASH_HANDLE = std::ptr::null_mut();
        if BCryptCreateHash(alg, &mut hash, std::ptr::null_mut(), 0, std::ptr::null(), 0, 0) < 0 {
            BCryptCloseAlgorithmProvider(alg, 0);
            return Err("не удалось начать подсчёт SHA-256".into());
        }
        let mut result: Result<(), String> = Ok(());
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if BCryptHashData(hash, buf.as_ptr(), n as u32, 0) < 0 {
                        result = Err("сбой при подсчёте SHA-256".into());
                        break;
                    }
                }
                Err(e) => {
                    result = Err(e.to_string());
                    break;
                }
            }
        }
        let mut out = [0u8; 32];
        if result.is_ok() && BCryptFinishHash(hash, out.as_mut_ptr(), out.len() as u32, 0) < 0 {
            result = Err("сбой при подсчёте SHA-256".into());
        }
        BCryptDestroyHash(hash);
        BCryptCloseAlgorithmProvider(alg, 0);
        result?;
        Ok(out.iter().map(|b| format!("{b:02x}")).collect())
    }
}

#[cfg(not(target_os = "windows"))]
pub fn sha256_file(_path: &std::path::Path) -> Result<String, String> {
    Err("SHA-256 считается только в Windows".into())
}

#[cfg(test)]
mod proc_and_hash_tests {
    use super::*;

    #[test]
    fn свой_процесс_виден_а_чужого_нет() {
        let me = std::env::current_exe().unwrap();
        let name = me.file_name().unwrap().to_string_lossy().to_string();
        assert!(proc_running(&name), "{name}");
        assert!(proc_running(&name.to_uppercase()), "имя образа сравнивается без регистра");
        assert!(!proc_running("klutz-такого-процесса-нет.exe"));
    }

    #[test]
    fn sha256_как_у_всех() {
        let p = std::env::temp_dir().join(format!("klutz-sha-{}.txt", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&p);
    }
}

/// Какая стратегия прописана у установленной службы zapret.
pub fn installed_service_strategy() -> Option<String> {
    let out = run(
        "reg",
        &[
            "query",
            r"HKLM\System\CurrentControlSet\Services\zapret",
            "/v",
            "zapret-discord-youtube",
        ],
    );
    let idx = out.find("REG_SZ")?;
    let tail = out[idx + "REG_SZ".len()..].trim_start();
    let line = tail.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn utf8_проходит_как_есть() {
        assert_eq!(decode_console("служба запущена".as_bytes()), "служба запущена");
        assert_eq!(decode_console(b""), "");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn однобайтовый_вывод_не_превращается_в_ромбики() {
        // Кодовая страница OEM у каждой машины своя (866 на русской, 437 на
        // английской), поэтому проверяем не конкретные буквы, а само
        // свойство: байты, не являющиеся UTF-8, декодируются в осмысленный
        // текст без U+FFFD — именно их from_utf8_lossy превращал в ромбики.
        let байты = [0x8E_u8, 0xE8, 0xA8, 0xA1, 0xAA, 0xA0];
        #[allow(invalid_from_utf8)]
        {
            assert!(std::str::from_utf8(&байты).is_err(), "проверяем именно не-UTF-8");
        }

        let lossy = String::from_utf8_lossy(&байты);
        assert!(lossy.contains('\u{FFFD}'), "старое поведение — ромбики");

        let s = decode_console(&байты);
        assert!(!s.contains('\u{FFFD}'), "новое — без потерь, получили {s:?}");
        assert_eq!(s.chars().count(), байты.len(), "однобайтовая кодировка: символ на байт");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn системные_утилиты_разрешаются_в_абсолютный_путь() {
        for exe in ["cmd.exe", "tasklist.exe", "sc.exe", "powershell.exe"] {
            let p = system_exe(exe);
            assert!(p.is_absolute(), "{exe}: {p:?}");
            assert!(p.exists(), "{exe}: {p:?} не существует");
        }
    }

    #[test]
    fn построчное_чтение_не_теряет_кириллицу() {
        let data = "первая\nвторая\r\nтретья".as_bytes().to_vec();
        let mut got = Vec::new();
        for_each_line(std::io::Cursor::new(data), |l| got.push(l));
        assert_eq!(got, vec!["первая", "вторая", "третья"]);
    }
}
