// Заменяет preload.js/contextBridge из Electron-версии: собирает тот же
// объект window.zapret, но поверх Tauri invoke()/event API. renderer.js
// продолжает звать window.zapret.* как раньше.
//
// Заглушек здесь больше нет: каждый метод ведёт в настоящую команду Rust.
(function () {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const { open, save } = window.__TAURI__.dialog;
  const { getCurrentWindow } = window.__TAURI__.window;
  const { getCurrentWebview } = window.__TAURI__.webview;

  window.zapret = {
    copyText: (text) => invoke('copy_text', { text }),

    // Возвращает { ok, dataUri }. Картинку качает и проверяет Rust —
    // сюда приходит готовый data-URI, окно в сеть не ходит.
    getFavicon: (host) => invoke('get_favicon', { host }),

    // Перетаскивание идёт не через DOM: WebView2 не даёт настоящий путь
    // (File.path — свойство Electron, здесь его нет), а Tauri перехватывает
    // drop сам и оставляет dataTransfer.files пустым. Настоящие пути
    // приходят только этим событием.
    onFileDrop: (cb) => {
      const un = getCurrentWebview().onDragDropEvent((e) => cb(e.payload));
      return () => un.then((f) => f());
    },

    pickFolder: async () => {
      const dir = await open({ directory: true, multiple: false });
      return dir || null;
    },
    pickArchive: async () => {
      const f = await open({ multiple: false, filters: [{ name: 'Архив zapret', extensions: ['zip'] }] });
      return f || null;
    },
    loadPath: (p) =>
      String(p).toLowerCase().endsWith('.zip')
        ? invoke('load_archive', { zipPath: p })
        : invoke('load_path', { inputPath: p }),
    getState: () => invoke('get_state'),

    runConfig: (fileName) => invoke('run_config', { fileName }),
    stopConfig: () => invoke('stop_config'),
    getWinwsLog: () => invoke('get_winws_log'),
    onWinwsLog: (cb) => {
      const un = listen('winws-log', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    onWinwsCrashed: (cb) => {
      const un = listen('winws-crashed', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },

    windowMinimize: () => invoke('window_minimize'),
    windowToggleMaximize: () => invoke('window_toggle_maximize'),
    windowClose: () => invoke('window_close'),
    windowIsMaximized: () => invoke('window_is_maximized'),
    onWindowMaximized: (cb) => {
      const win = getCurrentWindow();
      const un = win.onResized(async () => cb(await win.isMaximized()));
      return () => un.then((f) => f());
    },

    listReleases: () => invoke('list_releases'),
    deleteRelease: (folderName) => invoke('delete_release', { folderName }),
    getLatestReleaseInfo: () => invoke('get_latest_release_info'),
    downloadLatestRelease: () => invoke('download_latest_release'),
    onDownloadProgress: (cb) => {
      const un = listen('download-progress', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    getOnboardingDone: () => invoke('get_onboarding_done'),
    setOnboardingDone: (done) => invoke('set_onboarding_done', { done }),
    installService: (fileName) => invoke('install_service', { fileName }),
    removeService: () => invoke('remove_service'),
    getServiceStatus: () => invoke('get_service_status'),
    // mode: 'standard' | 'dpi' | 'funnel' (DPI по всем → HTTP только по тем,
    // кто прошёл DPI на 100%).
    runTests: (opts) => invoke('run_tests', { mode: (opts && opts.mode) || 'standard' }),
    stopTests: () => invoke('stop_tests'),
    getLastTestResults: () => invoke('get_last_test_results'),
    onTestLog: (cb) => {
      const un = listen('test-log', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    getToggles: () => invoke('get_toggles'),
    setGameFilter: (mode) => invoke('set_game_filter', { mode }),
    cycleIpsetMode: () => invoke('cycle_ipset_mode'),
    setAutoUpdate: (enabled) => invoke('set_auto_update', { enabled }),
    checkBypassChance: () => invoke('check_bypass_chance'),
    getGameScan: () => invoke('get_game_scan'),
    gameCandidates: () => invoke('game_candidates'),
    onGameScan: (cb) => {
      const un = listen('game-scan', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    scanGameTraffic: (images, seconds) => invoke('scan_game_traffic', { images, seconds: seconds || null }),
    scanGameFromLog: (seconds) => invoke('scan_game_from_log', { seconds: seconds || null }),
    clearGameIps: () => invoke('clear_game_ips'),
    excludeGameIps: () => invoke('exclude_game_ips'),
    removeGameIps: (addrs) => invoke('remove_game_ips', { addrs }),
    identifyGameGroup: (nets) => invoke('identify_game_group', { nets }),
    getExtraStrategies: () => invoke('get_extra_strategies'),
    generateExtraStrategies: (template) => invoke('generate_extra_strategies', { template: template || null }),
    removeExtraStrategies: () => invoke('remove_extra_strategies'),

    updateIpsetList: () => invoke('update_ipset_list'),
    updateHostsFile: () => invoke('update_hosts_file'),
    checkUpdates: () => invoke('check_updates'),
    clearDiscordCache: () => invoke('clear_discord_cache'),
    runDiagnostics: (deep) => invoke('run_diagnostics', { deep: !!deep }),
    fixDiagnostic: (key) => invoke('fix_diagnostic', { key }),
    getCustomLists: () => invoke('get_custom_lists'),
    saveCustomLists: (l) => invoke('save_custom_lists', { include: (l&&l.include)||'', exclude: (l&&l.exclude)||'' }),
    checkGames: () => invoke('check_games'),
    getGameTargets: () => invoke('get_game_targets'),
    getDefaultGameTargets: () => invoke('get_default_game_targets'),
    saveGameTargets: (targets) => invoke('save_game_targets', { targets }),
    resetGameTargets: () => invoke('reset_game_targets'),
    getAutostart: () => invoke('get_autostart'),
    setAutostart: (enabled) => invoke('set_autostart', { enabled }),
    exportSettings: async () => {
      const path = await save({ defaultPath: 'klutz-settings.json', filters: [{ name: 'JSON', extensions: ['json'] }] });
      if (!path) return { ok: false, cancelled: true };
      return invoke('export_settings', { path });
    },
    importSettings: async () => {
      const path = await open({ multiple: false, filters: [{ name: 'JSON', extensions: ['json'] }] });
      if (!path) return { ok: false, cancelled: true };
      return invoke('import_settings', { path });
    },
    getNotifications: () => invoke('get_notifications'),
    setNotifications: (enabled) => invoke('set_notifications', { enabled }),
    testNotification: () => invoke('test_notification'),
    getNotifySound: () => invoke('get_notify_sound'),
    setNotifySound: (o) => invoke('set_notify_sound', { volume: o && o.volume, duration: o && o.duration }),
    // Тост уходит беззвучным, а сигнал с настоящей громкостью играет renderer.
    onPlayNotifySound: (cb) => {
      const un = listen('play-notify-sound', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    getAutoSwitch: () => invoke('get_auto_switch'),
    setAutoSwitch: (o) => invoke('set_auto_switch', { enabled: !!o.enabled, threshold: o.threshold, intervalSec: o.intervalSec }),
    getHealLog: () => invoke('get_heal_log'),
    getAutoTestSchedule: () => invoke('get_auto_test_schedule'),
    setAutoTestSchedule: (o) => invoke('set_auto_test_schedule', { enabled: o && o.enabled, days: o && o.days, mode: o && o.mode }),
    onAutoSwitched: (cb) => {
      const un = listen('auto-switched', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    getTestHistory: () => invoke('get_test_history'),
    // release — из какого релиза прогон: история хранится в папке Klutz
    // по релизам, и файл прежнего релиза лежит не в текущей папке.
    openResultFile: (fileName, release) => invoke('open_result_file', { fileName, release: release || null }),
    getReleaseRegression: () => invoke('get_release_regression'),
    getDiscordQuic: () => invoke('get_discord_quic'),
    setDiscordQuic: (enabled) => invoke('set_discord_quic', { enabled }),
    openExternalUrl: (url) => invoke('open_external_url', { url }),
    openReleaseFolder: () => invoke('open_release_folder'),
    getVersions: () => invoke('get_versions'),
    checkComponentUpdates: () => invoke('check_component_updates'),
    checkKlutzUpdate: () => invoke('check_klutz_update'),
    startTgwsproxy: () => invoke('start_tgwsproxy'),
    stopTgwsproxy: () => invoke('stop_tgwsproxy'),
    restartTgwsproxy: () => invoke('restart_tgwsproxy'),
    getTgwsproxyStatus: () => invoke('get_tgwsproxy_status'),
    openTgProxyLink: () => invoke('open_tg_proxy_link'),
    getTgwsproxyLog: () => invoke('get_tgwsproxy_log'),
    onTgwsproxyLog: (cb) => {
      const un = listen('tgwsproxy-log', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    onTgwsproxyStateChanged: (cb) => {
      const un = listen('tgwsproxy-state-changed', (e) => cb(e.payload));
      return () => un.then((f) => f());
    },
    getTgwsproxySettings: () => invoke('get_tgwsproxy_settings'),
    setTgwsproxySettings: (o) => invoke('set_tgwsproxy_settings', o || {}),
    regenerateTgwsproxySecret: () => invoke('regenerate_tgwsproxy_secret'),
    setTgwsproxyAutostart: (enabled) => invoke('set_tgwsproxy_autostart', { enabled }),
  };
})();
