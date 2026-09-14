// Стаб window.zapret для проверки интерфейса без Tauri.
// НЕ используется в продакшене — подключается только preview-server.js по /mock.
//
// ВАЖНО: формы ответов должны совпадать с тем, что реально отдают команды
// Rust (src-tauri/src/commands.rs и структуры с serde rename). Стенд, который
// врёт про контракт, хуже, чем его отсутствие: именно так здесь незаметно
// разъехались getToggles, checkUpdates и getNotifySound.
(function () {
  const CONFIGS = [
    'general.bat', 'general (ALT).bat', 'general (ALT2).bat', 'general (ALT3).bat',
    'general (FAKE TLS AUTO).bat', 'general (FAKE TLS AUTO ALT).bat',
    'general (SIMPLE FAKE).bat', 'general (SIMPLE FAKE ALT).bat',
    'general (MGTS).bat', 'general (MGTS2).bat',
  ];

  const DEFAULT_TARGETS = [
    { name: 'Discord Main', host: 'discord.com', port: 443 },
    { name: 'Discord Gateway', host: 'gateway.discord.gg', port: 443 },
    { name: 'Discord CDN', host: 'cdn.discordapp.com', port: 443 },
    { name: 'Discord Updates', host: 'updates.discord.com', port: 443 },
    { name: 'YouTube Web', host: 'www.youtube.com', port: 443 },
    { name: 'YouTube Video', host: 'redirector.googlevideo.com', port: 443 },
    { name: 'YouTube Short', host: 'youtu.be', port: 443 },
    { name: 'Rocket League', host: 'api.rlpp.psynet.gg', port: 443 },
    { name: 'Epic Online', host: 'api.epicgames.dev', port: 443 },
    { name: 'Steam', host: 'api.steampowered.com', port: 443 },
    { name: 'Riot', host: 'auth.riotgames.com', port: 443 },
    { name: 'Battle.net', host: 'us.actual.battle.net', port: 1119 },
    { name: 'Xbox Live', host: 'title.mgt.xboxlive.com', port: 443 },
  ];

  let customTargets = [
    { name: 'PlayStation Network', host: 'auth.api.sonyentertainmentnetwork.com', port: 443 },
  ];

  function allTargets() {
    return [...DEFAULT_TARGETS, ...customTargets];
  }

  // Разные вердикты у разных упавших целей — чтобы на стенде была видна
  // каждая ветка, а не одно «нет связи» на все случаи.
  const FAIL_KINDS = [
    { verdict: 'sni', code: 'tls_failed', why: 'с нейтральным именем example.com тот же адрес отвечает — режут по имени' },
    { verdict: 'server', code: 'tls_cert', why: 'ответил сам сервер — это его политика, а не блокировка' },
    { verdict: 'ip', code: 'timeout', why: 'с нейтральным именем тот же адрес тоже молчит — режут адрес, не имя' },
    { verdict: 'legal', code: 'http_451', why: 'ответ 451 — это не DPI, и стратегия обхода такое не чинит' },
    { verdict: 'unknown', code: 'unknown', why: 'контрольный замер не выполнялся — где именно режут, неизвестно' },
    { verdict: 'cutoff', code: 'cutoff', why: 'сервер ответил, отдал 18 КБ и замолчал — режут уже установленный поток' },
  ];

  function pingResult(t, i) {
    const ok = i % 5 !== 4;
    if (ok) {
      return { name: t.name, host: t.host, port: t.port, ok, ms: 30 + (i * 7) % 120,
               pending: false, code: 'ok', verdict: 'ok' };
    }
    const k = FAIL_KINDS[(i / 5 | 0) % FAIL_KINDS.length];
    return { name: t.name, host: t.host, port: t.port, ok: false, ms: null, pending: false,
             code: k.code, verdict: k.verdict, why: k.why };
  }

  const state = {
    rootPath: 'C:\\Users\\vbu00\\Desktop\\zapret-discord-youtube-1.9.9c',
    configs: CONFIGS,
    activeConfig: 'general (ALT).bat',
    running: true,
    installedAsService: false,
    // Служба zapret, поставленная не из Klutz (см. get_state в commands.rs).
    serviceExists: false,
    canInstallService: true,
    startedAt: Date.now() - 12 * 60 * 1000,
    monitor: { checkedAt: Date.now(), targets: allTargets().map(pingResult) },
  };

  const noop = async () => ({ ok: true });

  window.zapret = {
    // Перетаскивание идёт событием Tauri, а не через DOM. Метод обязан
    // существовать: renderer подписывается на него при загрузке, и без
    // заглушки весь скрипт падал бы на TypeError.
    onFileDrop: () => () => {},
    getVersions: async () => ({ app: '1.2.3', zapret: '1.9.9c', tgws: '1.10.2' }),
    checkKlutzUpdate: async () => ({ current: '1.2.3', latest: '1.2.4', error: null, url: 'https://github.com/vbu00/zapret-klutz/releases/latest' }),
    checkComponentUpdates: async () => ({
      klutz: { current: '1.2.3', latest: '1.2.3', error: null, url: 'https://github.com/vbu00/zapret-klutz/releases/latest' },
      zapret: { current: '1.9.9c', latest: '1.9.9c', error: null },
      tgws: { current: '1.10.2', latest: '1.10.2', error: null },
    }),
    copyText: async () => ({ ok: true }),

    pickFolder: noop,
    pickArchive: noop,
    loadPath: noop,
    getState: async () => state,

    listReleases: async () => ({
      ok: true,
      releases: [
        { name: 'zapret-discord-youtube-1.9.9c', path: 'C:\\Users\\vbu00\\AppData\\Roaming\\com.vbu00.klutz\\releases\\zapret-discord-youtube-1.9.9c', current: true, extractedAt: Date.now() - 3 * 86400000 },
        { name: 'zapret-discord-youtube-1.9.8', path: 'C:\\Users\\vbu00\\AppData\\Roaming\\com.vbu00.klutz\\releases\\zapret-discord-youtube-1.9.8', current: false, extractedAt: Date.now() - 40 * 86400000 },
      ],
    }),
    deleteRelease: noop,

    getLatestReleaseInfo: async () => ({
      ok: true,
      error: null,
      version: '1.9.9c',
      name: 'zapret-discord-youtube-1.9.9c.zip',
      size: 12_400_000,
      url: 'https://example.invalid/zapret.zip',
      notesUrl: 'https://github.com/Flowseal/zapret-discord-youtube/releases/tag/1.9.9c',
    }),
    downloadLatestRelease: noop,
    onDownloadProgress: () => () => {},

    getOnboardingDone: async () => true,
    setOnboardingDone: noop,

    runConfig: noop,
    stopConfig: noop,
    getWinwsLog: async () => ({ live: true, lines: ['[winws] запущен', '[winws] general (ALT).bat активен'] }),
    onWinwsLog: () => () => {},
    onWinwsCrashed: () => () => {},
    installService: noop,
    removeService: noop,
    getServiceStatus: async () => ({ serviceExists: false, serviceState: 'STOPPED', windivertState: 'RUNNING', winwsRunning: true, strategy: state.activeConfig }),

    runTests: async () => ({
      ok: true,
      text:
        '=== ANALYTICS ===\n' +
        CONFIGS.map((c, i) => `${c}: HTTP OK: ${7 - (i % 5)}, ERR: ${i % 5}, UNSUP: 0, Ping OK: ${7 - (i % 4)}, Fail: ${i % 4}`).join('\n'),
    }),
    stopTests: noop,
    getLastTestResults: async () => ({
      ok: true,
      text:
        '=== ANALYTICS ===\n' +
        CONFIGS.map((c, i) => `${c}: HTTP OK: ${7 - (i % 5)}, ERR: ${i % 5}, UNSUP: 0, Ping OK: ${7 - (i % 4)}, Fail: ${i % 4}`).join('\n'),
    }),
    onTestLog: () => () => {},

    getToggles: async () => ({ gameMode: 'off', ipsetMode: 'loaded', autoUpdate: false }),
    setGameFilter: noop,
    cycleIpsetMode: noop,
    setAutoUpdate: noop,

    checkBypassChance: async () => ({
      verdict: 'helps',
      note: 'разрез ClientHello пробивает — стратегии zapret здесь применимы, подбор имеет смысл',
      targets: [{ name: 'Discord Main', host: 'discord.com', verdict: 'helps', note: 'целый режут, разрезанный проходит' }],
      tls13: 'TLS 1.3 не проходит, а откат на 1.2 проходит — режут именно ClientHello 1.3',
      response: { verdict: 'blocked', reason: 'запрос проходит, а рукопожатие не завершается ни разу — режут ОТВЕТ', target: 0, control: 2, repeats: 2 },
    }),
    getGameScan: async () => ({
      saved: 12,
      addrs: [
        '103.10.124.0/23', '146.66.152.0/22', '146.66.155.0/24', '155.133.224.0/22',
        '155.133.226.0/24', '155.133.230.0/24', '155.133.232.0/24', '162.254.192.0/21',
        '185.25.180.0/23', '190.217.33.0/24', '205.196.6.0/24',
        '1.2.3.0/24',
      ],
      gameFilter: 'tcp',
      changedAt: Date.now() - 3 * 3600 * 1000,
      groups: [
        {
          asn: '32590',
          name: 'Valve Corporation',
          at: Date.now() - 3 * 3600 * 1000,
          nets: [
            '103.10.124.0/23', '146.66.152.0/22', '146.66.155.0/24', '155.133.224.0/22',
            '155.133.226.0/24', '155.133.230.0/24', '155.133.232.0/24', '162.254.192.0/21',
            '185.25.180.0/23', '190.217.33.0/24', '205.196.6.0/24',
          ],
        },
        { asn: '', name: '', at: 0, legacy: true, nets: ['1.2.3.0/24'] },
      ],
      skipped: [
        { addr: '104.29.153.1', asn: '13335', name: 'Cloudflare, Inc.', prefixes: 2395 },
      ],
    }),
    gameCandidates: async () => [
      { name: 'cs2.exe', score: 12, addrs: 3, ports: [27015, 27018] },
      { name: 'steam.exe', score: 4, addrs: 1, ports: [27023] },
    ],
    onGameScan: () => () => {},
    scanGameTraffic: async () => ({
      running: true,
      addrs: ['104.16.0.1', '162.159.135.232', '155.133.226.76'],
      tcpPorts: [443, 27018],
      udpPorts: [27015],
      ticks: 15,
      note: 'собрано адресов: 3. Это те, до которых игра ДОШЛА; если её режут на подключении, часть серверов сюда не попадёт — сканируй при работающем обходе',
    }),
    scanGameFromLog: async () => ({
      running: true,
      process: 'cs2.exe',
      addrs: ['146.66.155.73', '155.133.226.76'],
      tcpPorts: [],
      udpPorts: [],
      ticks: 60,
      note: 'Игра: cs2.exe, поймано адресов: 2. В список идёт не сам адрес: Klutz узнаёт оператора и берёт все его сети. Game Filter включён на UDP — без него игровые порты обход не видит.',
    }),
    clearGameIps: noop,
    excludeGameIps: noop,
    removeGameIps: async () => ({ ok: true }),
    identifyGameGroup: async () => ({ ok: true }),
    getExtraStrategies: async () => ({ count: 0, template: 'general (ALT).bat' }),
    generateExtraStrategies: noop,
    removeExtraStrategies: noop,

    updateIpsetList: noop,
    updateHostsFile: noop,
    checkUpdates: async () => ({
      ok: true,
      error: null,
      local: '1.9.9c',
      remote: '1.9.10',
      upToDate: false,
      releaseUrl: 'https://github.com/Flowseal/zapret-discord-youtube/releases/tag/1.9.10',
    }),
    clearDiscordCache: noop,
    runDiagnostics: async (deep) => ({
      ok: true,
      results: [
        { label: 'Служба фильтрации Windows (BFE)', ok: true },
        { label: 'Драйвер WinDivert', ok: true },
        { label: 'winws.exe', ok: true },
        { label: 'Файл hosts', ok: false, fixKey: 'hosts', warn: 'найдены посторонние записи' },
        { label: 'UDP наружу проходит (голос Discord, QUIC)', ok: true },
      ],
    }),
    fixDiagnostic: noop,

    getCustomLists: async () => ({ ok: true, include: '', exclude: '' }),
    saveCustomLists: noop,

    checkGames: async () => ({ ok: true, targets: allTargets().map(pingResult), pending: false, checkedAt: Date.now(), running: true, strategy: state.activeConfig }),
    getGameTargets: async () => ({ targets: allTargets() }),
    getDefaultGameTargets: async () => ({ targets: DEFAULT_TARGETS }),
    saveGameTargets: async (targets) => {
      customTargets = targets.filter((t) => !DEFAULT_TARGETS.some((d) => d.host === t.host && d.port === t.port));
      return { ok: true, targets: allTargets() };
    },
    resetGameTargets: async () => {
      customTargets = [];
      return { ok: true, targets: allTargets() };
    },
    getAutostart: async () => ({ enabled: true }),
    setAutostart: noop,

    exportSettings: async () => ({ ok: false, cancelled: true }),
    importSettings: async () => ({ ok: true }),

    getNotifications: async () => ({ enabled: true, supported: true }),
    setNotifications: noop,
    testNotification: noop,

    getNotifySound: async () => ({ volume: 70, duration: 'short' }),
    setNotifySound: noop,
    onPlayNotifySound: () => () => {},

    getAutoSwitch: async () => ({ enabled: true, threshold: 3, intervalSec: 30, hasRanking: true }),
    setAutoSwitch: async () => ({ ok: true }),
    getHealLog: async () => ({
      ok: true,
      entries: [
        { at: Date.now() - 40 * 60000, type: 'switch', from: 'general.bat', to: 'general (ALT).bat', ok: true },
        { at: Date.now() - 90 * 60000, type: 'switch', from: 'general (MGTS).bat', to: 'general.bat', ok: false },
        // Запись «сдалось» бэкенд теперь пишет — до этого ветка в рендерере была мёртвой.
        { at: Date.now() - 120 * 60000, type: 'gave-up', from: 'general.bat', to: null, ok: false, triedCount: 7 },
      ],
      workingConfig: 'general (ALT).bat',
      workingAt: Date.now() - 12 * 60000,
    }),

    getAutoTestSchedule: async () => ({ enabled: false, days: 14, mode: 'dpi', lastRunAt: null }),
    setAutoTestSchedule: noop,
    onAutoSwitched: () => () => {},

    // Не пустой список: ровно на пустом списке в своё время и не заметили,
    // что карточка релизов рендерит «undefined». Поля — те же, что у Rust.
    getTestHistory: async () => ({
      ok: true,
      runs: [
        { date: '2026-09-10_19-20', file: '2026-09-10_19-20.txt', release: 'zapret-discord-youtube-1.9.9c', best: 'general (ALT11).bat', mode: 'standard', bestOk: 36, bestTotal: 36 },
        { date: '2026-09-13_21-44', file: '2026-09-13_21-44.txt', release: 'zapret-discord-youtube-1.10.2', best: 'general (ALT12).bat', mode: 'standard', bestOk: 34, bestTotal: 36 },
      ],
      configs: [
        { name: 'general (ALT).bat', latestShare: 0.86, shareSeries: [1, 0.86], wins: 1 },
        { name: 'general (ALT2).bat', latestShare: 1, shareSeries: [0.71, 1], wins: 1 },
      ],
    }),
    openResultFile: noop,
    getDiscordQuic: async () => ({ enabled: false, installed: true }),
    setDiscordQuic: noop,
    getReleaseRegression: async () => ({
      current: 'zapret-discord-youtube-1.10.2',
      previous: 'zapret-discord-youtube-1.9.9c',
      curBest: 'general (ALT12).bat',
      curOk: 34,
      curTotal: 36,
      prevBest: 'general (ALT11).bat',
      prevOk: 36,
      prevTotal: 36,
      drops: [{ name: 'general (ALT11).bat', prevOk: 36, curOk: 12, total: 36 }],
      rollbackPath: 'C:\\Users\\vbu00\\Desktop\\zapret-discord-youtube-1.9.9c',
    }),
    openExternalUrl: noop,
    openReleaseFolder: noop,

    startTgwsproxy: noop,
    stopTgwsproxy: noop,
    restartTgwsproxy: noop,
    getTgwsproxyStatus: async () => ({ running: true, healthy: true, available: true, host: '127.0.0.1', port: 8443, autostart: false, tgProxyUrl: 'tg://proxy?server=127.0.0.1&port=8443&secret=dda1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4' }),
    openTgProxyLink: noop,
    getTgwsproxyLog: async () => ({ lines: ['[tgws] прокси запущен', '[tgws] клиент подключился 127.0.0.1:51422', '[tgws] handshake ok'] }),
    onTgwsproxyLog: () => () => {},
    onTgwsproxyStateChanged: () => () => {},
    getTgwsproxySettings: async () => ({ host: '127.0.0.1', port: 8443, secret: 'a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4', dcIps: ['2:149.154.167.220', '4:149.154.167.220'], cfproxy: true, autoStart: false }),
    setTgwsproxySettings: noop,
    regenerateTgwsproxySecret: async () => ({ ok: true, secret: 'ee' + 'ffffffffffffffffffffffffffffffff' }),
    setTgwsproxyAutostart: noop,

    windowMinimize: noop,
    windowToggleMaximize: noop,
    windowClose: noop,
    windowIsMaximized: async () => false,
    onWindowMaximized: () => () => {},
  };
})();
