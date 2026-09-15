//! Не идёт ли сеть мимо провайдера: через VPN, туннель или прокси.
//!
//! Зачем. Разведка «как у меня режут», подбор стратегии и тесты меряют сеть
//! провайдера. Если трафик уходит в туннель, меряется сеть VPN: там ничего
//! не режут, всё зелёное, и вывод получается про чужую сеть. Системный
//! прокси Klutz видел и раньше, а VPN в режиме туннеля (TUN у Hiddify,
//! WireGuard, OpenVPN, WARP) прокси не ставит — его видно только по сетевым
//! адаптерам.
//!
//! Как узнаём. Спрашиваем Windows, через какой адаптер уйдёт пакет на адрес
//! discord.com (GetBestInterface), и смотрим, что это за адаптер. Просто
//! «есть туннельный адаптер» — не повод: Tailscale, Hamachi и Radmin держат
//! свой адаптер постоянно, а интернет идёт мимо него.

use serde::Serialize;

use crate::discorddiag::Proxy;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Adapter {
    pub index: u32,
    pub name: String,
    pub description: String,
    #[serde(rename = "ifType")]
    pub if_type: u32,
    pub up: bool,
}

/// Виртуальный адаптер и туннель по классификации IANA. Wintun (на нём
/// WireGuard, Hiddify, sing-box) регистрируется как виртуальный.
///
/// PPP (23) сюда намеренно не входит: так выглядит и встроенный VPN Windows,
/// и обычное «Высокоскоростное подключение» PPPoE, через которое многие
/// провайдеры и дают интернет. По типу их не различить, а назвать
/// провайдера VPN-ом — сломать разведку половине пользователей.
const IF_TYPE_PROP_VIRTUAL: u32 = 53;
const IF_TYPE_TUNNEL: u32 = 131;

/// Слова в описании или имени адаптера, по которым узнаётся программа.
/// Порядок важен: сначала конкретные программы, потом общий Wintun, на
/// котором они построены.
const KNOWN: &[(&str, &str)] = &[
    ("hiddify", "Hiddify"),
    ("amnezia", "AmneziaVPN"),
    ("nekoray", "NekoRay"),
    ("sing-tun", "sing-box"),
    ("singbox", "sing-box"),
    ("clash", "Clash"),
    ("v2ray", "v2ray"),
    ("xray", "Xray"),
    ("tun2socks", "tun2socks"),
    ("outline", "Outline"),
    ("warp", "Cloudflare WARP"),
    ("cloudflare", "Cloudflare WARP"),
    ("proton", "Proton VPN"),
    ("nordlynx", "NordVPN"),
    ("nordvpn", "NordVPN"),
    ("expressvpn", "ExpressVPN"),
    ("surfshark", "Surfshark"),
    ("windscribe", "Windscribe"),
    ("mullvad", "Mullvad"),
    ("psiphon", "Psiphon"),
    ("adguard vpn", "AdGuard VPN"),
    ("kaspersky", "Kaspersky VPN"),
    ("forticlient", "FortiClient"),
    ("anyconnect", "Cisco AnyConnect"),
    ("zerotier", "ZeroTier"),
    ("tailscale", "Tailscale"),
    ("hamachi", "Hamachi"),
    ("radmin", "Radmin VPN"),
    ("wireguard", "WireGuard"),
    ("openvpn", "OpenVPN"),
    ("tap-windows", "OpenVPN (TAP)"),
    ("tap-win32", "OpenVPN (TAP)"),
    ("wintun", "туннель Wintun (WireGuard, Hiddify, sing-box и подобные)"),
];

/// Программа по подписи адаптера — только если узнали наверняка.
fn known_label(a: &Adapter) -> Option<&'static str> {
    let hay = format!("{} {}", a.description, a.name).to_lowercase();
    KNOWN.iter().find(|(k, _)| hay.contains(k)).map(|(_, label)| *label)
}

/// Похож ли адаптер на туннель: узнанная программа или туннельный тип.
pub fn tunnel_label(a: &Adapter) -> Option<String> {
    if let Some(label) = known_label(a) {
        return Some(label.to_string());
    }
    if a.if_type == IF_TYPE_PROP_VIRTUAL || a.if_type == IF_TYPE_TUNNEL {
        let what = if a.description.is_empty() { &a.name } else { &a.description };
        return Some(format!("туннельный адаптер «{what}»"));
    }
    None
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VpnCheck {
    /// Замерам верить нельзя — разведку и подбор не запускаем.
    pub blocked: bool,
    /// Что именно мешает, словами.
    pub reasons: Vec<String>,
    /// Что нашли, но замерам не мешает, и что проверить не вышло.
    pub notes: Vec<String>,
}

/// Чистое правило — системные данные собирает `check`.
pub fn decide(
    proxy: Option<&Proxy>,
    route: Option<&Adapter>,
    adapters: &[Adapter],
    dns: Option<(&str, &str)>,
) -> VpnCheck {
    let mut reasons = Vec::new();
    let mut notes = Vec::new();

    if let Some(p) = proxy {
        let who = p.owner.clone().unwrap_or_else(|| p.server.clone());
        reasons.push(format!("включён системный прокси {who} — Discord и браузеры идут через него"));
    }
    match route {
        Some(a) => {
            if let Some(label) = tunnel_label(a) {
                reasons.push(format!("интернет идёт через {label}"));
            }
        }
        None => notes.push("не удалось узнать, через какой адаптер идёт интернет, — туннель не проверен".into()),
    }
    // Остальные адаптеры — только узнанные программы. Общий «туннельный тип»
    // здесь был бы шумом: Teredo и IP-HTTPS есть почти в каждой Windows.
    for a in adapters.iter().filter(|a| a.up && Some(a.index) != route.map(|r| r.index)) {
        if let Some(label) = known_label(a) {
            let note = format!("есть {label}, но интернет идёт не через него — замерам не мешает");
            if !notes.contains(&note) {
                notes.push(note);
            }
        }
    }
    if let Some((ip, hint)) = dns {
        reasons.push(format!("discord.com разрешается в {ip}: {hint}"));
    }

    VpnCheck { blocked: !reasons.is_empty(), reasons, notes }
}

/// Одна фраза для окна: что мешает и что сделать.
pub fn message(v: &VpnCheck) -> String {
    format!(
        "Похоже, сеть идёт не напрямую: {}. Выключи VPN или прокси и повтори — иначе проверяется их сеть, а не твоя.",
        v.reasons.join("; ")
    )
}

pub fn check() -> VpnCheck {
    let proxy = crate::discorddiag::system_proxy();
    let adapters = adapters();
    let dns_ip = crate::probe::resolve_ips("discord.com", 443).into_iter().next();
    // Маршрут спрашиваем до настоящего адреса цели: раздельный туннель может
    // пускать мимо себя всё, кроме заблокированного. Имя не разрешилось —
    // берём любой внешний адрес.
    let dest = dns_ip
        .as_deref()
        .and_then(|ip| ip.parse::<std::net::Ipv4Addr>().ok())
        .unwrap_or(std::net::Ipv4Addr::new(1, 1, 1, 1));
    let route = best_interface(dest).and_then(|i| adapters.iter().find(|a| a.index == i));
    let dns = dns_ip.as_deref().and_then(|ip| crate::probe::tunnel_hint(ip).map(|h| (ip, h)));
    decide(proxy.as_ref(), route, &adapters, dns)
}

#[cfg(windows)]
fn best_interface(dest: std::net::Ipv4Addr) -> Option<u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::GetBestInterface;
    // Адрес в сетевом порядке байт — ровно как октеты лежат в памяти.
    let addr = u32::from_ne_bytes(dest.octets());
    let mut index = 0u32;
    let rc = unsafe { GetBestInterface(addr, &mut index) };
    (rc == 0).then_some(index)
}

#[cfg(not(windows))]
fn best_interface(_dest: std::net::Ipv4Addr) -> Option<u32> {
    None
}

#[cfg(windows)]
fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

#[cfg(windows)]
fn adapters() -> Vec<Adapter> {
    use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GAA_FLAG_SKIP_UNICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

    const AF_UNSPEC: u32 = 0;
    let flags = GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 32 * 1024;
    // Адаптер мог появиться между вызовами — тогда размер вырастет ещё раз.
    for _ in 0..4 {
        // Буфер из u64: в списке структуры с восьмибайтным выравниванием.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let head = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let rc = unsafe { GetAdaptersAddresses(AF_UNSPEC, flags, std::ptr::null(), head, &mut size) };
        if rc == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if rc != 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut cur = head;
        while !cur.is_null() {
            let a = unsafe { &*cur };
            out.push(Adapter {
                index: unsafe { a.Anonymous1.Anonymous.IfIndex },
                name: wide(a.FriendlyName),
                description: wide(a.Description),
                if_type: a.IfType,
                up: a.OperStatus == IfOperStatusUp,
            });
            cur = a.Next;
        }
        return out;
    }
    Vec::new()
}

#[cfg(not(windows))]
fn adapters() -> Vec<Adapter> {
    Vec::new()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn ad(index: u32, name: &str, description: &str, if_type: u32) -> Adapter {
        Adapter { index, name: name.into(), description: description.into(), if_type, up: true }
    }

    #[test]
    fn туннели_узнаются_а_обычные_сети_нет() {
        assert_eq!(tunnel_label(&ad(1, "wg0", "WireGuard Tunnel", 53)).as_deref(), Some("WireGuard"));
        assert_eq!(tunnel_label(&ad(1, "HiddifyTunnel", "Wintun Userspace Tunnel", 53)).as_deref(), Some("Hiddify"));
        assert_eq!(tunnel_label(&ad(1, "Ethernet 3", "TAP-Windows Adapter V9", 6)).as_deref(), Some("OpenVPN (TAP)"));
        assert_eq!(
            tunnel_label(&ad(1, "CloudflareWARP", "Cloudflare WARP Interface Tunnel", 53)).as_deref(),
            Some("Cloudflare WARP")
        );
        // Неизвестная программа, но тип туннельный.
        assert!(tunnel_label(&ad(1, "tun0", "Some Tunnel", 53)).unwrap().contains("Some Tunnel"));

        assert_eq!(tunnel_label(&ad(1, "Ethernet", "Intel(R) Ethernet Connection I219-V", 6)), None);
        assert_eq!(tunnel_label(&ad(1, "Wi-Fi", "Intel(R) Wi-Fi 6 AX201 160MHz", 71)), None);
        assert_eq!(tunnel_label(&ad(1, "vEthernet (Default Switch)", "Hyper-V Virtual Ethernet Adapter", 6)), None);
        // PPPoE провайдера — не VPN.
        assert_eq!(tunnel_label(&ad(1, "Высокоскоростное подключение", "Высокоскоростное подключение", 23)), None);
    }

    #[test]
    fn мешает_только_то_через_что_идёт_интернет() {
        let wifi = ad(10, "Wi-Fi", "Intel(R) Wi-Fi 6 AX201", 71);
        let tailscale = ad(20, "Tailscale", "Tailscale Tunnel", 53);
        let teredo = ad(30, "Teredo", "Microsoft Teredo Tunneling Adapter", 131);
        let all = vec![wifi.clone(), tailscale.clone(), teredo];

        let v = decide(None, Some(&wifi), &all, None);
        assert!(!v.blocked, "{v:?}");
        assert_eq!(v.notes, vec!["есть Tailscale, но интернет идёт не через него — замерам не мешает"]);

        let v = decide(None, Some(&tailscale), &all, None);
        assert!(v.blocked);
        assert_eq!(v.reasons, vec!["интернет идёт через Tailscale"]);
    }

    #[test]
    fn прокси_и_подмена_адреса_тоже_мешают() {
        let wifi = ad(10, "Wi-Fi", "Intel(R) Wi-Fi 6 AX201", 71);
        let proxy = Proxy { server: "127.0.0.1:12334".into(), owner: Some("Hiddify.exe".into()) };
        let v = decide(Some(&proxy), Some(&wifi), std::slice::from_ref(&wifi), None);
        assert!(v.blocked);
        assert!(v.reasons[0].contains("Hiddify.exe"), "{:?}", v.reasons);

        let v = decide(None, Some(&wifi), std::slice::from_ref(&wifi), Some(("198.18.0.5", "fake-IP")));
        assert!(v.blocked);
        assert!(v.reasons[0].contains("198.18.0.5"));
        assert!(message(&v).contains("Выключи VPN"));
    }

    #[test]
    fn маршрут_не_узнали_это_не_блок() {
        let v = decide(None, None, &[], None);
        assert!(!v.blocked);
        assert_eq!(v.notes.len(), 1);
    }
}
