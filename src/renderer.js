// ═══════════════════════════════════════════════════════════════
//  Klutz — renderer
// ═══════════════════════════════════════════════════════════════

const $ = (id) => document.getElementById(id);

const shell = $('shell');
const dropZone = $('dropZone');
const loadError = $('loadError');

let currentState = { rootPath: null, configs: [], activeConfig: null, running: false, monitor: null };
// "Сменить релиз" only clears this local flag, not the backend's state.rootPath
// (which still points at the working release in case the user backs out) — so
// the periodic refreshState() poll below would otherwise restore the shell out
// from under the onboarding screen a few seconds later. This guards against that.
let choosingNewRelease = false;
// Set by the setup wizard when the user picks "Telegram only" — unlocks the
// shell (Telegram page specifically) without a zapret release loaded, since
// TgWsProxy doesn't actually depend on one. Everything else in the shell
// (Стратегии/Диагностика/Настройки) still assumes a release exists in a lot
// of places that were never audited for a null rootPath, so those stay
// gated on a real release regardless of this flag.
let telegramOnlyMode = localStorage.getItem('zapretTelegramOnly') === '1';
let lastServiceStatus = null;
let lastCheck = null;
let testing = false;
// Автоподбор отработал, но связь после него так и не появилась — Главная
// показывает это отдельным состоянием, пока пользователь что-то не поменяет.
let pickFailed = false;
let groupFilter = null;

// ─────────── Тема ───────────

let theme = localStorage.getItem('zapretTheme') || 'light';

function applyTheme() {
  document.documentElement.setAttribute('data-theme', theme);
  localStorage.setItem('zapretTheme', theme);
}

$('themeBtn').onclick = () => {
  theme = theme === 'dark' ? 'light' : 'dark';
  applyTheme();
};
applyTheme();

// ─────────── Кнопки окна ───────────

$('winMinBtn').onclick = () => window.zapret.windowMinimize();
$('winMaxBtn').onclick = () => window.zapret.windowToggleMaximize();
$('winCloseBtn').onclick = () => window.zapret.windowClose();

function setMaximized(on) {
  document.body.classList.toggle('maximized', !!on);
  $('winMaxBtn').title = on ? 'Свернуть в окно' : 'Развернуть';
}
window.zapret.onWindowMaximized(setMaximized);
// Команда отдаёт голый bool, а не { maximized }.
window.zapret.windowIsMaximized().then((r) => setMaximized(r === true || !!(r && r.maximized)));

// ─────────── Тосты / подтверждение ───────────

const toastContainer = $('toastContainer');

const TOAST_ICONS = {
  success: '<circle cx="8" cy="8" r="6.5"/><path d="M5.2 8.2l1.9 1.9 3.8-4"/>',
  error: '<circle cx="8" cy="8" r="6.5"/><path d="M8 4.8v3.9"/><circle cx="8" cy="11.2" r=".6" fill="currentColor" stroke="none"/>',
  warn: '<circle cx="8" cy="8" r="6.5"/><path d="M8 4.8v3.9"/><circle cx="8" cy="11.2" r=".6" fill="currentColor" stroke="none"/>',
  info: '<circle cx="8" cy="8" r="6.5"/><path d="M8 7.3v3.9"/><circle cx="8" cy="4.9" r=".6" fill="currentColor" stroke="none"/>',
};

// opts: { body, actionLabel, onAction } — тело и кнопка необязательны.
function showToast(title, type = 'info', opts = {}) {
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  el.innerHTML = `
    <span class="toast-icon"><svg width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">${
      TOAST_ICONS[type] || TOAST_ICONS.info
    }</svg></span>
    <div class="toast-text">
      <div class="toast-title">${esc(title)}</div>
      ${opts.body ? `<div class="toast-body">${esc(opts.body)}</div>` : ''}
    </div>
    ${opts.actionLabel ? '<button class="toast-action"></button>' : ''}`;
  if (opts.actionLabel) {
    const btn = el.querySelector('.toast-action');
    btn.textContent = opts.actionLabel;
    btn.onclick = () => {
      el.remove();
      if (opts.onAction) opts.onAction();
    };
  }
  toastContainer.appendChild(el);
  setTimeout(() => {
    el.style.transition = 'opacity .2s';
    el.style.opacity = '0';
    setTimeout(() => el.remove(), 200);
  }, 4200);
}

// Вызов команды, который не бросает: упавшая команда или мост возвращается
// обычным отказом { ok: false, error }. Обработчики кнопок и так умеют его
// показать и вернуть кнопку — а исключение до этой ветки не доходило, и
// кнопка с «Проверяю…» оставалась заблокированной до перезапуска Klutz.
function callSafe(promise) {
  return Promise.resolve(promise).catch((e) => ({
    ok: false,
    error: String((e && e.message) || e || 'неизвестная ошибка'),
  }));
}

const confirmOverlay = $('confirmOverlay');

function showConfirm(message) {
  return new Promise((resolve) => {
    $('confirmMessage').textContent = message;
    confirmOverlay.classList.remove('hidden');
    const done = (result) => {
      confirmOverlay.classList.add('hidden');
      $('confirmOkBtn').onclick = null;
      $('confirmCancelBtn').onclick = null;
      resolve(result);
    };
    $('confirmOkBtn').onclick = () => done(true);
    $('confirmCancelBtn').onclick = () => done(false);
  });
}

// ─────────── Утилиты ───────────

function bareName(name) {
  return name.replace(/\.bat$/i, '');
}

function displayName(name) {
  return name ? prettyName(name) : '—';
}

// «general (FAKE TLS AUTO ALT3).bat» → «FAKE TLS AUTO ALT3»,
// «general.bat» → «Базовый» — как подписи в макете.
function prettyName(name) {
  const base = bareName(name);
  const m = base.match(/^general\s*\((.+)\)$/i);
  if (m) return m[1].trim();
  if (/^general$/i.test(base)) return 'Базовый';
  return base;
}

// Семейства как в макете: базовый и все ALT — это один набор («Базовые»),
// остальные собираются по префиксу. Раньше ALT жили отдельной группой из
// тринадцати строк, а FAKE TLS AUTO и FAKE TLS AUTO ALT расходились по
// разным семействам.
function deriveGroup(name) {
  const p = prettyName(name);
  if (p === 'Базовый' || /^ALT\d*$/i.test(p)) return 'Базовые';
  if (/^FAKE TLS/i.test(p)) return 'FAKE TLS';
  if (/^SIMPLE FAKE/i.test(p)) return 'SIMPLE FAKE';
  if (/^MGTS/i.test(p)) return 'MGTS';
  return p;
}

function formatUptime(startedAt) {
  if (!startedAt) return '—';
  const sec = Math.max(0, Math.floor((Date.now() - startedAt) / 1000));
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  if (h > 0) return `${h} ч ${m} мин`;
  if (m > 0) return `${m} мин ${s} сек`;
  return `${s} сек`;
}

function shortPath(p) {
  if (!p) return '';
  return p.length > 52 ? '…' + p.slice(-49) : p;
}

function esc(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
}

// ─────────── Режим и навигация ───────────

// A telegram-only user has no release, so Home/Стратегии/Диагностика stay
// hidden (see render()) — defaulting to 'home' would land them on a blank
// page every single launch, not just the first one right after the wizard.
let activePage = telegramOnlyMode ? 'telegram' : 'home';
let activeSubtab = 'configs';

// Простой режим — по умолчанию: Главная сводится к одной кнопке, Стратегии и
// Диагностика скрыты целиком. Продвинутый включается в Настройках.
let uiMode = localStorage.getItem('zapretUiMode') === 'advanced' ? 'advanced' : 'simple';
const isAdvanced = () => uiMode === 'advanced';

// Держится 700 мс после включения обхода — под него рисуется ударная волна
// вокруг круглой кнопки.
let launching = false;

const sidebar = $('sidebar');

if (localStorage.getItem('zapretSidebarCollapsed') === '1') sidebar.classList.add('collapsed');

$('sidebarCollapseBtn').onclick = () => {
  const collapsed = sidebar.classList.toggle('collapsed');
  localStorage.setItem('zapretSidebarCollapsed', collapsed ? '1' : '0');
};

function switchPage(name) {
  activePage = name;
  render();
  if (name === 'home') {
    loadOverview();
    ensureTargetsLoaded();
  }
  if (name === 'diagnostics') {
    ensureTargetsLoaded();
    loadGameTargetsArea();
  }
  if (name === 'games') loadGames();
  if (name === 'telegram') {
    loadTgwsproxyStatus();
    loadTgwsproxyAutostart();
    openTgwsproxySettings();
  }
  if (name === 'settings') {
    loadToggles();
    loadServiceStatus();
    loadAutostart();
    loadAutoSwitch();
    loadAutoTestSchedule();
    loadNotifications();
    loadNotifySound();
    loadCustomLists();
    loadReleaseList();
  }
  if (name === 'strategies' && activeSubtab === 'tests') loadTestsHistory();
}

function switchSubtab(name) {
  activeSubtab = name;
  render();
  if (name === 'tests') loadTestsHistory();
}

document.querySelectorAll('.nav-item[data-page]').forEach((el) => {
  el.onclick = () => switchPage(el.dataset.page);
});

document.querySelectorAll('[data-subtab]').forEach((el) => {
  el.onclick = () => {
    switchPage('strategies');
    switchSubtab(el.dataset.subtab);
  };
});

document.querySelectorAll('.chip[data-goto]').forEach((el) => {
  el.onclick = () => switchPage(el.dataset.goto);
});

$('homeGoTestsBtn').onclick = () => {
  switchPage('strategies');
  switchSubtab('tests');
};

// ─────────── Отрисовка каркаса ───────────

function render() {
  const hasRelease = !!currentState.rootPath;
  // Telegram doesn't actually need a zapret release — telegramOnlyMode lets
  // the setup wizard unlock just that one page without it. Everything else
  // stays gated on a real release: too much of Стратегии/Диагностика/
  // Настройки assumes one exists to safely open up without a proper audit.
  const shellUnlocked = (hasRelease || telegramOnlyMode) && !choosingNewRelease;
  dropZone.classList.toggle('hidden', shellUnlocked);
  shell.classList.toggle('hidden', !shellUnlocked);

  $('pageHome').classList.toggle('hidden', !hasRelease || activePage !== 'home');
  $('pageStrategies').classList.toggle('hidden', !hasRelease || activePage !== 'strategies');
  $('pageDiagnostics').classList.toggle('hidden', !hasRelease || activePage !== 'diagnostics');
  $('pageGames').classList.toggle('hidden', !hasRelease || activePage !== 'games');
  $('pageTelegram').classList.toggle('hidden', !shellUnlocked || activePage !== 'telegram');
  $('pageSettings').classList.toggle('hidden', !hasRelease || activePage !== 'settings');

  // Стратегии и Диагностика существуют только в продвинутом режиме.
  document.documentElement.classList.toggle('simple', !isAdvanced());
  const ADV_ONLY_PAGES = ['strategies', 'diagnostics'];
  document.querySelectorAll('.nav-item[data-page]').forEach((el) => {
    const advGated = ADV_ONLY_PAGES.includes(el.dataset.page) && !isAdvanced();
    el.classList.toggle('hidden', advGated || (el.dataset.page !== 'telegram' && !hasRelease));
    el.classList.toggle('active', el.dataset.page === activePage);
  });
  document.querySelectorAll('[data-subtab]').forEach((el) =>
    el.classList.toggle('active', el.dataset.subtab === activeSubtab && activePage === 'strategies')
  );

  $('paneConfigs').classList.toggle('hidden', activeSubtab !== 'configs');
  $('paneTests').classList.toggle('hidden', activeSubtab !== 'tests');

  renderStatusbar();
  renderHero();
  renderConfigList();
  renderHomeFooter();
}

function renderStatusbar() {
  const running = !!currentState.running;
  $('statusbar').classList.toggle('running', running);
  $('tbDot').classList.toggle('on', running);

  $('statusText').textContent = running ? 'Работает' : 'Остановлен';
  $('sbarSep1').classList.toggle('hidden', !running);
  // winws.exe может работать и без нашей стратегии — службой «zapret»,
  // поставленной не из Klutz. Раньше строка оставалась пустой или «—».
  $('sbarName').textContent = !running
    ? ''
    : currentState.activeConfig
    ? displayName(currentState.activeConfig)
    : currentState.serviceExists
    ? 'служба zapret'
    : '';
  $('sbarUptime').textContent = running ? formatUptime(currentState.startedAt) : '';
}

function renderTgStatusbar(on) {
  $('sbarTgDot').classList.toggle('on', on);
  $('sbarTgText').classList.toggle('on', on);
  $('sbarTgText').textContent = on ? 'Telegram-прокси' : 'Telegram-прокси выключен';
}

// Какие цели «ядровые» (Discord/YouTube) в последней проверке связи.
function coreCheck() {
  const src = lastCheck || currentState.monitor;
  if (!src || !src.targets) return null;
  const core = src.targets.filter((t) => CORE_RE.test(t.name));
  if (!core.length) return null;
  return { total: core.length, ok: core.filter((t) => t.ok).length, at: src.checkedAt, targets: core };
}

// Что сейчас держит обход. winws может работать и без конфига из Klutz:
// службой «zapret», поставленной не отсюда, или .bat, запущенным руками.
function activeLabel() {
  if (currentState.activeConfig) return displayName(currentState.activeConfig);
  return currentState.serviceExists ? 'служба zapret' : 'winws.exe не из Klutz';
}

// Конфиг из прошлого прогона — среди конфигов ТЕКУЩЕГО релиза. История
// прогонов хранит и прежние релизы, а варианты можно убрать: раньше
// «Включить» брало такой конфиг и падало с ошибкой на каждое нажатие.
function findConfig(name) {
  if (!name) return null;
  const bare = (n) => String(n).replace(/\.bat$/i, '').toLowerCase();
  return (currentState.configs || []).find((c) => bare(c) === bare(name)) || null;
}

// Пять состояний, как в макете: подбираю → выключен → ничего не пробило →
// работает частично → работает. Ровно один блок виден за раз.
function renderHero() {
  // По факту работы winws, а не по конфигу из Klutz: иначе при чужой службе
  // строка состояния говорила «Работает», а Главная — «Выключен».
  const running = !!currentState.running;
  // Как в макете: верим только проверке, сделанной после запуска текущего
  // конфига. Старая, от прошлого варианта, иначе держала бы «Работает, но…»
  // до следующей проверки.
  const rawCheck = coreCheck();
  const check =
    rawCheck && (!currentState.startedAt || rawCheck.at >= currentState.startedAt) ? rawCheck : null;
  const state = testing
    ? 'picking'
    : !running
    ? 'idle'
    : pickFailed
    ? 'failed'
    : check && check.ok < check.total
    ? 'degraded'
    : 'ok';

  const adv = isAdvanced();
  $('heroSimple').classList.toggle('hidden', adv);
  $('heroPicking').classList.toggle('hidden', !adv || state !== 'picking');
  $('heroIdle').classList.toggle('hidden', !adv || state !== 'idle');
  $('heroFailed').classList.toggle('hidden', !adv || state !== 'failed');
  $('heroDegraded').classList.toggle('hidden', !adv || state !== 'degraded');
  $('heroActive').classList.toggle('hidden', !adv || state !== 'ok');
  if (!adv) renderSimpleHero(state, check, running);

  // "Что обходим" reflects reality regardless of idle/active — updated here
  // (not just on click) so tray/self-heal/wizard changes show up too.
  $('engineZapretToggle').classList.toggle('on', running);
  $('engineZapretDot').className = 'status-dot' + (running ? ' ok' : '');

  const variant = currentState.activeConfig || lastTestBest;
  $('statVariant').textContent = variant ? displayName(variant) : 'не подобран';
  $('statNow').textContent = running ? `${activeLabel()} — обход включён` : 'Discord и YouTube напрямую';
  $('heroStartLabel').textContent = findConfig(lastTestBest) ? 'Включить' : 'Подобрать и включить';

  if (state === 'ok') {
    $('heroOkTitle').textContent = check && check.ok === check.total ? 'Discord и YouTube отвечают' : 'Обход включён';
    $('heroName').textContent = activeLabel();
    $('heroUptime').textContent = formatUptime(currentState.startedAt);
    const when = check ? new Date(check.at).toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' }) : null;
    $('heroHealth').textContent = check ? `${check.ok} из ${check.total} · проверено ${when}` : 'не проверялись';
    $('heroHealth').classList.toggle('ok-val', !!check && check.ok === check.total);
  }

  if (state === 'degraded') {
    const failing = [...new Set(check.targets.filter((t) => !t.ok).map((t) => t.name.split(' ')[0]))];
    $('heroDegradedName').textContent = activeLabel();
    $('heroDegradedTitle').textContent = failing.length
      ? `Работает, но ${failing.join(' и ')} не ${failing.length > 1 ? 'отвечают' : 'отвечает'}`
      : 'Работает, но не всё отвечает';
    const note = pathNote(check.targets);
    $('heroDegradedSub').textContent =
      `Обход запущен, но ${check.ok} из ${check.total} целей отвечают. ` +
      (note || 'Это не ошибка приложения — провайдер мог сменить блокировку. Обычно помогает другой вариант.');
  }

  if (state === 'failed') {
    const note = check ? pathNote(check.targets) : '';
    $('heroFailedSub').textContent =
      `Сейчас включён лучший из проверенных — ${activeLabel()}, но Discord и YouTube ` +
      'не отвечают. ' + (note || 'Иногда помогает соседний вариант или перезапуск через минуту.');
    renderHeroAlternatives();
  }
}

// Простой режим: одна кнопка и текст под ней вместо всей раскладки.
// Состояния те же самые (picking/idle/failed/degraded/ok), просто показаны
// одним блоком, а вокруг кнопки крутятся ореолы под текущее состояние.
function renderSimpleHero(state, check, running) {
  const activeName = activeLabel();
  const live = running && state !== 'picking';

  $('heroSimple').classList.toggle('ok', state === 'ok');

  const circle = $('simpleCircle');
  circle.classList.toggle('on', live);
  circle.classList.toggle('ok', state === 'ok');
  circle.title = running ? 'Остановить обход' : 'Включить обход';

  // Ореолы — чистая декорация, пересобираем их целиком под состояние.
  const halos = [];
  if (live) {
    halos.push('<span class="halo-glow"></span>', '<span class="halo-sweep"></span>');
    halos.push('<span class="halo-ring"></span>', '<span class="halo-ring delayed"></span>');
  }
  if (launching) halos.push('<span class="halo-shock"></span>', '<span class="halo-shock fill"></span>');
  if (state === 'picking') halos.push('<span class="halo-pick"></span>');
  $('simpleHalo').innerHTML = halos.join('');

  $('simpleHint').textContent =
    state === 'picking' ? 'подбираю' : running ? 'нажмите, чтобы остановить' : 'нажмите, чтобы включить';

  const failing = check ? [...new Set(check.targets.filter((t) => !t.ok).map((t) => t.name.split(' ')[0]))] : [];
  $('simpleTitle').textContent =
    state === 'picking'
      ? 'Проверяю варианты обхода'
      : state === 'ok'
      ? check && check.ok === check.total
        ? 'Discord и YouTube отвечают'
        : 'Обход включён'
      : state === 'failed'
      ? 'Ни один вариант не пробил блокировку'
      : state === 'degraded'
      ? failing.length
        ? `Работает, но ${failing.join(' и ')} не ${failing.length > 1 ? 'отвечают' : 'отвечает'}`
        : 'Работает, но не всё отвечает'
      : 'Обход выключен';

  // Совет в простом режиме тот же, что и в подробном: если известно, что режут
  // адрес или отказывает сам сервер, «подберите другой вариант» — вредный
  // совет, перебор там ничего не даст.
  const note = check ? pathNote(check.targets) : '';

  $('simpleSub').textContent =
    state === 'picking'
      ? $('heroPickStatus').textContent
      : state === 'ok'
      ? `${activeName} · работает ${formatUptime(currentState.startedAt)}`
      : state === 'failed'
      ? `Включён лучший из проверенных — ${activeName}, но Discord и YouTube не отвечают. ` +
        (note || 'Прогоните тесты ещё раз или попробуйте другой вариант.')
      : state === 'degraded'
      ? `Включён ${activeName}, отвечают ${check.ok} из ${check.total} целей. ` +
        (note || 'Попробуйте подобрать другой вариант.')
      : findConfig(lastTestBest)
      ? `Включится последний рабочий вариант — ${displayName(lastTestBest)}. Пара секунд, без подбора.`
      : 'Klutz проверит варианты обхода и включит тот, с которым Discord и YouTube откроются. Займёт пару минут.';

  $('simpleProgress').classList.toggle('hidden', state !== 'picking');
  $('simpleCancelWrap').classList.toggle('hidden', state !== 'picking');
  $('simpleProgressFill').style.width = $('heroPickProgress').style.width || '0%';

  const needsHelp = state === 'degraded' || state === 'failed';
  $('simpleHelpActions').classList.toggle('hidden', !needsHelp);
  if (!needsHelp) $('simpleAlts').classList.add('hidden');
}

// Соседние по рейтингу варианты — предлагаем вручную, когда автоподбор не помог.
function renderHeroAlternatives() {
  const rows = (lastResultsCache && lastResultsCache.rows) || [];
  const alts = rows
    .filter((r) => r.name !== currentState.activeConfig)
    .slice(0, 3)
    .map((r) => {
      const score = verdictFor(r, lastResultsCache.mode).score;
      return { name: r.name, desc: `${Math.round(score * 100)}% по последнему прогону` };
    });
  const box = $('heroAlts');
  if (!alts.length) {
    box.innerHTML = '<div class="hero-alt-desc">Прошлых результатов нет — прогони тесты, чтобы появились варианты.</div>';
    return;
  }
  box.innerHTML = alts
    .map(
      (a, i) => `
      <div class="hero-alt">
        <div>
          <div class="hero-alt-title">${esc(displayName(a.name))}</div>
          <div class="hero-alt-desc">${esc(a.desc)}</div>
        </div>
        <button class="btn primary xs" data-alt="${i}">Включить</button>
      </div>`
    )
    .join('');
  box.querySelectorAll('button[data-alt]').forEach((btn) => {
    btn.onclick = async () => {
      const a = alts[Number(btn.dataset.alt)];
      if (await applyConfig(a.name, false, true)) {
        pickFailed = false;
        await verifyAppliedAndToast(a.name);
      }
    };
  });
}

function renderHomeFooter() {
  const wd = lastServiceStatus && lastServiceStatus.windivertState === 'RUNNING' ? 'активен' : 'неактивен';
  $('homeFooter').textContent =
    `Ядро: winws.exe · WinDivert ${wd}`;
  $('maintReleaseSub').textContent = currentState.rootPath
    ? `Релиз: ${shortPath(currentState.rootPath)}`
    : 'одноразовые действия, не настройки';
  const release = currentState.rootPath ? currentState.rootPath.split(/[\\/]/).pop() : null;
  $('sidebarVersionText').textContent = release ? `zapret ${release.replace(/^zapret[-\s]*/i, '')}` : 'zapret не загружен';
  $('sidebarVersionSub').textContent = release
    ? `${(currentState.configs || []).length} конфигов`
    : 'релиз не выбран';
  $('sidebarVersion').title = currentState.rootPath || 'Загруженный релиз zapret';
  $('engineMenuSub').textContent = release || 'релиз не загружен';
}

setInterval(() => {
  if (currentState.running) {
    $('sbarUptime').textContent = formatUptime(currentState.startedAt);
    if (activePage === 'home') $('heroUptime').textContent = formatUptime(currentState.startedAt);
  }
}, 1000);

async function refreshState() {
  currentState = await window.zapret.getState();
  render();
}

// ─────────── Пуск / остановка ───────────

async function stopActive() {
  pickFailed = false;
  if (currentState.installedAsService || currentState.serviceExists) {
    // Чужая служба тоже сюда: taskkill по её winws.exe оставлял службу
    // «завершённой неожиданно», а при следующей загрузке она поднималась
    // снова — обход «не выключался».
    const ok = await showConfirm(
      currentState.installedAsService
        ? 'Стратегия установлена как служба. Снять службу и остановить?'
        : 'Обход работает службой Windows «zapret», установленной не из Klutz. Снять службу и остановить?'
    );
    if (!ok) return;
    const removed = await window.zapret.removeService();
    loadServiceStatus();
    if (removed && removed.ok === false) {
      showToast(removed.error || 'Не удалось снять службу', 'error');
      refreshState();
      return;
    }
  } else {
    // Команда теперь честно отвечает, получилось ли: раньше она всегда
    // возвращала успех, и окно рапортовало «Обход остановлен» поверх
    // работающего обхода.
    const res = await window.zapret.stopConfig();
    if (res && !res.ok) {
      showToast(res.error || 'Не удалось остановить обход', 'error');
      refreshState();
      return;
    }
  }
  showToast('Обход остановлен', 'success');
  refreshState();
}

$('heroStopBtn').onclick = stopActive;
// Обход включён и в «Работает, но…», и после неудачного подбора — остановить
// его должно быть можно из любого состояния, не только из «всё отвечает».
$('heroDegradedStopBtn').onclick = stopActive;
$('heroFailedStopBtn').onclick = stopActive;

// silent: skip the generic success toast — used by the auto-pick flow, which
// shows a more specific one after actually checking whether it worked.
// Служба «zapret», поставленная не из Klutz (через service.bat самого
// zapret), держит свой winws.exe: прямой запуск упирается в неё, а скрипт
// тестов при установленной службе отказывается работать вовсе. Обычная
// картина на первом запуске у тех, кто раньше пользовался zapret вручную:
// «Подобрать и включить» кончалось невнятной ошибкой. Предлагаем снять её
// на месте, а не отправлять искать, где это делается.
//
// Своя служба тоже мешает. Раньше проверка её пропускала: флаг
// installedAsService означал «всё под контролем», и «Включить» шло прямым
// запуском прямо в неё — голая ошибка «сначала сними службу» без кнопки.
// Пропускаем только установку службой: её заменяет сама install_service.
async function ensureNoForeignService(asService) {
  if (!currentState.serviceExists || asService) return true;
  const ok = await showConfirm(
    currentState.installedAsService
      ? 'Обход сейчас держится службой Windows «zapret». Чтобы запустить стратегию из Klutz, ' +
          'службу нужно снять — «Держать обход включённым» при этом выключится. Снять и продолжить?'
      : 'На компьютере уже стоит служба Windows «zapret», установленная не из Klutz. ' +
          'Пока она работает, Klutz не может запускать обход сам. Снять службу и продолжить?'
  );
  if (!ok) return false;
  const res = await window.zapret.removeService();
  if (res && res.ok === false) {
    showToast(res.error || 'Не удалось снять службу', 'error');
    return false;
  }
  await refreshState();
  loadServiceStatus();
  return true;
}

async function applyConfig(name, asService, silent) {
  if (!(await ensureNoForeignService(asService))) return false;
  const res = await callSafe(asService ? window.zapret.installService(name) : window.zapret.runConfig(name));
  if (!res.ok) {
    showToast(res.error || 'Не удалось запустить', 'error');
    return false;
  }
  if (!silent) showToast(`${asService ? 'Установлено службой' : 'Запущено'}: ${displayName(name)}`, 'success');
  await refreshState();
  return true;
}

$('heroStartBtn').onclick = async (e) => {
  const btn = e.currentTarget;
  btn.disabled = true;
  const res = await callSafe(window.zapret.getLastTestResults());
  const best = res.ok ? findConfig(parseResults(res.text).best) : null;
  if (best) {
    const applied = await applyConfig(best, false, true);
    btn.disabled = false;
    if (applied) await verifyAppliedAndToast(best);
    return;
  }
  btn.disabled = false;
  // Рейтинга ещё нет — прогоняем тесты и включаем лучшую.
  runAllTests({ autoApply: true, btn });
};

$('heroRedoBtn').onclick = (e) => runAllTests({ autoApply: true, btn: e.currentTarget });

$('heroCancelBtn').onclick = () => window.zapret.stopTests();
$('heroDegradedPickBtn').onclick = () => runAllTests({ autoApply: true });
$('heroFailedRetryBtn').onclick = () => runAllTests({ autoApply: true });
$('heroDegradedDiagBtn').onclick = () => switchPage('diagnostics');
$('heroFailedDiagBtn').onclick = () => switchPage('diagnostics');
$('heroAltBtn').onclick = () => {
  const open = $('heroAlts').classList.toggle('hidden');
  $('heroAltBtn').textContent = open ? 'Попробовать вручную' : 'Скрыть варианты';
};

// ─────────── Простой режим ───────────

// Одна кнопка на все случаи: работает — остановить, есть рабочий вариант —
// включить его сразу, нет — подобрать. Во время подбора клик игнорируется.
$('simpleCircle').onclick = () => {
  if (testing || launching) return;
  if (currentState.running) stopActive();
  else if (findConfig(lastTestBest)) applyBestFromSimple();
  else runAllTests({ autoApply: true });
};

async function applyBestFromSimple() {
  const name = findConfig(lastTestBest);
  launching = true;
  renderHero();
  setTimeout(() => {
    launching = false;
    renderHero();
  }, 700);
  if (await applyConfig(name, false, true)) await verifyAppliedAndToast(name);
}

$('simpleCancelBtn').onclick = () => window.zapret.stopTests();
$('simplePickBtn').onclick = () => runAllTests({ autoApply: true });
$('simpleAltBtn').onclick = () => {
  const hidden = $('simpleAlts').classList.contains('hidden');
  if (hidden) renderSimpleAlternatives();
  $('simpleAlts').classList.toggle('hidden', !hidden);
  $('simpleAltBtn').textContent = hidden ? 'Скрыть варианты' : 'Выбрать другой вариант';
};

function renderSimpleAlternatives() {
  renderHeroAlternatives();
  $('simpleAlts').innerHTML = $('heroAlts').innerHTML;
  $('simpleAlts')
    .querySelectorAll('button[data-alt]')
    .forEach((btn) => {
      btn.onclick = () => $('heroAlts').querySelector(`button[data-alt="${btn.dataset.alt}"]`).click();
    });
}

// ─────────── Режим простой / продвинутый ───────────

const advancedToggle = $('advancedToggle');

function applyUiMode() {
  advancedToggle.classList.toggle('on', isAdvanced());
  localStorage.setItem('zapretUiMode', uiMode);
  // Из простого режима страницы Стратегии/Диагностика недоступны — если мы
  // на одной из них, уводим на Главную, иначе экран останется пустым.
  if (!isAdvanced() && ['strategies', 'diagnostics'].includes(activePage)) activePage = 'home';
  render();
}

advancedToggle.onclick = () => {
  uiMode = isAdvanced() ? 'simple' : 'advanced';
  applyUiMode();
  showToast(isAdvanced() ? 'Продвинутый режим включён' : 'Простой режим включён', 'success');
};

// ─────────── О программе ───────────

const aboutOverlay = $('aboutOverlay');

// Версия Klutz приходит из сборки (tauri.conf.json), а не зашита в HTML —
// иначе титулбар, «О программе» и установщик рано или поздно разойдутся.
function fillVersions(v) {
  $('tbVersion').textContent = v.app;
  $('aboutAppVersion').textContent = v.app;
  const k = $('aboutKlutzVer');
  k.textContent = v.app;
  k.className = '';
  const z = $('aboutZapretVer');
  z.textContent = v.zapret || 'не загружен';
  z.className = '';
  const t = $('aboutTgwsVer');
  t.textContent = v.tgws;
  t.className = '';
}
window.zapret.getVersions().then(fillVersions);

$('aboutBtn').onclick = async () => {
  closeMenus();
  $('aboutUpdateNote').classList.add('hidden');
  aboutOverlay.classList.remove('hidden');
  // Релиз zapret могли сменить с прошлого открытия — перечитываем.
  fillVersions(await window.zapret.getVersions());
};
aboutOverlay.onclick = (e) => {
  if (e.target === aboutOverlay) aboutOverlay.classList.add('hidden');
};
// Проблемы с Klutz — к нам, а не к Flowseal: его проекты тут ни при чём.
const KLUTZ_REPO_URL = 'https://github.com/vbu00/zapret-klutz';
$('aboutGithubBtn').onclick = () => window.zapret.openExternalUrl(KLUTZ_REPO_URL);
$('aboutReportBtn').onclick = () => window.zapret.openExternalUrl(`${KLUTZ_REPO_URL}/issues`);
// Сравнение версий по частям: 1.10 новее 1.9, «1.9.9c» новее «1.9.9».
// Одного равенства мало — сборка новее опубликованной выглядела бы
// «устаревшей». Хвост через дефис — пререлиз по semver: «1.4.0-beta.1»
// СТАРШЕ 1.3.0, но младше 1.4.0, иначе бете не предложат выпуск.
function cmpVer(a, b) {
  const split = (v) => {
    const s = String(v || '').trim().replace(/^v/i, '').toLowerCase();
    const i = s.indexOf('-');
    return i < 0 ? [s, ''] : [s.slice(0, i), s.slice(i + 1)];
  };
  const [ca, pa] = split(a);
  const [cb, pb] = split(b);
  const core = cmpParts(ca, cb);
  if (core || pa === pb) return core;
  if (!pa) return 1;
  if (!pb) return -1;
  return cmpParts(pa, pb);
}

function cmpParts(a, b) {
  const parts = (v) => v.match(/\d+|[a-z]+/g) || [];
  const pa = parts(a);
  const pb = parts(b);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i];
    const y = pb[i];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    const nx = /^\d+$/.test(x);
    const ny = /^\d+$/.test(y);
    if (nx && ny) {
      const d = Number(x) - Number(y);
      if (d) return Math.sign(d);
    } else if (x !== y) {
      return nx ? 1 : ny ? -1 : x < y ? -1 : 1;
    }
  }
  return 0;
}

const isNewer = (u) => !!(u && u.current && u.latest && cmpVer(u.latest, u.current) > 0);
const KLUTZ_RELEASES_URL = `${KLUTZ_REPO_URL}/releases/latest`;

// Сверяет Klutz и обе встроенные части с последними версиями на GitHub,
// результат прямо в строках окна.
$('aboutUpdateBtn').onclick = async () => {
  const btn = $('aboutUpdateBtn');
  btn.disabled = true;
  btn.textContent = 'Проверяю…';
  const res = await callSafe(window.zapret.checkComponentUpdates());
  btn.disabled = false;
  btn.textContent = 'Проверить обновления';
  if (!res || !res.klutz) {
    showToast('Не удалось проверить обновления', 'error', { body: res && res.error });
    return;
  }

  const show = (el, u) => {
    el.className = '';
    if (!u.current) {
      el.textContent = 'не загружен';
    } else if (!u.latest) {
      el.textContent = `${u.current} · не удалось проверить`;
    } else if (isNewer(u)) {
      el.textContent = `${u.current} → есть ${u.latest}`;
      el.className = 'upd';
    } else {
      el.textContent = `${u.current} · актуальная`;
      el.className = 'fresh';
    }
  };
  show($('aboutKlutzVer'), res.klutz);
  show($('aboutZapretVer'), res.zapret);
  show($('aboutTgwsVer'), res.tgws);
  $('aboutBtn').classList.toggle('has-update', isNewer(res.klutz));

  const note = $('aboutUpdateNote');
  const parts = [];
  if (isNewer(res.klutz)) parts.push(`<button class="link-btn" id="aboutGetKlutz">Скачать Klutz ${esc(res.klutz.latest)} →</button>`);
  if (isNewer(res.zapret)) parts.push('<button class="link-btn" id="aboutGoUpdateZapret">Обновить zapret в Настройках →</button>');
  if (isNewer(res.tgws)) parts.push('<span>Новый TgWsProxy придёт с обновлением Klutz — он встроен в приложение.</span>');
  const all = [res.klutz, res.zapret, res.tgws];
  if (!all.some(isNewer) && all.every((u) => u.latest)) parts.push('<span>Всё актуально.</span>');
  note.innerHTML = parts.join('<br>');
  note.classList.toggle('hidden', !parts.length);
  const getKlutz = $('aboutGetKlutz');
  // То же окно, что и при проверке по расписанию: что нового и установить.
  if (getKlutz) {
    getKlutz.onclick = () => {
      aboutOverlay.classList.add('hidden');
      showKlutzUpdate(true);
    };
  }
  const go = $('aboutGoUpdateZapret');
  if (go) {
    go.onclick = () => {
      aboutOverlay.classList.add('hidden');
      switchPage('settings');
      scrollToCard('settingsMaintCard');
      $('checkUpdatesBtn').click();
    };
  }
};

// Раз в сутки при запуске Klutz сам смотрит, не вышла ли новая версия.
// Чаще незачем, да и GitHub ограничивает анонимные запросы. Найденную
// версию запоминаем, чтобы точка на «О программе» не пропадала до обновления.
// ─────────── Что нового ───────────
//
// Самообновления нет — новая версия ставится поверх, — и о том, что в ней
// изменилось, человек узнавал разве что случайно. Запоминаем версию, с
// которой Klutz открывали в прошлый раз; сменилась — показываем окно один раз.
const LAST_RUN_VERSION_KEY = 'klutzLastRunVersion';

async function maybeShowWhatsNew(onboardingDone) {
  const v = await window.zapret.getVersions();
  let last = null;
  try {
    last = localStorage.getItem(LAST_RUN_VERSION_KEY);
    localStorage.setItem(LAST_RUN_VERSION_KEY, v.app);
  } catch {}
  if (last === v.app) return;
  // Самая первая установка: рассказывать «что нового» не о чем. А вот если
  // Klutz уже настроен, но версия не записана — это обновление с версии, где
  // этого окна ещё не было.
  if (!last && !onboardingDone) return;
  let sections = [];
  try {
    sections = await window.zapret.getWhatsNew(last);
  } catch {
    return;
  }
  if (!sections.length) return;
  $('whatsNewTitle').textContent = `Klutz обновился до ${v.app}`;
  $('whatsNewList').innerHTML = sections
    .map(
      (s) =>
        (sections.length > 1 ? `<h4>${esc(s.version === 'Не выпущено' ? v.app : s.version)}</h4>` : '') +
        s.items.map((i) => `<details><summary>${esc(i.title)}</summary><p>${esc(i.text)}</p></details>`).join('')
    )
    .join('');
  $('whatsNewOverlay').classList.remove('hidden');
}

$('whatsNewOkBtn').onclick = () => $('whatsNewOverlay').classList.add('hidden');
$('whatsNewChangelogBtn').onclick = () => window.zapret.openExternalUrl(`${KLUTZ_REPO_URL}/blob/main/CHANGELOG.md`);

const KLUTZ_CHECKED_KEY = 'klutzUpdateCheckedAt';
const KLUTZ_SEEN_KEY = 'klutzLatestSeen';

async function checkKlutzUpdateDaily() {
  const v = await window.zapret.getVersions();
  let last = 0;
  let seen = null;
  try {
    last = Number(localStorage.getItem(KLUTZ_CHECKED_KEY)) || 0;
    seen = localStorage.getItem(KLUTZ_SEEN_KEY);
  } catch {}
  if (seen && cmpVer(seen, v.app) > 0) $('aboutBtn').classList.add('has-update');
  if (Date.now() - last < 24 * 3600 * 1000) return;

  const u = await window.zapret.checkKlutzUpdate();
  if (!u || !u.latest) return; // нет сети — попробуем при следующем запуске
  try {
    localStorage.setItem(KLUTZ_CHECKED_KEY, String(Date.now()));
    localStorage.setItem(KLUTZ_SEEN_KEY, u.latest);
  } catch {}
  $('aboutBtn').classList.toggle('has-update', isNewer(u));
  if (isNewer(u)) showKlutzUpdate(false);
}

// ─────────── Вышла новая версия Klutz ───────────
//
// Раньше было уведомление со ссылкой на страницу релиза — дальше человек сам
// искал нужный файл, качал и запускал. Теперь окно показывает, что в новой
// версии, и ставит её в одно нажатие. «Позже» запоминает версию, чтобы окно
// не всплывало заново; точка на «О программе» остаётся.
const KLUTZ_DISMISSED_KEY = 'klutzUpdateDismissed';

async function showKlutzUpdate(force) {
  let rel;
  try {
    rel = await window.zapret.getKlutzRelease();
  } catch {
    if (force) window.zapret.openExternalUrl(KLUTZ_RELEASES_URL);
    return;
  }
  const v = await window.zapret.getVersions();
  if (cmpVer(rel.version, v.app) <= 0) return;
  let dismissed = null;
  try {
    dismissed = localStorage.getItem(KLUTZ_DISMISSED_KEY);
  } catch {}
  if (!force && dismissed === rel.version) return;

  $('klutzUpdateTitle').textContent = `Вышел Klutz ${rel.version}`;
  $('klutzUpdateNotes').textContent =
    `У тебя ${v.app}. Новая версия ставится поверх, настройки сохранятся.\n\n` +
    (rel.notes || 'Что изменилось — на странице релиза.');
  const progress = $('klutzUpdateProgress');
  progress.classList.add('hidden');
  const install = $('klutzUpdateInstallBtn');
  install.disabled = false;
  install.classList.toggle('hidden', !rel.asset);
  $('klutzUpdateOverlay').classList.remove('hidden');

  $('klutzUpdateLaterBtn').onclick = () => {
    try {
      localStorage.setItem(KLUTZ_DISMISSED_KEY, rel.version);
    } catch {}
    $('klutzUpdateOverlay').classList.add('hidden');
  };
  $('klutzUpdatePageBtn').onclick = () => window.zapret.openExternalUrl(rel.url || KLUTZ_RELEASES_URL);
  install.onclick = async () => {
    install.disabled = true;
    progress.classList.remove('hidden');
    progress.textContent = 'Скачиваю…';
    const mb = (b) => (b / 1024 / 1024).toFixed(1);
    const off = window.zapret.onKlutzUpdateProgress((pct) => {
      progress.textContent =
        pct >= 100
          ? 'Скачано, запускаю установщик…'
          : `Скачиваю… ${pct}%` + (rel.size ? ` · ${mb((rel.size * pct) / 100)} из ${mb(rel.size)} МБ` : '');
    });
    const res = await callSafe(window.zapret.installKlutzUpdate());
    off();
    if (res && res.ok === false) {
      install.disabled = false;
      progress.textContent = res.error || 'Не удалось скачать — открой страницу релиза.';
      return;
    }
    progress.textContent = 'Установщик запущен — Klutz сейчас закроется.';
  };
}
// Не в первые секунды: при старте и так идут проверка связи и подъём прокси.
setTimeout(checkKlutzUpdateDaily, 15000);

// ─────────── Главная: «Что обходим» и карточки автоматизации ───────────
// These proxy the real controls elsewhere (Стратегии, Telegram, Настройки)
// instead of duplicating their logic — clicking here just drives the same
// handler, so there's exactly one place that owns each action.

$('engineZapretToggle').onclick = async () => {
  if (currentState.running) await stopActive();
  else $('heroStartBtn').click();
};
$('engineZapretLink').onclick = () => {
  switchPage('strategies');
  switchSubtab('configs');
};

$('engineTgLink').onclick = () => switchPage('telegram');
// tgwsproxyToggle's own handler already refreshes this card via
// loadTgwsproxyStatus() at the end — nothing else to do here.
$('engineTgToggle').onclick = () => tgwsproxyToggle.onclick();

$('homeAutostartToggle').onclick = async () => {
  await autostartToggle.onclick();
  $('homeAutostartToggle').classList.toggle('on', autostartToggle.classList.contains('on'));
};
$('homeAutoSwitchToggle').onclick = async () => {
  await autoSwitchToggle.onclick();
  $('homeAutoSwitchToggle').classList.toggle('on', autoSwitchToggle.classList.contains('on'));
};

// ─────────── Список конфигов ───────────

const configListEl = $('configList');

// Поля поиска по конфигам больше нет — отбор делают чипы семейств.

function closeMenus() {
  document.querySelectorAll('.menu').forEach((m) => m.remove());
  $('engineMenu').classList.add('hidden');
}
document.addEventListener('click', (e) => {
  if (
    !e.target.closest('.menu') &&
    !e.target.closest('.cfg-more') &&
    !e.target.closest('.game-select-panel') &&
    !e.target.closest('.game-select-btn') &&
    !e.target.closest('.engine-menu') &&
    !e.target.closest('.sb-version')
  ) {
    closeMenus();
  }
});

// Строка версии в сайдбаре открывает меню релиза — те же действия, что и в
// «Обслуживании», просто под рукой.
$('sidebarVersion').onclick = () => {
  const menu = $('engineMenu');
  if (!menu.classList.contains('hidden')) {
    menu.classList.add('hidden');
    return;
  }
  closeMenus();
  const r = $('sidebarVersion').getBoundingClientRect();
  menu.classList.remove('hidden');
  menu.style.left = `${r.left}px`;
  menu.style.bottom = `${window.innerHeight - r.top + 6}px`;
};
$('engineCheckUpdateBtn').onclick = () => {
  closeMenus();
  switchPage('settings');
  scrollToCard('settingsMaintCard');
  $('checkUpdatesBtn').click();
};
$('engineChangeBtn').onclick = () => {
  closeMenus();
  $('changeReleaseBtn2').click();
};
$('engineOpenFolderBtn').onclick = () => {
  closeMenus();
  $('openReleaseFolderBtn').click();
};

// Scrolls a card into view inside .content and flashes a highlight ring —
// same "jump to the thing I'm talking about" pattern for cross-page links.
function scrollToCard(id) {
  setTimeout(() => {
    const el = $(id);
    if (!el) return;
    el.scrollIntoView({ behavior: 'smooth', block: 'center' });
    el.classList.remove('jump-highlight');
    void el.offsetWidth; // restart the animation if it's already mid-flash
    el.classList.add('jump-highlight');
    setTimeout(() => el.classList.remove('jump-highlight'), 1800);
  }, 60);
}

document.querySelectorAll('.settings-anchor[data-jump]').forEach((el) => {
  el.onclick = () => scrollToCard(el.dataset.jump);
});

function openRowMenu(anchor, name, isActive) {
  closeMenus();
  const menu = document.createElement('div');
  menu.className = 'menu';

  const items = [];
  // Во время прогона скрипт тестов сам поднимает и гасит winws под каждый
  // конфиг. Кнопка «Включить» в это время заблокирована, а меню — нет, и
  // запуск отсюда посреди прогона ломал тесты.
  if (testing) {
    items.push({ label: 'Идёт прогон тестов — дождись окончания', disabled: true });
  } else if (!isActive) {
    items.push({ label: 'Запустить разово', fn: () => applyConfig(name, false) });
    items.push({ label: 'Установить службой', fn: () => applyConfig(name, true) });
  } else {
    if (!currentState.installedAsService) {
      items.push({ label: 'Перевести в службу', fn: () => applyConfig(name, true) });
    }
    items.push({ label: 'Остановить', danger: true, fn: stopActive });
  }

  for (const it of items) {
    const el = document.createElement('div');
    el.className = 'menu-item' + (it.danger ? ' danger' : '') + (it.disabled ? ' disabled' : '');
    el.textContent = it.label;
    if (!it.disabled) {
      el.onclick = () => {
        closeMenus();
        it.fn();
      };
    }
    menu.appendChild(el);
  }

  document.body.appendChild(menu);
  const r = anchor.getBoundingClientRect();
  const h = menu.offsetHeight;
  menu.style.left = `${Math.max(8, r.right - menu.offsetWidth)}px`;
  menu.style.top = `${r.bottom + h + 8 > window.innerHeight ? r.top - h - 6 : r.bottom + 6}px`;
}

function renderConfigList() {
  if (!currentState.configs.length) {
    configListEl.innerHTML = '';
    $('groupChips').innerHTML = '';
    return;
  }

  renderGroupChips();

  const filtered = currentState.configs;

  const groups = new Map();
  for (const name of filtered) {
    const g = deriveGroup(name);
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(name);
  }

  configListEl.innerHTML = '';

  for (const [group, names] of groups) {
    if (groupFilter && group !== groupFilter) continue;

    const wrap = document.createElement('div');
    wrap.className = 'cfg-group';

    // Группы больше не сворачиваются — фильтр делают чипы сверху.
    const head = document.createElement('div');
    head.className = 'cfg-group-head';
    head.innerHTML = `
      <span class="cgh-name">${esc(group)}</span>
      <span class="cgh-rule"></span>
      <span class="cgh-count">${names.length} ${plural(names.length, 'конфиг', 'конфига', 'конфигов')}</span>`;
    wrap.appendChild(head);

    {
      for (const name of names) {
        const isActive = currentState.activeConfig === name && currentState.running;
        const row = document.createElement('div');
        row.className = 'cfg-row' + (isActive ? ' active' : '');

        // Плашку «не активен» убрали: ею была подписана каждая строка, кроме
        // одной, — шум. У запущенной строка и так зелёная, а вот способ
        // запуска (служба или разово) нигде больше не виден, его оставляем.
        const runTag = isActive
          ? `<span class="cfg-tag on">${currentState.installedAsService ? 'служба Windows' : 'запущен разово'}</span>`
          : '';

        const bareName = name.replace(/\.bat$/i, '');
        const testedRow = lastResultsCache?.rows.find((r) => r.config.replace(/\.bat$/i, '') === bareName);
        const verdict = testedRow
          ? (() => {
              const score = verdictFor(testedRow, lastResultsCache.mode).score;
              return `<span class="cfg-tag" style="color:${verdictColor(score)}" title="Доля проверенных целей, которые ответили">${Math.round(score * 100)}%</span>`;
            })()
          : '';

        const btnLabel = isActive ? 'Активен' : currentState.running ? 'Переключить' : 'Включить';

        row.innerHTML = `
          <div class="cfg-main">
            <div class="cfg-title-row">
              <span class="cfg-name">${esc(displayName(name))}</span>
              ${verdict}
              ${runTag}
            </div>
            <div class="cfg-desc">${esc(name)}</div>
          </div>
          <div class="cfg-actions">
            <button class="cfg-btn${isActive ? ' on' : ''}" ${isActive ? 'disabled' : ''}>${btnLabel}</button>
            <button class="cfg-more" title="Ещё">⋯</button>
          </div>`;

        const btn = row.querySelector('.cfg-btn');
        if (!isActive) {
          btn.disabled = testing;
          btn.onclick = async () => {
            btn.disabled = true;
            await applyConfig(name, false);
            btn.disabled = false;
          };
        }
        row.querySelector('.cfg-more').onclick = (e) => {
          e.stopPropagation();
          openRowMenu(e.currentTarget, name, isActive);
        };

        wrap.appendChild(row);
      }
    }

    configListEl.appendChild(wrap);
  }
}

function plural(n, one, few, many) {
  const m10 = n % 10;
  const m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 10 || m100 >= 20)) return few;
  return many;
}

// Ряд чипов-фильтров по семействам конфигов — как в макете, вместо кнопки,
// перебирающей группы по кругу без обратной связи, какая выбрана.
function renderGroupChips() {
  const counts = new Map();
  for (const name of currentState.configs) {
    const g = deriveGroup(name);
    counts.set(g, (counts.get(g) || 0) + 1);
  }
  // Семейства из прежнего релиза в новом может не быть — тогда фильтр по нему
  // оставлял пустой список без единого подсвеченного чипа.
  if (groupFilter && !counts.has(groupFilter)) groupFilter = null;
  // Без «· N»: количество и так стоит в заголовке каждой группы ниже, а в
  // чипе оно делало ряд длинным и пёстрым.
  const chips = [{ label: 'Все', value: null }, ...[...counts.keys()].map((g) => ({ label: g, value: g }))];
  $('groupChips').innerHTML = chips
    .map((c) => `<div class="group-chip${c.value === groupFilter ? ' active' : ''}" data-g="${esc(c.value || '')}">${esc(c.label)}</div>`)
    .join('');
  $('groupChips')
    .querySelectorAll('.group-chip')
    .forEach((el) => {
      el.onclick = () => {
        groupFilter = el.dataset.g || null;
        renderConfigList();
      };
    });
}

// ─────────── Цели: главная и диагностика ───────────

// Discord/YouTube — «цели обхода», всё прочее — игровые сервисы.
const CORE_RE = /^(discord|youtube)/i;

let knownTargets = [];
let targetsLoaded = false;
let autoCheckTimer = null;

// Бэкенд теперь говорит не только «не отвечает», но и ГДЕ режут: пробует тот
// же адрес с заведомо чистым именем и сравнивает. Показываем это вместо
// одинакового «Нет связи» на все случаи жизни.
const PATH_LABEL = {
  cutoff: 'Рвут поток',
  ip: 'Блок по адресу',
  server: 'Отказ сервера',
  sni: 'Режут по имени',
  legal: 'Блок по закону',
  unknown: 'Не измерено',
};

function verdict(t) {
  if (t.pending) return { cls: 'idle', text: '…' };
  if (!t.ok) return { cls: 'bad', text: PATH_LABEL[t.verdict] || 'Нет связи' };
  // Пинг теперь честный — одно TCP-рукопожатие. Прежние 500 мс были порогом
  // для всего запроса целиком; для пинга 300 — уже заметно медленно.
  if (t.ms >= 300) return { cls: 'warn', text: 'Медленно' };
  return { cls: '', text: 'ОК' };
}

// Одна фраза про то, лечится ли происходящее сменой стратегии. Самое важное,
// что здесь можно сказать человеку: перебирать варианты или это бесполезно.
function pathNote(targets) {
  const failed = (targets || []).filter((t) => !t.ok);
  if (!failed.length) return '';
  const every = (v) => failed.every((t) => t.verdict === v);
  if (every('legal')) {
    return 'Сервер отвечает 451 «недоступно по юридическим причинам» — это не DPI, ' +
      'и сменой стратегии такое не лечится.';
  }
  if (every('ip')) {
    return 'С нейтральным именем те же адреса тоже молчат — режут адрес, а не имя. ' +
      'Обход такое не обходит: поможет другой адрес или туннель.';
  }
  if (every('server')) {
    return 'Отвечает сам сервер и отказывает по своей политике — это не блокировка, ' +
      'и стратегия тут ни при чём.';
  }
  if (every('cutoff')) {
    return 'Соединение поднимается и отдаёт данные, а потом его обрывают — режут не имя, ' +
      'а уже установленный поток. Сменой стратегии это обычно не лечится.';
  }
  if (failed.some((t) => t.verdict === 'sni')) {
    return 'Режут по имени — ровно то, что обход умеет обходить. Обычно помогает другой вариант.';
  }
  return '';
}

function targetRow(t) {
  const v = verdict(t);
  const sub = `${t.host}:${t.port}`;
  const tip = [
    t.why,
    t.ok && t.totalMs ? `Пинг — TCP-рукопожатие. Полный ответ сервера: ${t.totalMs} мс` : '',
  ]
    .filter(Boolean)
    .join('\n');
  return `
    <div class="trow"${tip ? ` title="${esc(tip)}"` : ''}>
      <div class="tr-name">
        <span class="tr-dot ${v.cls}"></span><span>${esc(t.name)}</span>
      </div>
      <div class="tr-host">${esc(sub)}</div>
      <div class="tr-ms">${t.pending ? '—' : t.ok ? t.ms + ' ms' : '—'}</div>
      <div class="tr-verdict ${v.cls}">${v.text}</div>
    </div>`;
}

function pingCard(t) {
  const v = verdict(t);
  return `
    <div class="ping-card">
      <div class="pc-top">
        <span class="pc-name">${esc(t.name)}</span>
        <span class="pc-dot ${v.cls}"></span>
      </div>
      <div class="pc-val">
        <span class="pc-num ${v.cls}">${t.pending ? '…' : t.ok ? t.ms : 'нет'}</span>
        ${t.ok && !t.pending ? '<span class="pc-unit">ms</span>' : ''}
      </div>
      <div class="pc-host">${esc(t.host)}</div>
    </div>`;
}

function renderTargets(data) {
  const core = data.targets.filter((t) => CORE_RE.test(t.name));
  const games = data.targets.filter((t) => !CORE_RE.test(t.name));

  $('diagCoreRows').innerHTML = core.map(targetRow).join('');
  $('diagGameGrid').innerHTML = games.map(pingCard).join('');

  const coreOk = core.filter((t) => t.ok).length;
  const summary = core.length ? `${coreOk} из ${core.length} целей отвечают` : 'нет целей';
  $('diagCoreSummary').textContent = data.pending ? 'проверяю…' : summary;

  // Карточки «Здоровье связи» на Главной больше нет — состояние целей несут
  // сам герой (заголовок в простом режиме, строка «Цели» в продвинутом).
  if (!data.pending) {
    const when = new Date(data.checkedAt).toLocaleTimeString('ru-RU');
    const strat = data.running && data.strategy ? displayName(data.strategy) : null;
    $('gamesContext').textContent = strat
      ? `${when} · активна ${strat}`
      : `${when} · стратегия не запущена`;
  }

  // Custom-address rows show live ping status from this same check.
  if (knownTargets.length) renderCustomAddressList();
}

async function checkTargets() {
  const btns = [$('checkGamesBtn')];
  btns.forEach((b) => (b.disabled = true));
  if (knownTargets.length) {
    renderTargets({ targets: knownTargets.map((t) => ({ ...t, pending: true })), pending: true, checkedAt: Date.now() });
  }
  const data = await callSafe(window.zapret.checkGames());
  btns.forEach((b) => (b.disabled = false));
  if (data.ok) {
    lastCheck = data;
    renderTargets(data);
    if (activePage === 'home') loadOverview();
  } else {
    // Строки стояли в «…», пока шла проверка. Не вышло — возвращаем прежний
    // результат, а не оставляем «…» навсегда.
    if (lastCheck) renderTargets(lastCheck);
    $('diagCoreSummary').textContent = 'проверка не удалась';
  }
  return data.ok ? data : null;
}

$('checkGamesBtn').onclick = checkTargets;

// После автопримения стратегии — реально проверяем, отвечают ли Discord и
// YouTube, вместо того чтобы слепо считать запуск успехом.
async function verifyAppliedAndToast(name) {
  const data = await checkTargets();
  const core = data ? data.targets.filter((t) => CORE_RE.test(t.name)) : [];
  const okCore = core.filter((t) => t.ok).length;
  pickFailed = core.length > 0 && okCore === 0;
  if (pickFailed) {
    showToast(`Включено: ${displayName(name)} — но Discord и YouTube не отвечают`, 'error');
  } else if (core.length && okCore < core.length) {
    showToast(`Включено: ${displayName(name)} — отвечают не все цели`, 'success');
  } else {
    showToast(`Включено: ${displayName(name)} — Discord и YouTube отвечают`, 'success');
  }
  renderHero();
}

async function ensureTargetsLoaded() {
  if (targetsLoaded) return;
  targetsLoaded = true;
  const res = await window.zapret.getGameTargets();
  knownTargets = res.targets || [];
  checkTargets();
}

const autoCheckToggle = $('autoCheckToggle');
const AUTO_CHECK_KEY = 'klutzAutoCheck';
function setAutoCheck(on) {
  autoCheckToggle.classList.toggle('on', on);
  if (autoCheckTimer) {
    clearInterval(autoCheckTimer);
    autoCheckTimer = null;
  }
  if (on) {
    autoCheckTimer = setInterval(() => {
      if (activePage === 'diagnostics' || activePage === 'home') checkTargets();
    }, 60000);
  }
  // Раньше тумблер забывался при каждом перезапуске Klutz.
  try {
    localStorage.setItem(AUTO_CHECK_KEY, on ? '1' : '0');
  } catch {}
}
autoCheckToggle.onclick = () => setAutoCheck(!autoCheckToggle.classList.contains('on'));
try {
  if (localStorage.getItem(AUTO_CHECK_KEY) === '1') setAutoCheck(true);
} catch {}

// ─────────── Свои адреса ───────────

let defaultGameTargets = [];
window.zapret.getDefaultGameTargets().then((res) => {
  defaultGameTargets = res.targets || [];
});
function isDefaultTarget(t) {
  return defaultGameTargets.some((d) => d.name === t.name && d.host === t.host && d.port === t.port);
}

async function loadGameTargetsArea() {
  const res = await window.zapret.getGameTargets();
  knownTargets = res.targets || [];
  renderCustomAddressList();
}

// Reuses whatever the last real ping check found for this exact host:port —
// no separate round trip just to colour the list.
function liveVerdictFor(t) {
  const live = lastCheck?.targets?.find((x) => x.host === t.host && x.port === t.port);
  return live ? { ...verdict(live), ms: live.ok ? live.ms + ' ms' : '—' } : { cls: 'idle', text: '—', ms: '—' };
}

// Показывает только реально добавленные пользователем адреса — стандартные
// уже видны выше, в «Целях обхода» и «Игровых сервисах», повторять их здесь
// смысла нет.
function renderCustomAddressList() {
  const custom = knownTargets.filter((t) => !isDefaultTarget(t));
  $('customAddressesCount').textContent = `${custom.length} ${plural(custom.length, 'адрес', 'адреса', 'адресов')}`;

  const box = $('customAddressList');
  if (!custom.length) {
    box.innerHTML = '<div class="no-results">Своих адресов пока нет — стандартные видны выше</div>';
    return;
  }

  box.innerHTML = custom
    .map((t) => {
      const v = liveVerdictFor(t);
      return `
        <div class="addr-row">
          <div class="addr-main">
            <span class="tr-dot ${v.cls}"></span>
            <span class="addr-name">${esc(t.name)}</span>
          </div>
          <div class="addr-host">${esc(t.host)}:${t.port}</div>
          <div class="addr-ms">${esc(v.ms)}</div>
          <div class="addr-verdict ${v.cls}">${esc(v.text)}</div>
          <div class="addr-remove"><button class="addr-remove-btn" data-remove="${esc(t.host)}:${t.port}" title="Удалить">✕</button></div>
        </div>`;
    })
    .join('');

  box.querySelectorAll('[data-remove]').forEach((btn) => {
    btn.onclick = async () => {
      const key = btn.dataset.remove;
      const before = knownTargets;
      const next = knownTargets.filter((t) => `${t.host}:${t.port}` !== key);
      const res = await window.zapret.saveGameTargets(next);
      if (!res.ok) {
        showToast(res.error || 'Не удалось сохранить', 'error');
        return;
      }
      knownTargets = res.targets;
      renderCustomAddressList();
      checkTargets();
      showToast('Адрес удалён', 'success', {
        actionLabel: 'Отменить',
        onAction: async () => {
          const undoRes = await window.zapret.saveGameTargets(before);
          if (undoRes.ok) {
            knownTargets = undoRes.targets;
            renderCustomAddressList();
            checkTargets();
          }
        },
      });
    };
  });
}

// ─────────── Командная палитра «добавить адрес» (Ctrl+K) ───────────

const CATALOG = [
  // Хост — это то, что реально проверяется на связь, поэтому здесь стоят
  // рабочие адреса сервисов, а не сайты-витрины: витрина может открываться
  // и тогда, когда игра не заходит. `a` — синонимы для поиска: как сервис
  // называют вслух и по-русски.
  { g: 'Игры', name: 'Counter-Strike 2', host: 'cm.steampowered.com', port: 27017, a: 'cs2 кс контра ксго csgo' },
  { g: 'Игры', name: 'Dota 2', host: 'api.steampowered.com', port: 443, a: 'дота dota' },
  { g: 'Игры', name: 'Valorant', host: 'glz-ru-1.ru.a.pvp.net', port: 443, a: 'валорант вало' },
  { g: 'Игры', name: 'League of Legends', host: 'euw.api.riotgames.com', port: 443, a: 'лол lol лига' },
  { g: 'Игры', name: 'Fortnite', host: 'fortnite-public-service-prod11.ol.epicgames.com', port: 443, a: 'фортнайт фн' },
  { g: 'Игры', name: 'Apex Legends', host: 'r5-crossplay.r5prod.stryder.respawn.com', port: 443, a: 'апекс' },
  { g: 'Игры', name: 'Overwatch 2', host: 'eu.actual.battle.net', port: 1119, a: 'овервотч ow' },
  { g: 'Игры', name: 'Rocket League', host: 'api.rlpp.psynet.gg', port: 443, a: 'ракетлига рл' },
  { g: 'Игры', name: 'Roblox', host: 'apis.roblox.com', port: 443, a: 'роблокс' },
  { g: 'Игры', name: 'Minecraft', host: 'sessionserver.mojang.com', port: 443, a: 'майнкрафт майн mojang' },
  { g: 'Игры', name: 'Genshin Impact', host: 'sdk-os-static.hoyoverse.com', port: 443, a: 'геншин хойо hoyoverse' },
  { g: 'Игры', name: 'Honkai: Star Rail', host: 'api-os-takumi.hoyoverse.com', port: 443, a: 'хонкай хср hsr' },
  { g: 'Игры', name: 'PUBG', host: 'api.pubg.com', port: 443, a: 'пабг пубг' },
  { g: 'Игры', name: 'Escape from Tarkov', host: 'prod.escapefromtarkov.com', port: 443, a: 'тарков eft' },
  { g: 'Игры', name: 'GTA Online', host: 'prod.ros.rockstargames.com', port: 443, a: 'гта рокстар rockstar' },
  { g: 'Игры', name: 'Call of Duty', host: 'profile.callofduty.com', port: 443, a: 'колда cod warzone варзон' },
  { g: 'Игры', name: 'Destiny 2', host: 'www.bungie.net', port: 443, a: 'дестини bungie' },
  { g: 'Игры', name: 'Warframe', host: 'api.warframe.com', port: 443, a: 'варфрейм' },
  { g: 'Игры', name: 'War Thunder', host: 'login.gaijin.net', port: 443, a: 'вартандер гайдзин gaijin' },
  { g: 'Игры', name: 'Path of Exile', host: 'www.pathofexile.com', port: 443, a: 'поэ poe' },
  { g: 'Игры', name: 'Dead by Daylight', host: 'latest.live.bhvrdbd.com', port: 443, a: 'дбд dbd' },
  { g: 'Игры', name: 'Rust', host: 'api.facepunch.com', port: 443, a: 'раст facepunch' },
  { g: 'Игры', name: 'osu', host: 'osu.ppy.sh', port: 443, a: 'осу' },

  { g: 'Платформы', name: 'Steam', host: 'api.steampowered.com', port: 443, a: 'стим' },
  { g: 'Платформы', name: 'Steam Community', host: 'steamcommunity.com', port: 443, a: 'стим комьюнити профиль' },
  { g: 'Платформы', name: 'Steam Store', host: 'store.steampowered.com', port: 443, a: 'стим магазин' },
  { g: 'Платформы', name: 'Epic Online', host: 'api.epicgames.dev', port: 443, a: 'эпик epic' },
  { g: 'Платформы', name: 'Riot', host: 'auth.riotgames.com', port: 443, a: 'риот' },
  { g: 'Платформы', name: 'Battle.net', host: 'us.actual.battle.net', port: 1119, a: 'близзард blizzard батлнет' },
  { g: 'Платформы', name: 'Xbox Live', host: 'title.mgt.xboxlive.com', port: 443, a: 'иксбокс хбокс' },
  { g: 'Платформы', name: 'PlayStation Network', host: 'auth.api.sonyentertainmentnetwork.com', port: 443, a: 'плейстейшн псн psn sony' },
  { g: 'Платформы', name: 'Nintendo', host: 'accounts.nintendo.com', port: 443, a: 'нинтендо свитч switch' },
  { g: 'Платформы', name: 'EA App', host: 'accounts.ea.com', port: 443, a: 'еа origin ориджин' },
  { g: 'Платформы', name: 'Ubisoft Connect', host: 'public-ubiservices.ubi.com', port: 443, a: 'юбисофт uplay юплей' },
  { g: 'Платформы', name: 'GOG', host: 'www.gog.com', port: 443, a: 'гог' },
  { g: 'Платформы', name: 'itch.io', host: 'itch.io', port: 443, a: 'итч' },

  { g: 'Сервисы', name: 'Twitch', host: 'gql.twitch.tv', port: 443, a: 'твич' },
  { g: 'Сервисы', name: 'Telegram API', host: 'api.telegram.org', port: 443, a: 'телеграм тг' },
  { g: 'Сервисы', name: 'Instagram', host: 'i.instagram.com', port: 443, a: 'инстаграм инста' },
  { g: 'Сервисы', name: 'Facebook', host: 'graph.facebook.com', port: 443, a: 'фейсбук фб' },
  { g: 'Сервисы', name: 'X (Twitter)', host: 'api.x.com', port: 443, a: 'твиттер икс twitter' },
  { g: 'Сервисы', name: 'TikTok', host: 'www.tiktok.com', port: 443, a: 'тикток' },
  { g: 'Сервисы', name: 'Reddit', host: 'www.reddit.com', port: 443, a: 'реддит' },
  { g: 'Сервисы', name: 'Spotify', host: 'api.spotify.com', port: 443, a: 'спотифай' },
  { g: 'Сервисы', name: 'SoundCloud', host: 'api-v2.soundcloud.com', port: 443, a: 'саундклауд' },
  { g: 'Сервисы', name: 'Netflix', host: 'www.netflix.com', port: 443, a: 'нетфликс' },
  { g: 'Сервисы', name: 'Signal', host: 'chat.signal.org', port: 443, a: 'сигнал' },
  { g: 'Сервисы', name: 'WhatsApp', host: 'web.whatsapp.com', port: 443, a: 'вотсап ватсап' },
  { g: 'Сервисы', name: 'Zoom', host: 'zoom.us', port: 443, a: 'зум' },
  { g: 'Сервисы', name: 'Slack', host: 'slack.com', port: 443, a: 'слак' },
  { g: 'Сервисы', name: 'Figma', host: 'www.figma.com', port: 443, a: 'фигма' },
  { g: 'Сервисы', name: 'Notion', host: 'www.notion.so', port: 443, a: 'ноушен' },
  { g: 'Сервисы', name: 'GitHub', host: 'api.github.com', port: 443, a: 'гитхаб гит' },
  { g: 'Сервисы', name: 'npm', host: 'registry.npmjs.org', port: 443, a: 'нпм' },
  { g: 'Сервисы', name: 'PyPI', host: 'pypi.org', port: 443, a: 'пипи питон' },
  { g: 'Сервисы', name: 'Docker Hub', host: 'registry-1.docker.io', port: 443, a: 'докер' },
  { g: 'Сервисы', name: 'Hugging Face', host: 'huggingface.co', port: 443, a: 'хаггинг' },
  { g: 'Сервисы', name: 'ChatGPT', host: 'chatgpt.com', port: 443, a: 'чатгпт гпт openai' },
  { g: 'Сервисы', name: 'Claude', host: 'api.anthropic.com', port: 443, a: 'клод anthropic' },
  { g: 'Сервисы', name: 'Proton Mail', host: 'mail.proton.me', port: 443, a: 'протон' },
  { g: 'Сервисы', name: 'Cloudflare 1.1.1.1', host: 'one.one.one.one', port: 443, a: 'клаудфлер dns днс' },
];

let cmdItems = [];
let cmdIdx = 0;

// Принимает и «host:port», и полноценный URL — имя предлагает по домену.
function parseTarget(raw) {
  const s = (raw || '').trim();
  if (!s) return null;
  let host;
  let port;
  try {
    const u = new URL(/^[a-z]+:\/\//i.test(s) ? s : `https://${s}`);
    host = u.hostname;
    port = Number(u.port) || (u.protocol === 'http:' ? 80 : 443);
  } catch {
    return null;
  }
  if (!host || !host.includes('.')) return null;
  const label = host.replace(/^(www|api|auth|cdn|gateway)\./, '').split('.').slice(0, -1).join('.') || host;
  return { host, port, suggested: label.charAt(0).toUpperCase() + label.slice(1) };
}

// Насколько строка каталога подходит запросу. Ноль — не подходит.
//
// Раньше это была подстрока по имени и хосту. Она молчала на «кс» и «дота»,
// как их и набирают, зато охотно ставила случайное совпадение в длинном
// служебном хосте выше точного совпадения по названию. Порядок здесь — от
// самого уверенного совпадения к самому случайному.
function matchScore(c, q) {
  const name = c.name.toLowerCase();
  const host = c.host.toLowerCase();
  const aliases = (c.a || '').toLowerCase().split(' ').filter(Boolean);
  if (name === q) return 100;
  if (name.startsWith(q)) return 90;
  // С начала слова: «legends» найдёт Apex Legends, «exile» — Path of Exile.
  if (name.split(/[\s(:.-]+/).some((w) => w.startsWith(q))) return 80;
  if (aliases.some((w) => w === q)) return 75;
  if (aliases.some((w) => w.startsWith(q))) return 70;
  if (name.includes(q)) return 60;
  if (host.startsWith(q)) return 50;
  if (host.includes(q)) return 40;
  return 0;
}

function buildCmdItems(query) {
  const q = query.trim().toLowerCase();
  const have = new Set(knownTargets.map((t) => `${t.host}:${t.port}`));
  const matched = q
    ? CATALOG.map((c) => ({ c, s: matchScore(c, q) }))
        .filter((x) => x.s > 0)
        .sort((a, b) => b.s - a.s || a.c.name.localeCompare(b.c.name, 'ru'))
        .map((x) => x.c)
    : CATALOG;
  const list = matched.map((c) => ({
    ...c,
    added: have.has(`${c.host}:${c.port}`),
  }));
  const p = q ? parseTarget(query) : null;
  if (p && !list.some((c) => c.host === p.host && c.port === p.port)) {
    list.push({
      g: 'Свой адрес',
      name: query.trim(),
      host: p.host,
      port: p.port,
      custom: true,
      suggested: p.suggested,
      added: have.has(`${p.host}:${p.port}`),
    });
  }
  return list;
}

function renderCmdList() {
  const box = $('cmdList');
  if (!cmdItems.length) {
    box.innerHTML =
      '<div class="cmd-empty">Ничего не нашёл. Вставь адрес вида <span class="mono">host:port</span> или ссылку — добавлю как свой.</div>';
    return;
  }
  const groups = new Map();
  cmdItems.forEach((it, i) => {
    if (!groups.has(it.g)) groups.set(it.g, []);
    groups.get(it.g).push({ it, i });
  });
  box.innerHTML = [...groups.entries()]
    .map(
      ([label, items]) => `
      <div class="cmd-group">${esc(label)}</div>
      ${items
        .map(
          ({ it, i }) => `
        <div class="cmd-item${i === cmdIdx ? ' active' : ''}" data-i="${i}">
          <span class="cmd-initial" data-host="${esc(it.host)}">${esc((it.custom ? it.suggested : it.name).charAt(0).toUpperCase())}</span>
          <div class="cmd-item-text">
            <div class="cmd-item-name">${esc(it.custom ? it.suggested : it.name)}</div>
            <div class="cmd-item-host">${esc(it.host)}:${it.port}</div>
          </div>
          <span class="cmd-tag">${it.added ? 'уже есть' : it.custom ? 'свой' : ''}</span>
        </div>`
        )
        .join('')}`
    )
    .join('');
  box.querySelectorAll('.cmd-item').forEach((el) => {
    el.onmouseenter = () => {
      cmdIdx = Number(el.dataset.i);
      box.querySelectorAll('.cmd-item').forEach((o) => o.classList.toggle('active', o === el));
    };
    el.onclick = () => cmdPick(cmdItems[Number(el.dataset.i)]);
  });
  const active = box.querySelector('.cmd-item.active');
  if (active) active.scrollIntoView({ block: 'nearest' });
  loadFavicons(box);
}

// Иконки сервисов.
//
// Только для строк, которые сейчас на экране, и по одной: каждая — поход в
// сеть через curl, а список перерисовывается на каждое нажатие клавиши.
// Ответ кладём в кэш процесса, поэтому повторный показ той же строки уже
// ничего не спрашивает; на диске кэш держит Rust, так что и следующий
// запуск программы обойдётся без сети.
const faviconCache = new Map(); // host -> data-URI, null (не нашлось) или Promise

// То, что пришло из сети, попадает в url() внутри style. Base64 не может
// содержать кавычек, но проверяем форму явно, а не полагаемся на это.
const SAFE_ICON = /^data:image\/[a-z.+-]+;base64,[A-Za-z0-9+/=]+$/;

function paintFavicon(host, uri) {
  if (!uri || !SAFE_ICON.test(uri)) return;
  document.querySelectorAll('.cmd-initial').forEach((slot) => {
    if (slot.dataset.host !== host) return;
    slot.style.backgroundImage = `url("${uri}")`;
    slot.classList.add('has-icon');
    slot.textContent = '';
  });
}

function loadFavicons(box) {
  box.querySelectorAll('.cmd-initial').forEach((slot) => {
    const host = slot.dataset.host;
    if (!host) return;
    const cached = faviconCache.get(host);
    if (cached === null || cached instanceof Promise) return;
    if (typeof cached === 'string') {
      paintFavicon(host, cached);
      return;
    }
    const p = window.zapret
      .getFavicon(host)
      .then((r) => {
        const uri = r && r.ok ? r.dataUri : null;
        faviconCache.set(host, uri);
        paintFavicon(host, uri);
      })
      .catch(() => {
        // Иконка — украшение: не нашлась, значит остаётся буква.
        faviconCache.set(host, null);
      });
    faviconCache.set(host, p);
  });
}

async function cmdPick(it) {
  if (!it) return;
  if (it.added) {
    showToast(`${it.custom ? it.suggested : it.name} уже в списке`, 'warn');
    return;
  }
  const name = it.custom ? it.suggested : it.name;
  const res = await window.zapret.saveGameTargets([...knownTargets, { name, host: it.host, port: it.port }]);
  if (!res.ok) {
    showToast(res.error || 'Не удалось сохранить', 'error');
    return;
  }
  knownTargets = res.targets;
  showToast(`Добавлено: ${name} — ${it.host}:${it.port}`, 'success');
  closeCmd();
  renderCustomAddressList();
  checkTargets();
}

function refreshCmd() {
  cmdItems = buildCmdItems($('cmdInput').value);
  if (cmdIdx >= cmdItems.length) cmdIdx = 0;
  renderCmdList();
  const custom = knownTargets.filter((t) => !isDefaultTarget(t)).length;
  $('cmdFoot').textContent = `${CATALOG.length} в каталоге · своих ${custom}`;
}

function openCmd() {
  cmdIdx = 0;
  $('cmdInput').value = '';
  $('cmdOverlay').classList.remove('hidden');
  refreshCmd();
  setTimeout(() => $('cmdInput').focus(), 30);
}

function closeCmd() {
  $('cmdOverlay').classList.add('hidden');
}

$('openCmdBtn').onclick = openCmd;
$('cmdInput').oninput = () => {
  cmdIdx = 0;
  refreshCmd();
};
$('cmdOverlay').onclick = (e) => {
  if (e.target === $('cmdOverlay')) closeCmd();
};
$('cmdInput').onkeydown = (e) => {
  if (e.key === 'ArrowDown') {
    e.preventDefault();
    cmdIdx = Math.min(cmdIdx + 1, cmdItems.length - 1);
    renderCmdList();
  } else if (e.key === 'ArrowUp') {
    e.preventDefault();
    cmdIdx = Math.max(cmdIdx - 1, 0);
    renderCmdList();
  } else if (e.key === 'Enter') {
    e.preventDefault();
    cmdPick(cmdItems[cmdIdx]);
  } else if (e.key === 'Escape') {
    closeCmd();
  }
};
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !aboutOverlay.classList.contains('hidden')) {
    aboutOverlay.classList.add('hidden');
    return;
  }
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
    e.preventDefault();
    if ($('cmdOverlay').classList.contains('hidden')) openCmd();
    else closeCmd();
  }
});

$('resetGamesBtn').onclick = async () => {
  const res = await window.zapret.resetGameTargets();
  knownTargets = res.targets;
  $('gamesMsg').textContent = 'Восстановлен список по умолчанию.';
  renderCustomAddressList();
  checkTargets();
};

// ─────────── Тесты ───────────

// Тестовый скрипт пишет два разных формата аналитики в зависимости от режима,
// поэтому определяем по содержимому, а не по тому, что запускали.
function parseResults(text) {
  const idx = text.indexOf('=== ANALYTICS ===');
  const block = idx >= 0 ? text.slice(idx) : text;
  const rows = [];
  let mode = 'standard';

  const stdRe = /^(.+?)\s*:\s*HTTP OK:\s*(\d+),\s*ERR:\s*(\d+),\s*UNSUP:\s*(\d+),\s*Ping OK:\s*(\d+),\s*Fail:\s*(\d+)\s*$/gm;
  let m;
  while ((m = stdRe.exec(block))) {
    rows.push({ config: m[1].trim(), ok: +m[2], err: +m[3], unsup: +m[4], pingOk: +m[5], pingFail: +m[6] });
  }

  if (!rows.length) {
    mode = 'dpi';
    const dpiRe = /^(.+?)\s*:\s*OK:\s*(\d+),\s*ERR:\s*(\d+),\s*UNSUP:\s*(\d+),\s*BLOCK(?:ED)?:\s*(\d+)\s*$/gm;
    while ((m = dpiRe.exec(block))) {
      rows.push({ config: m[1].trim(), ok: +m[2], err: +m[3], unsup: +m[4], blocked: +m[5] });
    }
  }

  if (!rows.length) return { rows: [], best: null, mode };

  if (mode === 'dpi') rows.sort((a, b) => b.ok - a.ok || a.blocked - b.blocked || a.err - b.err);
  else rows.sort((a, b) => b.ok - a.ok || b.pingOk - a.pingOk || a.err - b.err);

  // Отметка проверки «как у приложений»: лучший по тестам мог не пройти то,
  // что нужно Discord и YouTube, — тогда первым идёт тот, кто прошёл.
  const appBest = (block.match(/^# Klutz-app-best: (.+?)\s*$/m) || [])[1];
  if (appBest) {
    const bare = (s) => s.replace(/\.bat$/i, '');
    const i = rows.findIndex((r) => bare(r.config) === bare(appBest));
    if (i > 0) rows.unshift(rows.splice(i, 1)[0]);
  }

  return { rows, best: rows[0].config, mode };
}

// Раньше здесь были ещё и словесные уровни («Пробивает»/«Частично»/«Не
// пробивает») — убрали: сам процент точнее и не нуждается в переводе на
// три размытые категории. Цвет остаётся, чтобы шкала читалась с одного взгляда.
// «6 из 7» вместо голого числа: сколько целей прошла лучшая стратегия
// прогона и сколько их вообще было. Число целей задаёт релиз и режим, а не
// константа — раньше здесь стояла доля, умноженная на семь.
function bestOf(run) {
  if (!run || !run.bestTotal) return '—';
  return `${run.bestOk} из ${run.bestTotal}`;
}

function verdictColor(score) {
  return score >= 0.85 ? 'var(--green)' : score >= 0.4 ? 'var(--tx-3)' : 'var(--red-row)';
}

// Одна оценка "качества" вместо разрозненных чисел — доля целей, которые
// реально ответили, из всех, что этот прогон вообще проверял для этой
// стратегии (для DPI-режима "заблокировано" тоже считается неудачей).
function verdictFor(r, mode) {
  const total = mode === 'dpi' ? r.ok + r.err + r.unsup + r.blocked : r.ok + r.err + r.unsup;
  const score = total ? r.ok / total : 0;
  return { score, total, color: verdictColor(score) };
}

// Последняя колонка таблицы результатов: что произойдёт по клику на строку.
function resultActionLabel(config) {
  if (!currentState.running) return 'Включить';
  return currentState.activeConfig === config ? 'активен' : 'Переключить';
}

const GRID_RESULTS = 'minmax(0,1fr) 110px minmax(140px,1.2fr) 90px';
const GRID_RESULTS_DETAILED = 'minmax(0,1fr) 110px minmax(120px,1fr) 56px 56px 56px 90px';
const QUALITY_HINT = 'Доля проверенных целей, которые ответили';
let resultsDetailsOpen = false;
// So the Конфиги list can show each strategy's last verdict without a
// separate round trip — same data renderResults() already parsed.
let lastResultsCache = null;

function renderResults(text) {
  const { rows, best, mode } = parseResults(text);
  const head = $('resultsHead');
  const body = $('resultsBody');

  if (!rows.length) {
    $('resultsCard').classList.add('hidden');
    $('testsEmpty').classList.remove('hidden');
    return;
  }

  lastResultsCache = { rows, mode };
  renderConfigList();

  $('resultsCard').classList.remove('hidden');
  $('testsEmpty').classList.add('hidden');

  const grid = resultsDetailsOpen ? GRID_RESULTS_DETAILED : GRID_RESULTS;
  head.style.gridTemplateColumns = grid;
  const detailsBtn = `<button class="link-btn" id="resDetailsToggle">${resultsDetailsOpen ? 'Скрыть детали' : 'Детали'}</button>`;
  head.innerHTML = resultsDetailsOpen
    ? `<div>Конфиг</div><div title="${QUALITY_HINT}">Пройдено</div><div>Качество</div>` +
      '<div>HTTP</div><div>Ping</div><div>DPI</div>' +
      `<div style="text-align:right">${detailsBtn}</div>`
    : `<div>Конфиг</div><div title="${QUALITY_HINT}">Пройдено</div><div>Качество</div>` +
      `<div style="text-align:right">${detailsBtn}</div>`;

  body.innerHTML = '';
  for (const r of rows) {
    const isBest = r.config === best;
    const v = verdictFor(r, mode);
    const pct = Math.round(v.score * 100);
    const row = document.createElement('div');
    row.className = 'tbl-row' + (isBest ? ' best' : '');
    row.style.gridTemplateColumns = grid;

    const httpText = mode === 'dpi' ? '—' : `${r.ok}/${v.total}`;
    const pingText = mode === 'dpi' ? '—' : `${r.pingOk}/${r.pingOk + r.pingFail}`;
    const dpiText = mode === 'dpi' ? `${r.ok}/${v.total}` : '—';

    row.innerHTML =
      `<div class="tc-name"><span>${esc(displayName(r.config))}</span>${
        isBest ? '<span class="badge">Лучший</span>' : ''
      }</div>` +
      `<div style="color:${v.color};font-weight:600">${pct}%</div>` +
      `<div class="quality-cell"><div class="quality-bar"><div class="quality-fill" style="width:${pct}%;background:${v.color}"></div></div><span class="quality-num">${r.ok} из ${v.total} целей</span></div>` +
      (resultsDetailsOpen
        ? `<div class="tc-r" style="font-size:12.5px">${httpText}</div><div class="tc-r" style="font-size:12.5px">${pingText}</div><div class="tc-r" style="font-size:12.5px">${dpiText}</div>`
        : '') +
      `<div class="tc-r" style="color:var(--tx-4)">${esc(resultActionLabel(r.config))}</div>`;

    row.title = 'Применить эту стратегию';
    row.onclick = () => applyConfig(r.config, false);
    body.appendChild(row);
  }

  $('resDetailsToggle').onclick = (e) => {
    e.stopPropagation();
    resultsDetailsOpen = !resultsDetailsOpen;
    renderResults(text);
  };
}

// "Оба" режима — гоняет HTTP/Ping и DPI-checker одно за другим (два реальных
// прогона, не выдумка) и усредняет их доли по каждому конфигу. Итоговая


let testMode = 'standard';

$('testModeSwitch').querySelectorAll('.seg-btn').forEach((btn) => {
  btn.onclick = () => {
    if (testing) return;
    testMode = btn.dataset.mode;
    $('testModeSwitch').querySelectorAll('.seg-btn').forEach((b) => b.classList.toggle('active', b === btn));
  };
});

let autoApply = localStorage.getItem('zapretAutoApply') === '1';
let autoApplyHow = localStorage.getItem('zapretAutoApplyHow') === 'service' ? 'service' : 'run';

function renderAutoApply() {
  $('autoApplyToggle').classList.toggle('on', autoApply);
  $('autoApplyHow').classList.toggle('hidden', !autoApply);
  $('autoApplyNote').textContent = autoApply
    ? autoApplyHow === 'service'
      ? 'лучшая станет службой'
      : 'лучшая применится автоматически'
    : 'выключено';
  $('autoApplyHow')
    .querySelectorAll('.seg-btn')
    .forEach((b) => b.classList.toggle('active', b.dataset.how === autoApplyHow));
}

$('autoApplyToggle').onclick = () => {
  autoApply = !autoApply;
  localStorage.setItem('zapretAutoApply', autoApply ? '1' : '0');
  renderAutoApply();
};

$('autoApplyHow')
  .querySelectorAll('.seg-btn')
  .forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      autoApplyHow = b.dataset.how;
      localStorage.setItem('zapretAutoApplyHow', autoApplyHow);
      renderAutoApply();
    };
  });

renderAutoApply();

async function runAllTests(opts = {}) {
  if (testing) {
    showToast('Тесты уже идут', 'warn');
    return;
  }
  if (!(await ensureNoForeignService())) return;
  // Через VPN тесты меряют его сеть, а через системный прокси Discord и
  // браузеры ходят мимо того, что тесты проверяют. Сказать до того, как
  // человек поверит зелёным результатам.
  window.zapret
    .checkVpn()
    .then((v) => {
      if (v && v.blocked) {
        showToast('Похоже, сеть идёт через VPN или прокси', 'warn', {
          body: `${v.reasons.join('; ')}. Результаты тестов сейчас не о твоей сети или не о том, как ходит Discord.`,
        });
      }
    })
    .catch(() => {});
  testing = true;
  pickFailed = false;
  if (opts.btn) opts.btn.disabled = true;
  renderConfigList();
  renderHero();

  // Прогон, запущенный с Главной, остаётся на Главной — там своё состояние
  // «Подбираю» с прогрессом. Полный лог всё так же на Стратегиях.
  if (opts.autoApply && activePage !== 'home') {
    switchPage('strategies');
    switchSubtab('tests');
  }

  const pickTotal = (currentState.configs || []).length;
  const pickSeen = new Set();
  const trackPick = (line) => {
    const hit = (currentState.configs || []).find((c) => line.includes(c));
    if (hit) pickSeen.add(hit);
    $('heroPickStatus').textContent = hit
      ? `${displayName(hit)} — ${pickSeen.size} из ${pickTotal}`
      : line.trim().slice(0, 90) || 'проверяю…';
    $('heroPickProgress').style.width = pickTotal ? `${Math.round((pickSeen.size / pickTotal) * 100)}%` : '0%';
  };

  $('testError').textContent = '';
  $('testsEmpty').classList.add('hidden');
  $('resultsCard').classList.add('hidden');
  $('testLog').classList.remove('hidden');
  $('testLog').textContent = '';
  $('runTestsBtn').classList.add('hidden');
  $('stopTestsBtn').classList.remove('hidden');
  $('testModeSwitch').classList.add('disabled');

  const appendLog = (line) => {
    const log = $('testLog');
    log.textContent += line + '\n';
    log.scrollTop = log.scrollHeight;
    trackPick(line);
  };

  const finish = () => {
    testing = false;
    if (opts.btn) opts.btn.disabled = false;
    renderConfigList();
    renderHero();
    $('runTestsBtn').classList.remove('hidden');
    $('stopTestsBtn').classList.add('hidden');
    $('testModeSwitch').classList.remove('disabled');
  };

  const fail = (error) => {
    $('testError').textContent = error || 'Тесты завершились с ошибкой';
    showToast(error || 'Тесты завершились с ошибкой', 'error');
  };

  const applyBestAndFinish = async (best) => {
    const shouldApply = opts.autoApply || autoApply;
    if (shouldApply) {
      if (!best) {
        showToast('Лучшая стратегия не определилась', 'warn');
      } else {
        const applied = await applyConfig(best, !opts.autoApply && autoApplyHow === 'service', true);
        if (applied) await verifyAppliedAndToast(best);
      }
    } else {
      showToast('Тесты завершены', 'success');
    }
    refreshState();
  };

  // Режим «DPI → HTTP» целиком отрабатывает на стороне бэкенда: он сам
  // прогоняет DPI по всем конфигам, отбирает прошедших на 100% и скармливает
  // их номера скрипту вторым прогоном. Сюда возвращается уже итог.
  const off = window.zapret.onTestLog(appendLog);
  let res;
  try {
    res = opts.trial ? await window.zapret.trialLatestRelease() : await window.zapret.runTests({ mode: testMode });
  } catch (e) {
    // Вызов мог не вернуться ответом, а упасть. Раньше finish() тогда не
    // выполнялся, и окно навсегда оставалось в «Подбираю» с заблокированными
    // кнопками — до перезапуска Klutz.
    res = { ok: false, error: `Прогон прервался: ${(e && e.message) || e}` };
  } finally {
    off();
    finish();
  }

  if (!res.ok) {
    fail(res.error);
    return;
  }

  // Проверка нового релиза: итоги относятся к нему, а не к рабочему, — ни
  // таблицу, ни «лучшую» стратегию к рабочему релизу не применяем. Лучший
  // конфиг нового там может и не существовать. Решает человек, по вердикту.
  if (opts.trial && res.trial) {
    loadLastResults();
    loadTestsHistory();
    await showTrialVerdict(res.trial);
    return;
  }

  renderResults(res.text);
  loadTestsHistory();
  const { best } = parseResults(res.text);
  await applyBestAndFinish(best);
}

// Вердикт проверки нового релиза: переключаться или остаться.
async function showTrialVerdict(t) {
  const cur = shortRelease(t.current);
  const next = shortRelease(t.release);
  const лучший = (b) => (b ? `${displayName(b.name)} — ${b.ok} из ${b.total}` : 'нет итогов');
  let question;
  if (t.worse) {
    const drops = (t.worse.drops || [])
      .slice(0, 3)
      .map((d) => `${displayName(d.name)}: было ${d.prevOk} → стало ${d.curOk}`)
      .join('; ');
    question =
      `На ${next} хуже, чем на ${cur}. Лучший на ${next}: ${лучший(t.newBest)}, на ${cur}: ${лучший(t.curBest)}.` +
      (drops ? ` Просели: ${drops}.` : '') +
      `\n\nРабочий релиз не тронут. Всё равно переключиться на ${next}?`;
  } else {
    question =
      `${next} не хуже ${cur}: лучший на нём ${лучший(t.newBest)}, сейчас ${лучший(t.curBest)}. ` +
      `Настройки уже перенесены.\n\nПереключиться на ${next}?`;
  }
  if (await showConfirm(question)) await switchToRelease(t.root);
}

$('runTestsBtn').onclick = () => runAllTests();
$('stopTestsBtn').onclick = () => window.zapret.stopTests();

function loadLastResults() {
  window.zapret.getLastTestResults().then((res) => {
    if (res.ok) renderResults(res.text);
    else {
      $('resultsCard').classList.add('hidden');
      $('testsEmpty').classList.remove('hidden');
    }
  });
}

// ─────────── Тесты: снимки прогонов и журнал автопереключений ───────────

function healLogRow(e) {
  const when = new Date(e.at).toLocaleString('ru-RU', {
    day: '2-digit',
    month: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
  if (e.type === 'gave-up') {
    return `<div class="snap-row">
      <div class="snap-left">
        <span class="snap-date">${esc(when)}</span>
        <span class="badge" style="color:var(--red-row);background:var(--red-bg)">сдалось</span>
        <span class="snap-best">Перепробовано ${e.triedCount || 0} — все не работают, самолечение выключено</span>
      </div>
    </div>`;
  }
  const badge = e.ok
    ? '<span class="badge">переключено</span>'
    : '<span class="badge" style="color:var(--red-row);background:var(--red-bg)">не удалось</span>';
  return `<div class="snap-row">
    <div class="snap-left">
      <span class="snap-date">${esc(when)}</span>
      ${badge}
      <span class="snap-best">${e.from ? esc(displayName(e.from)) + ' → ' : ''}${esc(displayName(e.to))}</span>
    </div>
  </div>`;
}

// Бывшая вкладка «История»: снимки прогонов и журнал самолечения живут под
// результатами тестов — там, где их и ищут сразу после прогона.
// Имя релиза без общей приставки: в строке прогона важна версия.
function shortRelease(name) {
  return String(name || '').replace(/^zapret-discord-youtube-/i, '');
}

// «На новом стало хуже». Без этой карточки провал после обновления zapret
// человек замечал сам, сличая файлы результатов руками: на 1.10.2 ALT11 упал
// с 36 до 12, а лучший результат изменился всего на два очка.
function regressionCard(reg) {
  const cur = shortRelease(reg.current);
  const prev = shortRelease(reg.previous);
  const строка = (left, right) =>
    `<div class="snap-row"><div class="snap-left"><span class="snap-best">${left}</span></div>` +
    `<div class="snap-right"><span class="snap-stat">${right}</span></div></div>`;
  // Под каждым просевшим конфигом — что в нём поменялось между релизами, и
  // ссылка положить прежний вариант рядом: тогда тесты покажут, в
  // изменениях ли дело.
  const diffs = reg.diffs || {};
  const drops = (reg.drops || [])
    .map((d) => {
      const что = (diffs[d.name] || [])
        .slice(0, 6)
        .map((c) => `<div class="snap-row diff-row"><span class="snap-stat">${esc(c.profile)}: ${esc(c.text)}</span></div>`)
        .join('');
      const проверить = reg.rollbackPath
        ? ` <span class="cf-link" data-import="${esc(d.name)}">Проверить прежний вариант</span>`
        : '';
      return строка(esc(displayName(d.name)), `было ${d.prevOk} → стало ${d.curOk} из ${d.total}${проверить}`) + что;
    })
    .join('');
  const action = reg.rollbackPath
    ? `<button class="btn sm" id="regressRollbackBtn">Вернуть ${esc(prev)}</button>`
    : `<span class="snap-stat">папки ${esc(prev)} больше нет — вернуть нечем</span>`;
  return `<div class="card">
      <div class="card-head">
        <div class="ch-left"><span class="ch-title">На ${esc(cur)} стало хуже, чем на ${esc(prev)}</span></div>
        <div class="ch-right">${action}</div>
      </div>
      ${строка(`Лучший на ${esc(prev)}: ${esc(displayName(reg.prevBest))}`, `${reg.prevOk} из ${reg.prevTotal}`)}
      ${строка(`Лучший на ${esc(cur)}: ${esc(displayName(reg.curBest))}`, `${reg.curOk} из ${reg.curTotal}`)}
      ${drops}
    </div>`;
}

// Системный прокси: Discord и браузеры идут через него, а тесты — напрямую.
// Ровно на этом ушёл час: тесты зелёные, а Discord висел, потому что шёл
// через Hiddify и к обходу отношения не имел.
function proxyCard(p) {
  const кто = esc(p.owner || p.server);
  return `<div class="card">
      <div class="card-head"><div class="ch-left"><span class="ch-title">Включён системный прокси ${кто}</span></div></div>
      <p class="game-note card-note">Discord и браузеры ходят через него, а тесты — напрямую, мимо прокси. Поэтому
        зелёный тест не значит, что Discord работает через обход. Чтобы проверить обход, выключи ${кто}.</p>
    </div>`;
}

async function loadTestsHistory() {
  const box = $('testsHistory');
  const [res, healRes, reg, proxy] = await Promise.all([
    window.zapret.getTestHistory(),
    window.zapret.getHealLog(),
    // Сравнение с прежним релизом — не обязательное: не вышло, значит без карточки.
    window.zapret.getReleaseRegression().catch(() => null),
    window.zapret.getSystemProxy().catch(() => null),
  ]);

  // Прогоны приходят от старого к новому — показываем свежие сверху.
  const runs = res.ok ? res.runs.slice().reverse() : [];
  const snaps = runs
    .map(
      (r) => `
      <div class="snap-row">
        <div class="snap-left">
          <span class="snap-date">${esc(r.date)}</span>
          <span class="badge ${r.mode === 'dpi' ? 'dpi' : 'neutral'}">${r.mode === 'dpi' ? 'DPI' : 'HTTP'}</span>
          ${r.release ? `<span class="badge neutral" title="${esc(r.release)}">${esc(shortRelease(r.release))}</span>` : ''}
          <span class="snap-best">${esc(r.best ? displayName(r.best) : '—')}</span>
        </div>
        <div class="snap-right">
          <span class="snap-stat">лучший результат ${bestOf(r)}</span>
          <span class="cf-link" data-open="${esc(r.file)}" data-release="${esc(r.release || '')}">Открыть</span>
        </div>
      </div>`
    )
    .join('');

  const healEntries = healRes.ok ? healRes.entries : [];
  const healBody = healEntries.length
    ? healEntries.map(healLogRow).join('')
    : `<div class="empty"><div class="empty-title">Пока не переключалось</div>
        <div class="empty-sub">Здесь будет журнал автопереключений — когда сработало самолечение и на что.</div></div>`;

  const snapsCard = !res.ok
    ? `<p class="error">${esc(res.error)}</p>`
    : runs.length
    ? `<div class="card">
        <div class="card-head"><div class="ch-left"><span class="ch-title">Снимки прогонов</span></div></div>
        ${snaps}
      </div>`
    : '';

  box.innerHTML = `
    ${proxy ? proxyCard(proxy) : ''}
    ${reg ? regressionCard(reg) : ''}
    ${snapsCard}
    <div class="card">
      <div class="card-head"><div class="ch-left"><span class="ch-title">Журнал автопереключений</span></div></div>
      ${healBody}
    </div>`;

  box.querySelectorAll('[data-open]').forEach((el) => {
    el.style.cursor = 'pointer';
    el.onclick = async () => {
      const res2 = await window.zapret.openResultFile(el.dataset.open, el.dataset.release);
      if (!res2.ok) showToast(res2.error || 'Не удалось открыть файл', 'error');
    };
  });

  const rollback = $('regressRollbackBtn');
  if (rollback && reg && reg.rollbackPath) {
    rollback.onclick = async () => {
      await switchToRelease(reg.rollbackPath);
      loadTestsHistory();
    };
  }

  box.querySelectorAll('[data-import]').forEach((el) => {
    el.style.cursor = 'pointer';
    el.onclick = async () => {
      try {
        const name = await window.zapret.importOldConfig(reg.rollbackPath, el.dataset.import);
        showToast(`Добавлен ${displayName(name)}`, 'success', {
          body: 'Это прежний вариант конфига на нынешнем winws. Прогони тесты — станет видно, в изменениях ли дело.',
        });
        refreshState();
      } catch (e) {
        showToast(typeof e === 'string' ? e : 'Не удалось добавить прежний вариант', 'error');
      }
    };
  });
}

// ─────────── Обзор (чипы главной) ───────────

let lastTestBest = null; // last test run's winning config, for the idle "Вариант" stat

async function loadOverview() {
  const [autoSwitch, autostart, testHistory] = await Promise.all([
    window.zapret.getAutoSwitch(),
    window.zapret.getAutostart(),
    window.zapret.getTestHistory(),
  ]);

  $('chipAutostart').textContent = autostart.enabled ? 'Включён' : 'Выключен';
  $('homeAutostartToggle').classList.toggle('on', !!autostart.enabled);

  $('chipHeal').textContent = autoSwitch.enabled ? `Включено · порог ${autoSwitch.threshold}` : 'Выключено';
  $('chipHealHint').textContent = autoSwitch.enabled
    ? `переключиться после ${autoSwitch.threshold} сбоев`
    : 'автопереключение при сбое';
  $('homeAutoSwitchToggle').classList.toggle('on', !!autoSwitch.enabled);

  if (testHistory.ok && testHistory.runs.length) {
    const last = testHistory.runs[testHistory.runs.length - 1];
    $('statLastRun').textContent = `${last.date} · ${bestOf(last)}`;
    lastTestBest = last.best || null;
  } else {
    $('statLastRun').textContent = 'ещё не запускались';
    lastTestBest = null;
  }
  $('heroRedoBtn').classList.toggle('hidden', !lastTestBest);
}

// ─────────── Настройки: служба ───────────

const SVC_LABELS = {
  RUNNING: 'работает',
  STOPPED: 'остановлена',
  START_PENDING: 'запускается',
  STOP_PENDING: 'останавливается',
  PAUSED: 'приостановлена',
};

function svcLabel(s) {
  return SVC_LABELS[s] || s || '—';
}

function statCell(label, text, dot) {
  return `<div>
    <div class="stat-label">${label}</div>
    <div class="stat-val"><span class="stat-dot ${dot}"></span><span class="stat-text" title="${esc(text)}">${esc(text)}</span></div>
  </div>`;
}

async function loadServiceStatus() {
  const s = await window.zapret.getServiceStatus();
  lastServiceStatus = s;
  $('serviceStatus').innerHTML =
    statCell(
      'Служба zapret',
      s.serviceExists ? svcLabel(s.serviceState) : 'не установлена',
      s.serviceState === 'RUNNING' ? 'ok' : s.serviceExists ? 'bad' : ''
    ) +
    statCell('WinDivert', s.windivertState === 'RUNNING' ? 'активен' : 'не активен', s.windivertState === 'RUNNING' ? 'ok' : '') +
    statCell('winws.exe', s.winwsRunning ? 'выполняется' : 'не выполняется', s.winwsRunning ? 'ok' : 'bad') +
    statCell('Стратегия службы', s.strategy ? displayName(s.strategy) : '—', s.strategy ? 'ok' : '');
  $('persistentToggle').classList.toggle('on', !!s.serviceExists);
  $('persistentDesc').textContent = s.serviceExists
    ? `Обход работает как служба Windows${s.strategy ? ` (${displayName(s.strategy)})` : ''} и переживает закрытие приложения и перезагрузку.`
    : 'Сейчас обход останавливается вместе с Klutz. Включи, чтобы он работал как служба Windows.';
  renderHomeFooter();
}

// «Держать обход включённым» = поставить текущий (или лучший из тестов)
// конфиг службой Windows; выключение — снять службу.
$('persistentToggle').onclick = async () => {
  const on = $('persistentToggle').classList.contains('on');
  if (on) {
    const ok = await showConfirm('Снять службу zapret? Обход снова будет работать только пока открыт Klutz.');
    if (!ok) return;
    const res = await window.zapret.removeService();
    if (res && res.ok === false) {
      showToast(res.error || 'Не удалось снять службу', 'error');
      loadServiceStatus();
      refreshState();
      return;
    }
    showToast('Служба снята', 'success');
  } else {
    const target = currentState.activeConfig || lastTestBest;
    if (!target) {
      showToast('Сначала подбери рабочий вариант', 'warn', { body: 'Службе нужно знать, какую стратегию держать включённой.' });
      return;
    }
    const res = await window.zapret.installService(target);
    if (!res.ok) {
      showToast(res.error || 'Не удалось установить службу', 'error');
      return;
    }
    showToast(`Обход держится службой: ${displayName(target)}`, 'success');
  }
  loadServiceStatus();
  refreshState();
};

// ─────────── Настройки: автозапуск ───────────

const autostartToggle = $('autostartToggle');

async function loadAutostart() {
  const res = await window.zapret.getAutostart();
  autostartToggle.classList.toggle('on', !!res.enabled);
}

autostartToggle.onclick = async () => {
  const wanted = !autostartToggle.classList.contains('on');
  autostartToggle.classList.toggle('on', wanted);
  const res = await window.zapret.setAutostart(wanted);
  if (!res.ok) {
    autostartToggle.classList.toggle('on', !wanted);
    showToast(res.error || 'Не удалось изменить автозапуск', 'error');
    return;
  }
  showToast(wanted ? 'Автозапуск включён' : 'Автозапуск отключён', 'success');
  loadOverview();
};

// ─────────── Настройки: самолечение ───────────

const autoSwitchToggle = $('autoSwitchToggle');
const thresholdSeg = $('thresholdSeg');
const checkIntervalSeg = $('checkIntervalSeg');

const segValue = (seg, attr, fallback) => {
  const active = seg.querySelector('.seg-btn.active');
  return Number(active ? active.dataset[attr] : fallback);
};

function renderThresholdDesc() {
  const sec = segValue(checkIntervalSeg, 'sec', 30);
  const th = segValue(thresholdSeg, 'th', 3);
  const every = sec >= 60 ? `${sec / 60} мин` : `${sec} сек`;
  $('thresholdDesc').textContent =
    `При интервале ${every} стратегия сменится примерно через ${Math.round((th * sec) / 60) || 1} мин после потери связи.`;
}

async function loadAutoSwitch() {
  const s = await window.zapret.getAutoSwitch();
  autoSwitchToggle.classList.toggle('on', !!s.enabled);
  thresholdSeg.querySelectorAll('.seg-btn').forEach((b) =>
    b.classList.toggle('active', Number(b.dataset.th) === s.threshold)
  );
  checkIntervalSeg.querySelectorAll('.seg-btn').forEach((b) =>
    b.classList.toggle('active', Number(b.dataset.sec) === s.intervalSec)
  );
  renderThresholdDesc();
}

async function saveAutoSwitch() {
  await window.zapret.setAutoSwitch({
    enabled: autoSwitchToggle.classList.contains('on'),
    threshold: segValue(thresholdSeg, 'th', 3),
    intervalSec: segValue(checkIntervalSeg, 'sec', 30),
  });
  renderThresholdDesc();
  loadOverview();
}

autoSwitchToggle.onclick = async () => {
  const on = autoSwitchToggle.classList.toggle('on');
  await saveAutoSwitch();
  showToast(on ? 'Автопереключение включено' : 'Автопереключение выключено', 'success');
};

[thresholdSeg, checkIntervalSeg].forEach((seg) => {
  seg.querySelectorAll('.seg-btn').forEach((b) => {
    b.onclick = () => {
      seg.querySelectorAll('.seg-btn').forEach((x) => x.classList.toggle('active', x === b));
      saveAutoSwitch();
    };
  });
});

window.zapret.onAutoSwitched(({ from, to }) => {
  showToast(`Переключился: ${from ? displayName(from) + ' → ' : ''}${displayName(to)}`, 'warn');
  refreshState();
  if (activePage === 'strategies' && activeSubtab === 'tests') loadTestsHistory();
});

// ─────────── Настройки: периодический автопрогон тестов ───────────

const autoTestToggle = $('autoTestToggle');
const autoTestIntervalSeg = $('autoTestIntervalSeg');
const autoTestModeSeg = $('autoTestModeSeg');

async function loadAutoTestSchedule() {
  const s = await window.zapret.getAutoTestSchedule();
  autoTestToggle.classList.toggle('on', !!s.enabled);
  autoTestIntervalSeg.querySelectorAll('.seg-btn').forEach((b) =>
    b.classList.toggle('active', Number(b.dataset.days) === s.days)
  );
  autoTestModeSeg.querySelectorAll('.seg-btn').forEach((b) =>
    b.classList.toggle('active', b.dataset.mode === (s.mode || 'standard'))
  );
}

async function saveAutoTestSchedule() {
  const active = autoTestIntervalSeg.querySelector('.seg-btn.active');
  const mode = autoTestModeSeg.querySelector('.seg-btn.active');
  await window.zapret.setAutoTestSchedule({
    enabled: autoTestToggle.classList.contains('on'),
    days: Number(active ? active.dataset.days : 7),
    mode: mode ? mode.dataset.mode : 'standard',
  });
}

autoTestToggle.onclick = async () => {
  const on = autoTestToggle.classList.toggle('on');
  await saveAutoTestSchedule();
  showToast(on ? 'Автопрогон тестов включён' : 'Автопрогон тестов выключен', 'success');
};

[autoTestIntervalSeg, autoTestModeSeg].forEach((seg) => {
  seg.querySelectorAll('.seg-btn').forEach((b) => {
    b.onclick = () => {
      seg.querySelectorAll('.seg-btn').forEach((x) => x.classList.toggle('active', x === b));
      saveAutoTestSchedule();
    };
  });
});

// ─────────── Настройки: уведомления ───────────

const notifyToggle = $('notifyToggle');

async function loadNotifications() {
  const s = await window.zapret.getNotifications();
  notifyToggle.classList.toggle('on', !!s.enabled);
  if (!s.supported) $('notifyMsg').textContent = 'Система не поддерживает уведомления.';
}

notifyToggle.onclick = async () => {
  const on = notifyToggle.classList.toggle('on');
  await window.zapret.setNotifications(on);
  showToast(on ? 'Уведомления включены' : 'Уведомления выключены', 'success');
};

$('testNotifyBtn').onclick = async () => {
  const res = await window.zapret.testNotification();
  $('notifyMsg').textContent = res.ok ? 'Отправлено — проверь угол экрана.' : res.error || 'Не удалось отправить.';
};

// Windows toast XML only offers picking a named system sound, not a real
// volume level — so the toast itself goes out silent (see buildToastXml in
// main.js) and this synthesised chime is what the user actually hears,
// with a real, continuously adjustable gain instead of an OS sound picker.
let notifyAudioCtx = null;

function playNotifyChime(volume, critical) {
  const gain = Math.max(0, Math.min(1, (Number(volume) || 0) / 100));
  if (gain <= 0) return;
  try {
    if (!notifyAudioCtx) notifyAudioCtx = new (window.AudioContext || window.webkitAudioContext)();
    if (notifyAudioCtx.state === 'suspended') notifyAudioCtx.resume();

    const playTone = (freq, startAt, dur) => {
      const osc = notifyAudioCtx.createOscillator();
      const g = notifyAudioCtx.createGain();
      osc.type = 'sine';
      osc.frequency.value = freq;
      const now = notifyAudioCtx.currentTime + startAt;
      g.gain.setValueAtTime(0, now);
      g.gain.linearRampToValueAtTime(gain * 0.5, now + 0.02);
      g.gain.exponentialRampToValueAtTime(0.001, now + dur);
      osc.connect(g);
      g.connect(notifyAudioCtx.destination);
      osc.start(now);
      osc.stop(now + dur + 0.02);
    };

    playTone(880, 0, 0.18);
    playTone(1320, 0.12, 0.22);
    if (critical) playTone(660, 0.32, 0.28);
  } catch {}
}

window.zapret.onPlayNotifySound(({ volume, critical }) => playNotifyChime(volume, critical));

const notifyVolumeRange = $('notifyVolumeRange');
const notifyVolumeVal = $('notifyVolumeVal');
const notifyDurationSeg = $('notifyDurationSeg');

async function loadNotifySound() {
  const s = await window.zapret.getNotifySound();
  notifyVolumeRange.value = s.volume;
  notifyVolumeVal.textContent = s.volume + '%';
  notifyDurationSeg
    .querySelectorAll('.seg-btn')
    .forEach((b) => b.classList.toggle('active', b.dataset.dur === s.duration));
}

async function saveNotifySound() {
  const active = notifyDurationSeg.querySelector('.seg-btn.active');
  await window.zapret.setNotifySound({
    volume: Number(notifyVolumeRange.value),
    duration: active ? active.dataset.dur : 'short',
  });
}

notifyVolumeRange.oninput = () => {
  notifyVolumeVal.textContent = notifyVolumeRange.value + '%';
};
notifyVolumeRange.onchange = () => {
  saveNotifySound();
  playNotifyChime(notifyVolumeRange.value, false);
};

notifyDurationSeg.querySelectorAll('.seg-btn').forEach((b) => {
  b.onclick = () => {
    notifyDurationSeg.querySelectorAll('.seg-btn').forEach((x) => x.classList.toggle('active', x === b));
    saveNotifySound();
  };
});

// ─────────── Telegram (TgWsProxy) ───────────

const tgwsproxyToggle = $('tgwsproxyToggle');
const tgwsproxyAutostartToggle = $('tgwsproxyAutostartToggle');
const openTgLinkBtn = $('openTgLinkBtn');
let tgwsproxyBusy = false;
let tgwsproxyLogLines = [];

async function loadTgwsproxyAutostart() {
  const s = await window.zapret.getTgwsproxySettings();
  tgwsproxyAutostartToggle.classList.toggle('on', !!s.autoStart);
}

tgwsproxyAutostartToggle.onclick = async () => {
  const enabled = !tgwsproxyAutostartToggle.classList.contains('on');
  tgwsproxyAutostartToggle.classList.toggle('on', enabled);
  await window.zapret.setTgwsproxyAutostart(enabled);
};

async function loadTgwsproxyStatus() {
  const status = await window.zapret.getTgwsproxyStatus();
  const warn = status.running && status.healthy === false;

  tgwsproxyToggle.classList.toggle('on', status.running);
  openTgLinkBtn.disabled = !status.running;
  $('tgwsproxyLogsBtn').classList.toggle('hidden', !status.running);
  $('tgwsproxyRestartBtn').classList.toggle('hidden', !status.running);
  $('tgwsproxyStatus').textContent = !status.running
    ? status.available
      ? 'остановлен'
      : 'файл не найден'
    : warn
    ? 'запущен, не отвечает'
    : 'запущен';
  // Separate from the toggle on purpose — the toggle reflects what you asked
  // for, this reflects what's actually true right now (including whether it's
  // actually accepting connections, not just "the process exists"), so the
  // two can never be silently out of sync with no visible sign of it.
  const dot = $('tgwsproxyDot');
  const live = $('tgwsproxyLive');
  dot.classList.toggle('off', !status.running);
  dot.classList.toggle('warn', warn);
  live.classList.toggle('off', !status.running);
  live.classList.toggle('warn', warn);
  live.textContent = !status.running ? 'ВЫКЛЮЧЕНО' : warn ? 'НЕ ОТВЕЧАЕТ' : 'ВКЛЮЧЕНО';

  // The Главная overview card shows the same state — keep it correct
  // regardless of what triggered the change (this page, the tray, the wizard).
  $('engineTgToggle').classList.toggle('on', status.running);
  $('engineTgDot').className = 'status-dot' + (warn ? ' warn' : status.running ? ' ok' : '');
  renderTgStatusbar(status.running);
  $('engineTgDesc').textContent = warn
    ? 'запущен, не отвечает'
    : status.running
    ? 'обходит блокировку'
    : 'Telegram — отдельный MTProto-прокси';
}

tgwsproxyToggle.onclick = async () => {
  if (tgwsproxyBusy) return;
  const turningOn = !tgwsproxyToggle.classList.contains('on');
  tgwsproxyBusy = true;

  if (!turningOn) {
    await window.zapret.stopTgwsproxy();
    tgwsproxyBusy = false;
    loadTgwsproxyStatus();
    return;
  }

  const startRes = await window.zapret.startTgwsproxy();
  tgwsproxyBusy = false;
  if (!startRes.ok) {
    showToast(startRes.error || 'Не удалось запустить TgWsProxy', 'error');
    loadTgwsproxyStatus();
    return;
  }
  showToast('TgWsProxy запущен', 'success');
  loadTgwsproxyStatus();
};

$('tgwsproxyRestartBtn').onclick = async () => {
  if (tgwsproxyBusy) return;
  tgwsproxyBusy = true;
  const res = await window.zapret.restartTgwsproxy();
  tgwsproxyBusy = false;
  if (!res.ok) showToast(res.error || 'Не удалось перезапустить', 'error');
  else showToast('TgWsProxy перезапущен', 'success');
  loadTgwsproxyStatus();
};

openTgLinkBtn.onclick = async () => {
  wizardTgLinkClicked = true;
  const res = await window.zapret.openTgProxyLink();
  if (!res.ok) showToast(res.error || 'Не удалось открыть ссылку', 'error');
};

$('copyTgLinkBtn').onclick = async () => {
  await window.zapret.getTgwsproxySettings(); // ensures secret/defaults exist even before the first start
  const status = await window.zapret.getTgwsproxyStatus();
  if (!status.tgProxyUrl) {
    showToast('Не удалось получить ссылку', 'error');
    return;
  }
  const copied = await window.zapret.copyText(status.tgProxyUrl);
  if (!copied || !copied.ok) {
    showToast('Не удалось скопировать ссылку' + (copied && copied.error ? ': ' + copied.error : ''), 'error');
    return;
  }
  showToast('Ссылка скопирована', 'success');
};

// ---- settings panel ----

// Fields are always visible on this page now (Главная owns the on/off
// switch) — this just (re)populates them from whatever's actually saved,
// on page load and as "Отменить правки".
async function openTgwsproxySettings() {
  const s = await window.zapret.getTgwsproxySettings();
  $('tgwsproxyHostInput').value = s.host;
  $('tgwsproxyPortInput').value = s.port;
  $('tgwsproxySecretInput').value = s.secret;
  $('tgwsproxyDcArea').value = (s.dcIps || []).join('\n');
  $('tgwsproxyCfToggle').classList.toggle('on', !!s.cfproxy);
  $('tgwsproxySettingsMsg').textContent = '';
}


$('tgwsproxyCfToggle').onclick = () => $('tgwsproxyCfToggle').classList.toggle('on');

$('regenTgSecretBtn').onclick = async () => {
  const res = await window.zapret.regenerateTgwsproxySecret();
  if (res.ok) $('tgwsproxySecretInput').value = res.secret;
};

$('tgwsproxySaveBtn').onclick = async () => {
  const wasRunning = tgwsproxyToggle.classList.contains('on');
  const res = await window.zapret.setTgwsproxySettings({
    host: $('tgwsproxyHostInput').value,
    port: Number($('tgwsproxyPortInput').value),
    secret: $('tgwsproxySecretInput').value,
    dcIps: $('tgwsproxyDcArea').value,
    cfproxy: $('tgwsproxyCfToggle').classList.contains('on'),
  });
  if (!res.ok) {
    $('tgwsproxySettingsMsg').textContent = res.error || 'Не удалось сохранить';
    return;
  }
  showToast('Настройки сохранены', 'success');

  if (wasRunning) {
    const ok = await showConfirm('Перезапустить TgWsProxy с новыми настройками?');
    if (ok) {
      await window.zapret.stopTgwsproxy();
      await window.zapret.startTgwsproxy();
    }
  }
  loadTgwsproxyStatus();
};

// ---- live log ----

function renderTgwsproxyLog() {
  const el = $('tgwsproxyLog');
  el.textContent = tgwsproxyLogLines.join('\n');
  el.scrollTop = el.scrollHeight;
}

window.zapret.onTgwsproxyLog((lines) => {
  tgwsproxyLogLines.push(...lines);
  if (tgwsproxyLogLines.length > 500) tgwsproxyLogLines = tgwsproxyLogLines.slice(-500);
  if (!$('tgwsproxyLog').classList.contains('hidden')) renderTgwsproxyLog();
});

// The tray is a second surface that can start/stop the proxy independently of
// this window — refresh our own display whenever the real state changes,
// regardless of which surface caused it.
window.zapret.onTgwsproxyStateChanged(() => {
  loadTgwsproxyStatus();
});

$('tgwsproxyLogsBtn').onclick = async () => {
  const el = $('tgwsproxyLog');
  if (el.classList.contains('hidden')) {
    const res = await window.zapret.getTgwsproxyLog();
    tgwsproxyLogLines = res.lines || [];
    el.classList.remove('hidden');
    renderTgwsproxyLog();
  } else {
    el.classList.add('hidden');
  }
};

// ─────────── Настройки: экспорт/импорт ───────────

$('exportSettingsBtn').onclick = async () => {
  const res = await window.zapret.exportSettings();
  if (res.cancelled) return;
  showToast(res.ok ? 'Настройки сохранены' : res.error || 'Не удалось экспортировать', res.ok ? 'success' : 'error');
};

$('importSettingsBtn').onclick = async () => {
  const res = await window.zapret.importSettings();
  if (res.cancelled) return;
  if (!res.ok) {
    showToast(res.error || 'Не удалось импортировать', 'error');
    return;
  }
  showToast('Настройки применены', 'success');
  loadNotifications();
  loadNotifySound();
  loadAutoSwitch();
  loadAutoTestSchedule();
  loadOverview();
};

// ─────────── Настройки: сеть и фильтры ───────────

const IPSET_LABELS = { any: 'любые IP (any)', none: 'нет (none)', loaded: 'загружен список' };
const gameFilterSeg = $('gameFilterSeg');

// Про Game Filter пользователю надо знать одну вещь, и она не очевидна:
// для сетевой игры он чаще вредит, чем помогает. Обход разбирает и
// пересобирает пакеты, а игровой UDP этого не прощает — в CS2 это видно
// как рывки и телепорты, у Valorant как ошибка подключения. В наборах
// zapret2 игровой UDP Riot и Valorant поэтому прямо помечен «не трогать».
function renderGameFilterNote(mode, el) {
    const d = el || $('gameFilterDesc');
    if (!d) return;
    if (mode === 'udp' || mode === 'all') {
        d.innerHTML =
            'Игровые порты (1024–65535) идут через обход. ' +
            '<b>UDP так лучше не пускать:</b> игровой трафик плохо переносит ' +
            'пересборку пакетов — в CS2 это рывки и телепорты, у Valorant ошибка ' +
            'подключения. Включай, только если без этого игра не запускается вовсе.';
    } else if (mode === 'tcp') {
        d.textContent =
            'Через обход идут игровые порты TCP. Для входа в игру и лаунчеров обычно ' +
            'достаточно этого, а игровой UDP остаётся нетронутым — так и надо.';
    } else {
        d.textContent =
            'Игровые порты через обход не идут. Подходит, пока игры заходят: ' +
            'сам матч так точно ничего не теряет.';
    }
}
const autoUpdateToggle = $('autoUpdateToggle');

async function loadToggles() {
  // Правило брандмауэра от релиза не зависит — грузим до проверки, есть ли он.
  loadDiscordQuic();
  const t = await window.zapret.getToggles();
  if (!t || !t.gameMode) return;
  gameFilterSeg.querySelectorAll('.seg-btn').forEach((b) => b.classList.toggle('active', b.dataset.gf === t.gameMode));
  renderGameFilterNote(t.gameMode);
  $('ipsetVal').textContent = IPSET_LABELS[t.ipsetMode] || t.ipsetMode;
  autoUpdateToggle.classList.toggle('on', !!t.autoUpdate);
}

gameFilterSeg.querySelectorAll('.seg-btn').forEach((b) => {
  b.onclick = async () => {
    const prev = gameFilterSeg.querySelector('.seg-btn.active');
    if (prev === b) return;
    gameFilterSeg.querySelectorAll('.seg-btn').forEach((x) => x.classList.toggle('active', x === b));

    renderGameFilterNote(b.dataset.gf);
    const res = await window.zapret.setGameFilter(b.dataset.gf);
    if (!res.ok) {
      // Запись в папку релиза могла не пройти — не оставляем сегмент
      // показывать режим, которого на диске нет.
      gameFilterSeg.querySelectorAll('.seg-btn').forEach((x) => x.classList.toggle('active', x === prev));
      showToast(res.error || 'Не удалось сменить фильтр игр', 'error');
      return;
    }

    // game_filter.enabled читается только в момент запуска winws.exe, так что
    // без перезапуска стратегии переключатель ничего бы не изменил.
    if (currentState.running && currentState.activeConfig) {
      const ok = await applyConfig(currentState.activeConfig, currentState.installedAsService, true);
      showToast(ok ? 'Фильтр игр применён, стратегия перезапущена' : 'Фильтр сохранён, но перезапустить стратегию не вышло', ok ? 'success' : 'warn');
    } else {
      showToast('Фильтр игр сохранён — применится при запуске стратегии', 'success');
    }
  };
});

$('ipsetModeBtn').onclick = async () => {
  const res = await window.zapret.cycleIpsetMode();
  if (!res.ok) showToast(res.error || 'Не удалось переключить', 'error');
  loadToggles();
};

// «Discord без QUIC» — правило брандмауэра, а не файл релиза: живёт своей
// жизнью и от загруженного релиза не зависит.
const discordQuicToggle = $('discordQuicToggle');

// Через $(), а не константу выше: loadToggles зовёт это и при старте, когда
// до объявления константы выполнение могло ещё не дойти.
async function loadDiscordQuic() {
  let s;
  try {
    s = await window.zapret.getDiscordQuic();
  } catch {
    return;
  }
  $('discordQuicToggle').classList.toggle('on', !!s.enabled);
  if (!s.installed && !s.enabled) {
    $('discordQuicDesc').textContent = 'Discord не найден — правило не к чему привязать. Установи Discord и открой настройки снова.';
  }
}

discordQuicToggle.onclick = async () => {
  const wanted = !discordQuicToggle.classList.contains('on');
  discordQuicToggle.classList.toggle('on', wanted);
  const res = await window.zapret.setDiscordQuic(wanted);
  if (!res.ok) {
    discordQuicToggle.classList.toggle('on', !wanted);
    showToast(res.error || 'Не удалось изменить правило брандмауэра', 'error');
    return;
  }
  // Уже открытый Discord держит соединения и помнит про QUIC — без
  // перезапуска переключатель ничего не изменит, и об этом надо сказать.
  showToast(wanted ? 'Discord без QUIC включён' : 'Discord снова может ходить по QUIC', 'success', {
    body: 'Перезапусти Discord полностью: в трее «Выйти из Discord», потом открой снова.',
  });
};

autoUpdateToggle.onclick = async () => {
  const on = autoUpdateToggle.classList.toggle('on');
  const res = await window.zapret.setAutoUpdate(on);
  if (!res.ok) {
    autoUpdateToggle.classList.toggle('on', !on);
    showToast(res.error || 'Не удалось изменить автопроверку обновлений', 'error');
  }
};

// ─────────── Настройки: свои списки ───────────

$('listsToggleBtn').onclick = () => {
  const hidden = $('listsEditor').classList.toggle('hidden');
  $('listsToggleBtn').textContent = hidden ? 'Показать' : 'Скрыть';
};

async function loadCustomLists() {
  const res = await window.zapret.getCustomLists();
  if (!res.ok) return;
  $('includeListArea').value = res.include;
  $('excludeListArea').value = res.exclude;
}

$('saveListsBtn').onclick = async () => {
  const res = await window.zapret.saveCustomLists({
    include: $('includeListArea').value,
    exclude: $('excludeListArea').value,
  });
  $('listsMsg').textContent = res.ok
    ? 'Сохранено. Применится при следующем запуске стратегии.'
    : `Ошибка: ${res.error}`;
  if (res.ok) showToast('Списки сохранены', 'success');
};

// ─────────── Настройки: обслуживание ───────────

// Плитка на время действия: под названием — что сейчас делается, второе
// нажатие не проходит, итог — уведомлением. Раньше итог писался строкой в
// самый низ раздела, под список релизов, и его никто не видел.
async function maintBusy(btn, busyText, fn) {
  const desc = btn.querySelector('.mt-desc');
  const was = desc ? desc.textContent : '';
  btn.disabled = true;
  if (desc) desc.textContent = busyText;
  try {
    return await fn();
  } catch (e) {
    showToast('Не получилось', 'error', { body: String((e && e.message) || e || '') });
  } finally {
    btn.disabled = false;
    if (desc) desc.textContent = was;
  }
}

$('updateIpsetBtn').onclick = (e) =>
  maintBusy(e.currentTarget, 'Скачиваю и проверяю список…', async () => {
    const res = await window.zapret.updateIpsetList();
    if (!res.ok) {
      showToast('Список IPSet не обновлён', 'error', { body: res.error });
      return;
    }
    if (!res.applied) {
      showToast('Список скачан про запас', 'info', {
        body: `Сейчас IPSet в режиме «${IPSET_LABELS[res.mode] || res.mode}» — список применится при переключении на «загружен список».`,
      });
    } else {
      // Работающий winws мог прочитать список при запуске — перезапуск
      // гарантирует, что новый подхвачен.
      const running = currentState.running && currentState.activeConfig && !currentState.installedAsService;
      showToast('Список IPSet обновлён', 'success', {
        body: `${res.count} адресов и сетей.${running ? ' Перезапусти обход, чтобы он точно взял новый список.' : ''}`,
        ...(running
          ? {
              actionLabel: 'Перезапустить',
              onAction: async () => {
                if (await applyConfig(currentState.activeConfig, false, true)) showToast('Обход перезапущен', 'success');
              },
            }
          : {}),
      });
    }
    loadToggles();
  });

$('updateHostsBtn').onclick = (e) =>
  maintBusy(e.currentTarget, 'Сверяю hosts с рекомендованным…', async () => {
    const s = await window.zapret.hostsStatus();
    if (!s.ok) {
      showToast('Не удалось проверить hosts', 'error', { body: s.error });
      return;
    }
    const conflicts = s.conflicts
      ? `\n\nДля ${s.conflicts} ${plural(s.conflicts, 'имени', 'имён', 'имён')} у тебя в hosts уже свой адрес — ` +
        `${s.conflicts === 1 ? 'его' : 'их'} Klutz не тронет.`
      : '';
    if (!s.missing && !s.stale) {
      if (!s.applied) {
        showToast('hosts уже актуален', 'success');
        return;
      }
      const back = await showConfirm(
        'Строки из рекомендованного уже в hosts.\n\nУбрать их? Всё остальное в файле останется как есть.'
      );
      if (!back) return;
      const r = await window.zapret.removeHosts();
      if (r.ok) showToast('Строки Klutz убраны из hosts', 'success', { body: `Убрано: ${r.removed}. Кэш DNS сброшен.` });
      else showToast('Не удалось изменить hosts', 'error', { body: r.error });
      return;
    }
    const what = [
      s.missing ? `добавить ${s.missing} ${plural(s.missing, 'строку', 'строки', 'строк')}` : '',
      s.stale ? `убрать ${s.stale} ${plural(s.stale, 'устаревшую', 'устаревшие', 'устаревших')}` : '',
    ]
      .filter(Boolean)
      .join(' и ');
    const ok = await showConfirm(
      `В hosts нужно ${what} из рекомендованного zapret-discord-youtube.${conflicts}\n\n` +
        'Klutz положит их отдельным блоком. Файл до первой правки сохранится рядом как hosts.klutz.bak, ' +
        'а убрать строки можно этой же кнопкой.'
    );
    if (!ok) return;
    const r = await window.zapret.applyHosts();
    if (r.ok) {
      showToast('hosts обновлён', 'success', {
        body: `Добавлено: ${r.added}${r.removed ? `, убрано устаревших: ${r.removed}` : ''}. Кэш DNS сброшен.`,
      });
    } else {
      showToast('hosts не обновлён', 'error', { body: r.error });
    }
  });

$('checkUpdatesBtn').onclick = async () => {
  const line = $('releaseVersionLine');
  const notesBox = $('updateNotes');
  const notesBody = $('updateNotesBody');
  const notesBtn = $('openReleaseNotesBtn');
  notesBox.classList.add('hidden');
  line.textContent = 'Проверяю версию…';

  const res = await window.zapret.checkUpdates();
  if (!res.ok) {
    line.textContent = `Ошибка: ${res.error}`;
    return;
  }
  // Не res.upToDate: там простое равенство строк, из-за которого 1.9.10
  // считалась «не последней» рядом с 1.9.9, а сборка новее опубликованной —
  // устаревшей. Сравниваем по частям тем же cmpVer, что и «О программе».
  const outdated = cmpVer(res.remote, res.local) > 0;
  $('engineUpdateDot').classList.toggle('hidden', !outdated);
  $('engineUpdateLabel').textContent = outdated ? `Есть ${res.remote}` : 'Проверить обновление';
  if (!outdated) {
    line.textContent = `Установлена последняя версия: ${res.local}`;
    return;
  }

  line.textContent = `Доступна новая версия ${res.remote} (у тебя ${res.local}).`;
  notesBody.textContent = 'Список изменений — на странице релиза.';
  notesBtn.dataset.url = res.releaseUrl;
  notesBox.classList.remove('hidden');
};

$('openReleaseNotesBtn').onclick = () => {
  const url = $('openReleaseNotesBtn').dataset.url;
  if (url) window.zapret.openExternalUrl(url);
};

// «Проверить и обновить»: новый релиз проходит тесты рядом с рабочим, и
// переключаться или нет — решается по итогу, а не после переключения.
$('trialUpdateBtn').onclick = () => {
  switchPage('strategies');
  switchSubtab('tests');
  runAllTests({ trial: true });
};

// Discord закрывается принудительно — раньше без предупреждения, хоть посреди
// звонка. Сами обратно его не запускаем: Klutz работает от администратора, и
// запущенный из него Discord тоже получил бы права администратора.
$('clearDiscordBtn').onclick = async (e) => {
  const btn = e.currentTarget;
  const ok = await showConfirm(
    'Discord закроется, если открыт, и его кэш удалится. Звонок, если он идёт, прервётся.\n\nОткрыть Discord потом нужно будет самому.'
  );
  if (!ok) return;
  await maintBusy(btn, 'Закрываю Discord и чищу кэш…', async () => {
    const res = await window.zapret.clearDiscordCache();
    if (res.cleared && res.cleared.length) {
      showToast('Кэш Discord очищен', 'success', { body: `Очищено: ${res.cleared.join(', ')}. Теперь открой Discord снова.` });
    } else {
      showToast('Чистить нечего', 'info', { body: 'Кэш уже пуст или Discord не найден.' });
    }
  });
};

// ─────────── Игры ───────────
//
// Адреса игровых серверов нигде не опубликованы и меняются от региона к
// региону. Единственный способ их узнать — посмотреть, куда ходит сам
// процесс игры. В сообществе это делают руками через TCPView; здесь то же
// самое, только само и сразу в список, по которому работает Game Filter.
//
// Имя процесса не спрашиваем: человек, который хочет просто поиграть, не
// обязан знать, как называется исполняемый файл.

let gameScanBusy = false;

// Какие группы развёрнуты и в каких показаны все сети. Живёт до
// перезагрузки окна: это состояние просмотра, а не данные.
const gameOpen = new Set();
const gameAll = new Set();
const СЕТЕЙ_СРАЗУ = 9;
let gameState = null;

const X_SVG =
    '<svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M4 4l8 8M12 4l-8 8"></path></svg>';

// Сообщение под карточкой. Пусто — строки не видно вовсе.
function gameMsg(text) {
    const el = $('gameScanHint');
    el.textContent = text || '';
    el.classList.toggle('hidden', !text);
}

function gameWhen(ms) {
    if (!ms) return '';
    const d = new Date(ms);
    const t = d.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' });
    return d.toDateString() === new Date().toDateString()
        ? `сегодня в ${t}`
        : `${d.toLocaleDateString('ru-RU', { day: '2-digit', month: '2-digit' })} в ${t}`;
}

function gameGroupKey(g, i) {
    return g.asn || `нет-${i}`;
}

function renderGameGroups(groups) {
    return groups
        .map((g, i) => {
            const key = gameGroupKey(g, i);
            const open = gameOpen.has(key);
            const nets = gameAll.has(key) ? g.nets : g.nets.slice(0, СЕТЕЙ_СРАЗУ);
            const имя =
                g.name ||
                (g.asn ? `AS${g.asn}` : g.legacy ? 'Оператор не сохранён' : 'Оператор не определён');
            // Облако узнаём по тому, что в группе одни одиночные адреса: у
            // облачного оператора сети не берутся, только пойманные серверы.
            const облако = !!g.asn && g.nets.length > 0 && g.nets.every((n) => /\/(32|128)$/.test(n));
            const счёт = облако
                ? `${g.nets.length} ${plural(g.nets.length, 'сервер', 'сервера', 'серверов')}`
                : `${g.nets.length} ${plural(g.nets.length, 'сеть', 'сети', 'сетей')}`;
            // Про «развёрнуты из одного адреса» говорим только там, где это
            // правда: у безымянной группы оператора нет, есть сеть вокруг
            // самого адреса.
            // Три разных случая, и путать их нельзя: у старых списков
            // оператора не спрашивали вовсе, и говорить про сеть вокруг
            // адреса там неправда — это объявленные сети оператора.
            const откуда = облако
                ? ' · облако, взяты только пойманные серверы'
                : g.asn
                ? ' · развёрнуты из одного пойманного адреса'
                : g.legacy
                  ? ' · из списка прежней версии'
                  : ' · сеть вокруг пойманного адреса';
            const строки = open
                ? nets
                      .map(
                          (n) =>
                              `<div class="game-net"><span class="game-net-addr">${esc(n)}</span>` +
                              `<button class="addr-remove-btn" data-net="${esc(n)}" title="Убрать эту сеть">${X_SVG}</button></div>`
                      )
                      .join('')
                : '';
            const ещё =
                open && !gameAll.has(key) && g.nets.length > СЕТЕЙ_СРАЗУ
                    ? `<button class="game-more" data-all="${esc(key)}">Показать все ${g.nets.length} ${plural(g.nets.length, 'сеть', 'сети', 'сетей')}</button>`
                    : '';
            return (
                `<div class="game-group${open ? ' open' : ''}" data-group="${esc(key)}">` +
                '<div class="game-group-left">' +
                '<svg class="game-group-chev" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6l4 4 4-4"></path></svg>' +
                `<span class="game-group-name">${esc(имя)}</span>` +
                (g.asn ? `<span class="game-asn">AS${esc(g.asn)}</span>` : '') +
                `<span class="game-group-meta">${счёт}${откуда}</span>` +
                '</div>' +
                '<div class="game-group-right">' +
                (g.legacy
                    ? `<button class="btn-link" data-identify="${esc(key)}">Определить</button>`
                    : '') +
                `<span class="game-when">${esc(gameWhen(g.at))}</span>` +
                `<button class="addr-remove-btn" data-group-remove="${esc(key)}" title="Убрать все сети этого оператора">${X_SVG}</button>` +
                '</div></div>' +
                строки +
                ещё
            );
        })
        .join('');
}

function renderGameSkipped(skipped) {
    const el = $('gameSkipped');
    el.classList.toggle('hidden', !skipped.length);
    if (!skipped.length) return;
    const s = skipped[0];
    const кто = s.name || `AS${s.asn}`;
    const хвост = skipped.length > 1 ? ` И ещё ${skipped.length - 1}.` : '';
    el.innerHTML =
        '<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" style="flex:0 0 auto;color:var(--tx-6)"><path d="M5 12.5h7a2.5 2.5 0 0 0 .4-4.97A4 4 0 0 0 4.7 8.3 2.1 2.1 0 0 0 5 12.5z"></path></svg>' +
        `<span class="game-skip-text">Пропущен адрес <span class="game-skip-addr">${esc(s.addr)}</span> — облако ${esc(кто)}, у него ${s.prefixes} ${plural(s.prefixes, 'сеть', 'сети', 'сетей')}. Прежняя версия облачные адреса не брала — следующий сбор положит такой сервер поштучно.${хвост}</span>`;
}

async function loadGames() {
    const s = await window.zapret.getGameScan();
    gameState = s;
    renderGameFilterNote(s.gameFilter, $('gameFilterDesc2'));
    $('gameFilterSeg2')
        .querySelectorAll('.seg-btn')
        .forEach((b) => b.classList.toggle('active', b.dataset.gf === s.gameFilter));

    if (gameScanBusy) return;
    const addrs = s.addrs || [];
    const groups = s.groups || [];
    const есть = addrs.length > 0;

    $('gameScanSub').textContent = есть
        ? `${addrs.length} ${plural(addrs.length, 'сеть', 'сети', 'сетей')}` +
          (s.changedAt ? ` · ${gameWhen(s.changedAt)}` : '')
        : 'не собраны';

    $('gameEmpty').classList.toggle('hidden', есть);
    $('gameScanning').classList.add('hidden');
    $('gameFoot').classList.toggle('hidden', !есть);

    const box = $('gameGroups');
    box.classList.toggle('hidden', !есть);
    box.innerHTML = есть ? renderGameGroups(groups) : '';

    renderGameSkipped(s.skipped || []);

    // Собранное без включённого фильтра лежит без дела — об этом надо
    // сказать, иначе человек ждёт эффекта, которого не будет.
    gameMsg(
        есть && (!s.gameFilter || s.gameFilter === 'off')
            ? 'Адреса собраны, но Game Filter выключен — до игровых портов обход не доходит, и список лежит без дела.'
            : ''
    );
}

// Клик по группе разворачивает её; крестики убирают сеть или всю группу.
$('gameGroups').onclick = async (e) => {
    const netBtn = e.target.closest('[data-net]');
    if (netBtn) {
        e.stopPropagation();
        const res = await window.zapret.removeGameIps([netBtn.dataset.net]);
        await loadGames();
        if (!res.ok) gameMsg(res.error || 'Не удалось убрать.');
        return;
    }
    const grpBtn = e.target.closest('[data-group-remove]');
    if (grpBtn) {
        e.stopPropagation();
        const key = grpBtn.dataset.groupRemove;
        const g = (gameState?.groups || []).find((x, i) => gameGroupKey(x, i) === key);
        if (!g) return;
        const имя = g.name || (g.asn ? `AS${g.asn}` : 'этого оператора');
        const сколько = `${g.nets.length} ${plural(g.nets.length, 'сеть', 'сети', 'сетей')}`;
        if (!(await showConfirm(`Убрать все сети «${имя}» — ${сколько}?`))) return;
        const res = await window.zapret.removeGameIps(g.nets);
        await loadGames();
        if (!res.ok) gameMsg(res.error || 'Не удалось убрать.');
        return;
    }
    const ident = e.target.closest('[data-identify]');
    if (ident) {
        e.stopPropagation();
        const g = (gameState?.groups || []).find((x, i) => gameGroupKey(x, i) === ident.dataset.identify);
        if (!g) return;
        ident.disabled = true;
        ident.textContent = 'Спрашиваю…';
        const res = await callSafe(window.zapret.identifyGameGroup(g.nets));
        await loadGames();
        if (!res.ok) gameMsg(res.error || 'Не удалось определить оператора.');
        return;
    }
    const more = e.target.closest('[data-all]');
    if (more) {
        gameAll.add(more.dataset.all);
        $('gameGroups').innerHTML = renderGameGroups(gameState?.groups || []);
        return;
    }
    const grp = e.target.closest('[data-group]');
    if (grp) {
        const key = grp.dataset.group;
        if (gameOpen.has(key)) gameOpen.delete(key);
        else gameOpen.add(key);
        $('gameGroups').innerHTML = renderGameGroups(gameState?.groups || []);
    }
};

// Живой сбор: секундомер и счётчик пойманного вместо немого ожидания.
function startScanUI(secs) {
    gameScanBusy = true;
    $('gameEmpty').classList.add('hidden');
    $('gameGroups').classList.add('hidden');
    $('gameFoot').classList.add('hidden');
    $('gameSkipped').classList.add('hidden');
    $('gameScanning').classList.remove('hidden');
    $('gameScanProc').textContent = '';
    $('gameScanCount').textContent = '0';
    $('gameScanFill').style.width = '0%';
    gameMsg('');
    $('gameScanBtn').disabled = true;

    const начало = Date.now();
    const tick = () => {
        const прошло = Math.min(secs, Math.round((Date.now() - начало) / 1000));
        $('gameScanFill').style.width = `${Math.round((прошло / secs) * 100)}%`;
        $('gameScanTime').textContent = `${прошло} с из ${secs}`;
    };
    tick();
    const таймер = setInterval(tick, 1000);
    return () => {
        clearInterval(таймер);
        gameScanBusy = false;
        $('gameScanning').classList.add('hidden');
        $('gameScanBtn').disabled = false;
    };
}

function gameScanTick(p) {
    if (!gameScanBusy) return;
    if (p.proc) $('gameScanProc').textContent = p.proc;
    $('gameScanCount').textContent = String(p.found);
    $('gameScanCountWord').textContent = plural(
        p.found,
        'адрес пойман',
        'адреса поймано',
        'адресов поймано'
    );
}

// Сбор адресов — одна кнопка. Раньше их было две, и обе упирались в то, что
// человек должен был сделать сам: обычный сбор искал игру по таблице
// соединений, где у игрового UDP нет адреса, и отвечал «не вижу игры»;
// глубокий требовал заранее включённый Game Filter и без него молча не
// находил ничего. Теперь Game Filter на время сбора включает сам Klutz, а
// процесс игры узнаёт по исходящему порту пакета.
const GAME_SCAN_SECS = 60;

$('gameScanBtn').onclick = async () => {
    const ok = await showConfirm(
        'Запусти игру и зайди в матч: сбор идёт минуту, и всё это время игра должна ' +
            'работать — адреса видны, только когда она реально шлёт пакеты.\n\n' +
            'На время сбора Klutz включит Game Filter и подробный режим обхода, связь ' +
            'дважды прервётся на секунду. Процесс игры он найдёт сам.'
    );
    if (!ok) return;

    const stopUI = startScanUI(GAME_SCAN_SECS);
    $('gameScanSub').textContent = 'слушаю обход…';
    $('gameScanProc').textContent = 'ищу игру';
    const stopProgress = window.zapret.onGameScan(gameScanTick);
    let note = '';
    try {
        const r = await window.zapret.scanGameFromLog(GAME_SCAN_SECS);
        note = r.note;
    } catch (e) {
        note = typeof e === 'string' ? e : 'Не удалось собрать.';
    }
    stopProgress();
    stopUI();
    await loadGames();
    gameMsg(note);
};

// Та же собранная пачка адресов, но в другую сторону: обход их не трогает.
// Нужно, когда игра работает, а обход ей мешает — на Valorant так и вышло.
$('gameScanSkipBtn').onclick = async () => {
    const ok = await showConfirm(
        'Перенести собранные адреса в список исключений?\n\n' +
            'Обход перестанет трогать этот трафик. Так делают для игр, которые и без ' +
            'обхода работают: в матче нет имени, прятать нечего, а лишние пакеты игра ' +
            'воспринимает как потери — отсюда высокий пинг и обрывы.'
    );
    if (!ok) return;
    const res = await window.zapret.excludeGameIps();
    gameOpen.clear();
    gameAll.clear();
    await loadGames();
    gameMsg(
        res.ok
            ? 'Адреса перенесены в исключения — обход их больше не трогает.'
            : res.error || 'Не удалось перенести.'
    );
};

$('gameScanClearBtn').onclick = async () => {
    const было = (await window.zapret.getGameScan()).saved;
    if (!(await showConfirm(`Убрать все собранные сети (${было})?`))) return;
    const res = await window.zapret.clearGameIps();
    gameOpen.clear();
    gameAll.clear();
    await loadGames();
    gameMsg(res.ok ? 'Адреса игр убраны.' : res.error || 'Не удалось убрать.');
};

$('gameFilterSeg2').querySelectorAll('.seg-btn').forEach((b) => {
    b.onclick = async () => {
        const res = await window.zapret.setGameFilter(b.dataset.gf);
        if (!res.ok) $('gameScanHint').textContent = res.error || 'Не удалось переключить.';
        await loadGames();
        loadToggles();
    };
});

// ─────────── Дополнительные стратегии ───────────
//
// Варианты конфига из релиза с другими точками разреза. Файлы кладутся прямо
// в папку релиза и дальше живут как обычные конфиги: попадают в список, в
// прогон тестов и в рейтинг самолечения.

let extraStrategiesCount = 0;

async function loadExtraStrategies() {
  const s = await window.zapret.getExtraStrategies();
  extraStrategiesCount = s.count || 0;
  const title = $('extraStrategiesTitle');
  const desc = $('extraStrategiesDesc');
  if (extraStrategiesCount > 0) {
    title.textContent = 'Убрать дополнительные стратегии';
    desc.textContent =
      `Сейчас добавлено ${extraStrategiesCount} ${plural(extraStrategiesCount, 'вариант', 'варианта', 'вариантов')}. ` +
      'Удалить их из папки релиза. Конфигов Flowseal это не касается.';
  } else {
    title.textContent = 'Добавить стратегии';
    desc.textContent = s.template
      ? `Варианты «${displayName(s.template)}» с другими точками разреза и приёмами обмана — когда штатные не пробивают.`
      : 'Сначала загрузи релиз zapret.';
  }
}

$('extraStrategiesBtn').onclick = async (e) => {
  const btn = e.currentTarget;
  if (extraStrategiesCount > 0) {
    const ok = await showConfirm(`Удалить ${extraStrategiesCount} добавленных вариантов из папки релиза?`);
    if (!ok) return;
    await maintBusy(btn, 'Убираю варианты…', async () => {
      const res = await window.zapret.removeExtraStrategies();
      if (res.ok) showToast('Дополнительные стратегии убраны', 'success');
      else showToast('Не удалось убрать варианты', 'error', { body: res.error });
    });
  } else {
    const ok = await showConfirm(
      'Добавить варианты текущего конфига в папку релиза?\n\n' +
        'Точки разреза взяты из боевых наборов z2k, приёмы обмана — те, что уже ' +
        'встречаются в твоём релизе. Работают они или нет — покажет только прогон ' +
        'тестов: заранее это не проверить. Убрать можно этой же кнопкой.'
    );
    if (!ok) return;
    await maintBusy(btn, 'Создаю варианты…', async () => {
      const res = await window.zapret.generateExtraStrategies();
      if (res.ok) showToast('Варианты добавлены', 'success', { body: 'Прогони тесты, чтобы узнать, помогает ли что-то из них.' });
      else showToast('Не удалось создать варианты', 'error', { body: res.error });
    });
  }
  await loadExtraStrategies();
  refreshState();
};

let lastDiagResults = null;

async function buildDiagReport(results) {
  const release = currentState.rootPath ? currentState.rootPath.split(/[\\/]/).pop() : 'не загружен';
  const lines = [
    'Klutz — отчёт для разработчика',
    new Date().toLocaleString('ru-RU'),
    `Релиз: ${release}`,
    '',
    ...results.map((r) => `${r.ok ? '✓' : '✗'} ${r.label}${r.warn ? ' — ' + r.warn : ''}`),
  ];

  // Всё, от чего зависит обход: версии, режимы, прокси, итоги последнего
  // прогона. Раньше это выспрашивали по одному после скриншота с ошибкой.
  try {
    const extra = await window.zapret.developerReport();
    if (extra) lines.push('', extra);
  } catch {}

  // Журнал winws — единственное место, где видно, ПОЧЕМУ обход не поднялся.
  // Шторку логов из интерфейса убрали по макету, и бэкенд с тех пор собирал
  // строки в никуда. Отчёт диагностики — это то, что пользователь присылает,
  // когда «не работает», так что место журналу здесь.
  try {
    const log = await window.zapret.getWinwsLog();
    const tail = (log && log.lines ? log.lines : []).slice(-50);
    if (tail.length) {
      const state = log.live ? 'процесс запущен' : 'процесс не запущен';
      lines.push('', `— журнал winws, последние ${tail.length} строк (${state}) —`, ...tail);
    }
  } catch (err) {
    // Отчёт без журнала лучше, чем отсутствие отчёта.
    lines.push('', `— журнал winws недоступен: ${err && err.message ? err.message : err} —`);
  }

  return lines.join('\n');
}

// Файлом — для issue: вставлять простыню текста в форму неудобно.
$('saveDiagBtn').onclick = async () => {
  const btn = $('saveDiagBtn');
  if (!lastDiagResults) {
    btn.disabled = true;
    btn.textContent = 'Проверяю…';
    await runDiagnosticsAndRender();
    btn.disabled = false;
    btn.textContent = 'Сохранить в файл';
  }
  if (!lastDiagResults) return;
  const res = await callSafe(window.zapret.saveReport(await buildDiagReport(lastDiagResults)));
  if (res && res.ok === false) showToast(res.error || 'Не удалось сохранить отчёт', 'error');
  else showToast('Отчёт сохранён', 'success', { body: 'Файл открыт — приложи его к вопросу или issue.' });
};

$('copyDiagBtn').onclick = async () => {
  const btn = $('copyDiagBtn');
  if (!lastDiagResults) {
    btn.disabled = true;
    btn.textContent = 'Проверяю…';
    await runDiagnosticsAndRender();
    btn.disabled = false;
    btn.textContent = 'Скопировать отчёт';
  }
  if (!lastDiagResults) return;

  const text = await buildDiagReport(lastDiagResults);
  let copied = false;
  let lastErr = null;
  try {
    const res = await window.zapret.copyText(text);
    copied = !!(res && res.ok);
    if (!copied) lastErr = new Error((res && res.error) || 'буфер обмена недоступен');
  } catch (err) {
    lastErr = err;
  }
  if (!copied) {
    // The native clipboard can briefly fail to open (another app holding it) —
    // the web API goes through a different code path, worth a second try
    // before telling the user it's broken.
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
    } catch (err2) {
      lastErr = err2;
    }
  }
  if (copied) {
    showToast('Отчёт скопирован', 'success');
  } else {
    showToast('Не удалось скопировать: ' + (lastErr && lastErr.message ? lastErr.message : lastErr), 'error');
  }
};

async function runDiagnosticsAndRender(deep) {
  const box = $('diagResults');
  $('diagHint').classList.add('hidden');
  $('runDiagBtn').disabled = true;
  box.innerHTML = '<div class="diag-row"><span class="dr-icon">·</span><span>Проверяю…</span></div>';
  const res = await callSafe(window.zapret.runDiagnostics(deep));
  $('runDiagBtn').disabled = false;
  lastDiagResults = res.ok ? res.results : null;
  if (!res.ok || !res.results) {
    box.innerHTML = `<div class="diag-row bad"><span class="dr-icon">✗</span><span>Проверка не удалась</span><span class="dr-warn">${esc(
      res.error || ''
    )}</span></div>`;
    return;
  }
  const bad = res.results.filter((r) => !r.ok).length;
  $('maintenanceMsg2').textContent = bad ? `${bad} из ${res.results.length} проверок с проблемами` : 'всё в порядке';
  $('diagFoot').classList.remove('hidden');
  box.innerHTML = res.results
    .map((r) => {
      const fixBtn =
        !r.ok && r.fixKey
          ? `<button class="btn ghost xs diag-fix-btn" data-fix="${esc(r.fixKey)}">Исправить</button>`
          : '';
      return `<div class="diag-row ${r.ok ? 'ok' : 'bad'}"><span class="dr-icon">${r.ok ? '✓' : '✗'}</span><span>${esc(
        r.label
      )}</span>${r.warn ? `<span class="dr-warn">${esc(r.warn)}</span>` : ''}${fixBtn}</div>`;
    })
    .join('');

  box.querySelectorAll('[data-fix]').forEach((btn) => {
    btn.onclick = async () => {
      btn.disabled = true;
      btn.textContent = 'Исправляю…';
      const fixRes = await callSafe(window.zapret.fixDiagnostic(btn.dataset.fix));
      if (!fixRes.ok) {
        showToast(fixRes.error || 'Не удалось исправить', 'error');
        btn.disabled = false;
        btn.textContent = 'Исправить';
        return;
      }
      showToast('Исправлено', 'success');
      runDiagnosticsAndRender();
    };
  });
}

// По кнопке — с глубокими сетевыми пробами; при открытии страницы без
// них, иначе каждый запуск приложения стоил бы мегабайта трафика.
$('runDiagBtn').onclick = () => runDiagnosticsAndRender(true);

// ─────────── «Почему Discord не запускается» ───────────

// Что предложить нажать — ровно тем путём, каким это делается в окне.
const DISCORD_DIAG_ACTIONS = {
  'enable-bypass': ['Включить обход', () => $('heroStartBtn').click()],
  'pick-strategy': [
    'Подобрать стратегию',
    () => {
      switchPage('strategies');
      switchSubtab('tests');
    },
  ],
  'enable-no-quic': [
    'Включить «Discord без QUIC»',
    async () => {
      const r = await window.zapret.setDiscordQuic(true);
      if (!r.ok) {
        showToast(r.error || 'Не удалось изменить правило брандмауэра', 'error');
        return;
      }
      showToast('Discord без QUIC включён', 'success', {
        body: 'Перезапусти Discord полностью: в трее «Выйти из Discord», потом открой снова.',
      });
      loadDiscordQuic();
    },
  ],
  'clear-cache': ['Очистить кэш Discord', () => $('clearDiscordBtn').click()],
};

async function runDiscordDiag() {
  const btn = $('discordDiagBtn');
  btn.disabled = true;
  $('discordDiagCard').classList.remove('hidden');
  $('discordDiagVerdict').textContent = 'Проверяю — до полуминуты…';
  $('discordDiagAdvice').textContent = 'Смотрю логи Discord, системный прокси и что проходит через обход.';
  $('discordDiagRows').innerHTML = '';
  $('discordDiagAction').innerHTML = '';
  let r = null;
  try {
    r = await window.zapret.diagnoseDiscord();
  } catch {
    r = null;
  }
  btn.disabled = false;
  if (!r) {
    $('discordDiagVerdict').textContent = 'Проверка не удалась';
    $('discordDiagAdvice').textContent = '';
    return;
  }
  $('discordDiagVerdict').textContent = r.verdict;
  $('discordDiagAdvice').textContent = r.advice;
  $('discordDiagRows').innerHTML = r.checks
    .map((c) => {
      const cls = c.ok === true ? ' ok' : c.ok === false ? ' bad' : '';
      const icon = c.ok === true ? '✓' : c.ok === false ? '✗' : '·';
      // Жёлтым — только подробности проблемы: «выключен» у прокси это хорошо,
      // и выглядеть предупреждением не должно.
      const note = c.ok === false ? 'dr-warn' : 'dr-note';
      return `<div class="diag-row${cls}"><span class="dr-icon">${icon}</span><span>${esc(c.label)}</span><span class="${note}">${esc(c.detail)}</span></div>`;
    })
    .join('');
  const a = DISCORD_DIAG_ACTIONS[r.action];
  if (a) {
    $('discordDiagAction').innerHTML = `<button class="btn sm" id="discordDiagFixBtn">${esc(a[0])}</button>`;
    $('discordDiagFixBtn').onclick = a[1];
  }
}

$('discordDiagBtn').onclick = runDiscordDiag;
// Из «Обслуживания» — туда, где результат показывается, и сразу проверка.
$('discordDiagTileBtn').onclick = () => {
  switchPage('diagnostics');
  runDiscordDiag();
};

// ─────────── «Как у меня режут» ───────────

function reconSay(verdict, advice) {
  $('reconVerdict').textContent = verdict;
  const lines = (Array.isArray(advice) ? advice : [advice]).filter(Boolean);
  $('reconAdvice').innerHTML = lines.map(esc).join('<br>');
  $('reconAdvice').classList.toggle('hidden', !lines.length);
}

function reconRow(ok, label, detail) {
  const cls = ok === true ? ' ok' : ok === false ? ' bad' : '';
  const icon = ok === true ? '✓' : ok === false ? '✗' : '·';
  const note = ok === false ? 'dr-warn' : 'dr-note';
  return `<div class="diag-row${cls}"><span class="dr-icon">${icon}</span><span>${esc(label)}</span><span class="${note}">${esc(detail)}</span></div>`;
}

function renderRecon(r) {
  let rows = [];
  if (r.status === 'vpn') {
    reconSay('Сначала выключи VPN или прокси', r.message);
    rows = r.vpn.reasons.map((x) => reconRow(false, 'Мешает замеру', x));
  } else if (r.status !== 'ok') {
    reconSay('Сейчас не получится', r.message);
  } else {
    reconSay(r.verdict, r.advice);
    rows = r.targets.map((t) =>
      reconRow(t.kind === 'clear' ? true : t.kind === 'unknown' ? null : false, `${t.name} — ${t.label}`, t.detail)
    );
    if (r.udp) {
      const ok = r.udp.verdict === 'ok' ? true : r.udp.verdict === 'blocked' ? false : null;
      rows.push(reconRow(ok, 'UDP наружу (голос, QUIC)', r.udp.note));
    }
  }
  rows.push(...(r.vpn?.notes || []).map((n) => reconRow(null, 'Заметка', n)));
  $('reconRows').innerHTML = rows.join('');
}

async function runRecon() {
  const btn = $('reconBtn');
  btn.disabled = true;
  $('reconCard').classList.remove('hidden');
  reconSay('Проверяю — до минуты…', 'Без обхода смотрю, как сеть обращается с Discord и YouTube.');
  $('reconRows').innerHTML = '';
  // Какой конфиг включить обратно, если ради разведки обход остановили.
  let restart = null;
  try {
    let r = await window.zapret.reconNetwork();
    if (r.status === 'bypass_running') {
      if (currentState.installedAsService || currentState.serviceExists) {
        reconSay(
          'Обход работает службой Windows',
          'Разведка идёт без обхода. Сними службу в «Настройках» и повтори — сам Klutz её ради проверки не трогает.'
        );
        return;
      }
      const ok = await showConfirm('Разведка идёт без обхода. Остановить его на минуту и потом включить обратно?');
      if (!ok) {
        reconSay('Разведка отменена', '');
        return;
      }
      const prev = currentState.activeConfig;
      const stop = await window.zapret.stopConfig();
      if (stop && !stop.ok) {
        reconSay('Не удалось остановить обход', stop.error || '');
        refreshState();
        return;
      }
      restart = prev;
      reconSay('Проверяю — до минуты…', 'Обход на время остановлен и включится сам.');
      r = await window.zapret.reconNetwork();
    }
    renderRecon(r);
  } catch {
    reconSay('Проверка не удалась', '');
  } finally {
    btn.disabled = false;
    if (restart) {
      if (await applyConfig(restart, false, true)) showToast('Обход включён обратно', 'success');
    } else {
      refreshState();
    }
  }
}

$('reconBtn').onclick = runRecon;
// Кнопки отчёта живут внизу «Диагностики» — плитка ведёт прямо к ним.
$('devReportTileBtn').onclick = () => {
  switchPage('diagnostics');
  requestAnimationFrame(() => $('diagFoot').scrollIntoView({ block: 'center', behavior: 'smooth' }));
};
$('reconTileBtn').onclick = () => {
  switchPage('diagnostics');
  runRecon();
};

// ─────────── Смена релиза ───────────

async function changeRelease() {
  if (currentState.running) {
    const ok = await showConfirm('Обход сейчас работает. Остановить и сменить релиз?');
    if (!ok) return;
    await window.zapret.stopConfig();
  }
  choosingNewRelease = true;
  targetsLoaded = false;
  loadError.textContent = '';
  $('cancelChangeReleaseBtn').classList.remove('hidden');
  render();
}

$('changeReleaseBtn2').onclick = changeRelease;

$('openReleaseFolderBtn').onclick = async () => {
  const res = await window.zapret.openReleaseFolder();
  if (!res.ok) showToast(res.error || 'Не удалось открыть папку', 'error');
};

$('cancelChangeReleaseBtn').onclick = () => {
  choosingNewRelease = false;
  // If this dropzone visit came from the wizard's "also add zapret?" offer,
  // backing out shouldn't leave a stale wait armed for whenever a release
  // eventually does get loaded some unrelated way later.
  wizardAwaitingRelease = false;
  $('cancelChangeReleaseBtn').classList.add('hidden');
  render();
};

// ─────────── Настройки: прошлые релизы (откат) ───────────

async function switchToRelease(root) {
  if (currentState.running) {
    const ok = await showConfirm('Обход сейчас работает. Остановить и переключиться на другой релиз?');
    if (!ok) return;
    await window.zapret.stopConfig();
  }
  const res = await window.zapret.loadPath(root);
  if (!res.ok) {
    showToast(res.error || 'Не удалось переключиться', 'error');
    return;
  }
  await afterReleaseLoaded();
  showToast('Переключился на другой релиз', 'success', carriedBody(res));
}

// Что перенеслось из прежнего релиза. Молча переносить нельзя: человек
// должен знать, что Game Filter и списки уже на месте, а не искать их.
function carriedBody(res) {
  const c = res && res.carried;
  return c && c.length ? { body: `Перенесено из прежнего: ${c.join(', ')}.` } : undefined;
}

async function deleteReleaseRow(folderName) {
  const ok = await showConfirm('Удалить эту версию с диска? Отменить не получится.');
  if (!ok) return;
  const res = await window.zapret.deleteRelease(folderName);
  if (!res.ok) {
    showToast(res.error || 'Не удалось удалить', 'error');
    return;
  }
  showToast('Релиз удалён', 'success');
  loadReleaseList();
}

async function loadReleaseList() {
  const box = $('releaseList');
  const res = await window.zapret.listReleases();
  if (!res.ok || res.releases.length === 0) {
    box.innerHTML = '';
    return;
  }

  // Поля именно такие, какие отдаёт releases.rs::ReleaseEntry:
  // name / path / current / extractedAt. Раньше здесь читались version,
  // folderName, active и root — их не существует, и каждая строка списка
  // выводила «undefined», а кнопки уезжали в бэкенд с этой же строкой.
  box.innerHTML = res.releases
    .map((r) => {
      const date = r.extractedAt ? new Date(r.extractedAt).toLocaleDateString('ru-RU') : '—';
      return `<div class="release-row ${r.current ? 'active' : ''}">
        <div class="release-main">
          <span class="release-ver">${esc(r.name)}</span>
          <span class="release-date">${date}</span>
          ${r.current ? '<span class="release-active-badge">активен</span>' : ''}
        </div>
        <div class="release-actions">
          ${
            r.current
              ? ''
              : `<button class="btn ghost xs" data-switch="${esc(r.name)}">Переключиться</button>
                 <button class="btn ghost xs" data-delete="${esc(r.name)}">Удалить</button>`
          }
        </div>
      </div>`;
    })
    .join('');

  box.querySelectorAll('[data-switch]').forEach((btn) => {
    btn.onclick = () => {
      const rel = res.releases.find((r) => r.name === btn.dataset.switch);
      if (rel) switchToRelease(rel.path);
    };
  });
  box.querySelectorAll('[data-delete]').forEach((btn) => {
    btn.onclick = () => deleteReleaseRow(btn.dataset.delete);
  });
}

// ─────────── Падение winws ───────────

window.zapret.onWinwsCrashed((name) => {
  showToast(`${displayName(name)} неожиданно остановился`, 'error');
  refreshState();
});

// ─────────── Онбординг ───────────

async function afterReleaseLoaded() {
  choosingNewRelease = false;
  $('cancelChangeReleaseBtn').classList.add('hidden');
  await refreshState();
  loadToggles();
  loadServiceStatus();
  loadCustomLists();
  loadLastResults();
  loadAutostart();
  loadAutoSwitch();
  loadAutoTestSchedule();
  loadNotifications();
  loadNotifySound();
  loadTgwsproxyStatus();
  loadReleaseList();
  loadExtraStrategies();
  loadOverview();
  ensureTargetsLoaded();

  if (wizardAwaitingRelease) {
    wizardAwaitingRelease = false;
    continueWizardAfterDiscordSetup();
  }
}

$('onboardStartBtn').onclick = () => {
  $('onboardTourOverlay').classList.add('hidden');
  startTour(INFO_TOUR_STEPS);
};
$('onboardSkipBtn').onclick = () => {
  $('onboardTourOverlay').classList.add('hidden');
  window.zapret.setOnboardingDone(true);
};
$('replayTourBtn').onclick = (e) => {
  e.preventDefault();
  startTour(INFO_TOUR_STEPS);
};

// selector: CSS selector string, or a function returning the element (for
// targets where the visible one depends on state, like the hero card).
const INFO_TOUR_STEPS = [
  {
    page: 'home',
    selector: '.sb-nav',
    title: 'Разделы приложения',
    text: 'Слева — вся навигация: Главная, Стратегии, Диагностика, Telegram и Настройки. Кнопка вверху сворачивает панель, если мешает.',
  },
  {
    page: 'home',
    selector: () => document.querySelector('#heroActive:not(.hidden), #heroIdle:not(.hidden)'),
    title: 'Статус обхода',
    text: 'Тут видно, работает ли обход прямо сейчас. Если нет — одна кнопка сама протестирует варианты и включит рабочий, без похода в «Стратегии».',
  },
  {
    page: 'diagnostics',
    selector: '#diagCoreCard',
    title: 'Здоровье связи',
    text: 'Пингует ключевые адреса Discord и YouTube и показывает, что из этого реально отвечает. Обновляется само, «Проверить» — вручную.',
  },
  {
    page: 'home',
    selector: '.auto-cards',
    title: 'Автоматизация',
    text: 'Включать обход при входе в Windows и сам переключать вариант, если связь пропала. Тумблеры здесь — те же, что в «Настройках».',
  },
  {
    page: 'strategies',
    subtab: 'configs',
    selector: '#configList',
    title: 'Конфиги обхода',
    text: 'Все стратегии из движка zapret, сгруппированные по семействам. У каждой своё меню «⋯»: запустить разово, поставить службой, остановить.',
  },
  {
    page: 'strategies',
    subtab: 'tests',
    selector: '#runTestsBtn',
    title: 'Тесты стратегий',
    text: '«Запустить тесты» прогоняет все конфиги по очереди и показывает, какой реально пробивает блокировку — лучший результат подсвечивается в таблице.',
  },
  {
    page: 'diagnostics',
    selector: '#diagCoreCard',
    title: 'Диагностика',
    text: 'Одно место проверить, что живо: цели обхода и игровые сервисы с пингом. Если что-то не отвечает — здесь же кнопки быстрого исправления типовых проблем.',
  },
  {
    page: 'home',
    selector: '#engineTgToggle',
    title: 'Обход для Telegram',
    text: 'Отдельный процесс специально под Telegram, независимый от обхода Discord/YouTube. Включается этим тумблером, а на вкладке Telegram есть «Открыть» — сразу настроит прокси в Telegram Desktop.',
  },
  {
    page: 'settings',
    selector: '#settingsAutomationCard',
    title: 'Настройки',
    text: 'Автозапуск при входе в Windows, самолечение при сбоях стратегии, расписание автотестов — и ещё три группы ниже: уведомления, сеть и фильтры, обслуживание.',
  },
];

// ---- generic spotlight engine — shared by the informational tour above and
// the action-guided setup wizard below ----

let tourIndex = 0;
let tourActiveSteps = INFO_TOUR_STEPS;
let tourOnComplete = null;
let tourWaitTimer = null;
const tourResizeHandler = () => positionTourStep(tourActiveSteps[tourIndex]);

function startTour(steps, onComplete) {
  tourActiveSteps = steps;
  tourOnComplete = onComplete || null;
  tourIndex = 0;
  $('tourOverlay').classList.remove('hidden');
  window.addEventListener('resize', tourResizeHandler);
  showTourStep(0);
}

function finishTour() {
  if (tourWaitTimer) {
    clearInterval(tourWaitTimer);
    tourWaitTimer = null;
  }
  $('tourOverlay').classList.add('hidden');
  window.removeEventListener('resize', tourResizeHandler);
  const cb = tourOnComplete;
  tourOnComplete = null;
  if (cb) cb();
  else window.zapret.setOnboardingDone(true);
}

async function showTourStep(i) {
  if (tourWaitTimer) {
    clearInterval(tourWaitTimer);
    tourWaitTimer = null;
  }
  const step = tourActiveSteps[i];
  if (!step) {
    finishTour();
    return;
  }
  tourIndex = i;

  if (step.page) switchPage(step.page);
  if (step.subtab) switchSubtab(step.subtab);

  // Give the page/subtab switch a frame to actually paint before measuring —
  // some target lists are populated by handlers triggered from switchPage.
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));

  const target = positionTourStep(step);
  if (!target) {
    // Target isn't there right now (e.g. list still empty) — don't get stuck,
    // just move past this step in whichever direction we were going.
    showTourStep(i + 1);
    return;
  }

  $('tourStepLabel').textContent = `Шаг ${i + 1} из ${tourActiveSteps.length}`;
  $('tourTitle').textContent = step.title;
  $('tourText').textContent = step.text;
  $('tourPrevBtn').disabled = i === 0;
  $('tourNextBtn').textContent = i === tourActiveSteps.length - 1 ? 'Готово' : 'Далее';

  // waitFor steps hold the "Далее" button disabled and auto-advance once the
  // real action actually happened — this is what makes the setup wizard walk
  // someone through actually doing the thing, not just pointing at a button.
  if (step.waitFor) {
    $('tourNextBtn').disabled = true;
    const check = async () => {
      if (await step.waitFor()) {
        if (tourWaitTimer) {
          clearInterval(tourWaitTimer);
          tourWaitTimer = null;
        }
        showTourStep(i + 1);
      }
    };
    tourWaitTimer = setInterval(check, 800);
    check();
  } else {
    $('tourNextBtn').disabled = false;
  }
}

// Край подсветки, не доехавший пару пикселей до края контейнера (сайдбара,
// карточки), оставлял тонкую затемнённую щель в 1px. Такие края дотягиваем
// до ближайшей границы родителя, а всё округляем до целых пикселей.
function snapTourHole(target, rect, pad) {
  const xs = [0, window.innerWidth];
  const ys = [0, window.innerHeight];
  for (let n = target.parentElement; n && n !== document.body; n = n.parentElement) {
    const r = n.getBoundingClientRect();
    xs.push(r.left, r.right);
    ys.push(r.top, r.bottom);
  }
  const snap = (v, edges) => {
    for (const e of edges) if (Math.abs(v - e) <= 3) return Math.round(e);
    return Math.round(v);
  };
  return {
    left: snap(rect.left - pad, xs),
    right: snap(rect.right + pad, xs),
    top: snap(rect.top - pad, ys),
    bottom: snap(rect.bottom + pad, ys),
  };
}

function positionTourStep(step) {
  const target = typeof step.selector === 'function' ? step.selector() : document.querySelector(step.selector);
  if (!target) return null;

  // Страница под туром заблокирована и сама не прокрутится — докручиваем.
  target.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  const rect = target.getBoundingClientRect();
  const hole = snapTourHole(target, rect, 6);
  const hl = $('tourHighlight');
  hl.style.top = `${hole.top}px`;
  hl.style.left = `${hole.left}px`;
  hl.style.width = `${hole.right - hole.left}px`;
  hl.style.height = `${hole.bottom - hole.top}px`;

  // Шаги мастера ждут настоящего действия — там подсвеченное должно
  // нажиматься, остальное нет. Обычные шаги — только смотреть.
  const blocker = $('tourBlocker');
  if (step.waitFor) {
    const T = 38; // блокировка начинается под титулбаром
    const W = window.innerWidth;
    const H = window.innerHeight - T;
    blocker.style.clipPath =
      `path(evenodd, 'M0 0H${W}V${H}H0Z ` +
      `M${hole.left} ${hole.top - T}H${hole.right}V${hole.bottom - T}H${hole.left}Z')`;
  } else {
    blocker.style.clipPath = '';
  }

  const callout = $('tourCallout');
  const calloutWidth = callout.offsetWidth || 300;
  const calloutHeight = callout.offsetHeight || 160;
  const margin = 14;

  let left = rect.right + margin;
  if (left + calloutWidth > window.innerWidth - margin) {
    left = rect.left - margin - calloutWidth;
  }
  if (left < margin) {
    left = Math.min(Math.max(rect.left, margin), window.innerWidth - calloutWidth - margin);
  }

  let top = rect.top;
  if (top + calloutHeight > window.innerHeight - margin) {
    top = window.innerHeight - calloutHeight - margin;
  }
  if (top < margin) top = margin;

  callout.style.left = `${left}px`;
  callout.style.top = `${top}px`;
  return target;
}

$('tourNextBtn').onclick = () => { if (!$('tourNextBtn').disabled) showTourStep(tourIndex + 1); };
$('tourPrevBtn').onclick = () => showTourStep(Math.max(0, tourIndex - 1));
$('tourSkipBtn').onclick = () => finishTour();

// Клавиатура тоже не должна трогать интерфейс под туром: Tab уводил фокус
// на кнопки страницы, а Enter и пробел их нажимали.
document.addEventListener(
  'keydown',
  (e) => {
    if ($('tourOverlay').classList.contains('hidden')) return;
    if ($('tourCallout').contains(e.target)) return;
    const step = tourActiveSteps[tourIndex];
    if (step && step.waitFor) return;
    if (e.key === 'Tab' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
    }
  },
  true
);

// ---- setup wizard: "what do you want to configure" → guided action steps ----

let wizardChoice = null; // 'discord' | 'telegram' | 'both' — the initial pick
let wizardAwaitingRelease = false;
let wizardTgLinkClicked = false;
// Branch-completion flags, not wizardChoice itself, drive what happens after
// each branch finishes — wizardChoice alone can't tell "just finished
// Discord, having arrived via the Telegram branch's cross-offer" apart from
// "just finished Discord as the very first and only branch", and those two
// need different endings (the former skips the offer it already got asked).
let wizardDiscordDone = false;
let wizardTelegramDone = false;

function showWizardChoice() {
  $('wizardChoiceOverlay').classList.remove('hidden');
}

$('wizardChooseDiscordBtn').onclick = () => startWizardBranch('discord');
$('wizardChooseTelegramBtn').onclick = () => startWizardBranch('telegram');
$('wizardChooseBothBtn').onclick = () => startWizardBranch('both');

function startWizardBranch(choice) {
  wizardChoice = choice;
  $('wizardChoiceOverlay').classList.add('hidden');

  if (choice === 'telegram') {
    telegramOnlyMode = true;
    localStorage.setItem('zapretTelegramOnly', '1');
    activePage = 'telegram';
    render();
    loadTgwsproxyStatus();
    loadTgwsproxyAutostart();
    runTelegramWizardSteps();
    return;
  }

  // 'discord' or 'both' — the dropzone is already showing underneath (no
  // release loaded yet); wait for a real one before continuing the wizard.
  wizardAwaitingRelease = true;
}

const DISCORD_WIZARD_STEPS = [
  {
    page: 'home',
    selector: () => document.querySelector('#heroActive:not(.hidden), #heroIdle:not(.hidden)'),
    title: 'Включи обход',
    text: 'Нажми «Подобрать и включить» — Klutz сам проверит несколько способов обхода и включит тот, что реально работает у тебя в сети.',
    waitFor: () => !!currentState.activeConfig,
  },
  {
    page: 'diagnostics',
    selector: '#diagCoreCard',
    title: 'Готово — проверь результат',
    text: 'Обход включён. Открой Discord или YouTube и убедись, что всё грузится. Здесь же можно свериться по пингу и, если что-то не так, воспользоваться кнопками быстрого исправления.',
  },
];

const TELEGRAM_WIZARD_STEPS = [
  {
    page: 'home',
    selector: '#engineTgToggle',
    title: 'Включи обход для Telegram',
    text: 'Нажми переключатель — Klutz поднимет локальный прокси специально для Telegram, отдельно от обхода Discord/YouTube.',
    waitFor: async () => (await window.zapret.getTgwsproxyStatus()).running,
  },
  {
    page: 'telegram',
    selector: '#openTgLinkBtn',
    title: 'Настрой прокси в Telegram',
    text: 'Нажми «Открыть» — Telegram Desktop сам предложит добавить этот прокси, останется подтвердить.',
    waitFor: () => wizardTgLinkClicked,
  },
];

function runDiscordWizardSteps() {
  startTour(DISCORD_WIZARD_STEPS, () => {
    wizardDiscordDone = true;
    if (wizardTelegramDone) {
      // Arrived here via the Telegram branch's own cross-offer — already
      // asked, already answered, nothing left to offer.
      window.zapret.setOnboardingDone(true);
    } else if (wizardChoice === 'both') {
      runTelegramWizardSteps();
    } else {
      offerCrossSetup(
        'Настроить также обход блокировки Telegram?',
        () => runTelegramWizardSteps(),
        () => window.zapret.setOnboardingDone(true)
      );
    }
  });
}

function continueWizardAfterDiscordSetup() {
  runDiscordWizardSteps();
}

function runTelegramWizardSteps() {
  wizardTgLinkClicked = false;
  startTour(TELEGRAM_WIZARD_STEPS, () => {
    wizardTelegramDone = true;
    if (wizardDiscordDone) {
      window.zapret.setOnboardingDone(true);
    } else if (wizardChoice === 'both') {
      runDiscordWizardSteps();
    } else {
      offerCrossSetup(
        'Настроить также обход блокировки Discord и YouTube (zapret)?',
        () => {
          wizardAwaitingRelease = true;
          changeRelease();
        },
        () => window.zapret.setOnboardingDone(true)
      );
    }
  });
}

function offerCrossSetup(text, onYes, onNo) {
  $('wizardCrossOfferText').textContent = text;
  $('wizardCrossOfferOverlay').classList.remove('hidden');
  $('wizardCrossOfferYesBtn').onclick = () => {
    $('wizardCrossOfferOverlay').classList.add('hidden');
    onYes();
  };
  $('wizardCrossOfferNoBtn').onclick = () => {
    $('wizardCrossOfferOverlay').classList.add('hidden');
    onNo();
  };
}

async function loadFromPath(p) {
  loadError.textContent = '';
  const res = await window.zapret.loadPath(p);
  if (!res.ok) {
    loadError.textContent = res.error || 'Не удалось загрузить релиз';
    return;
  }
  await afterReleaseLoaded();
  const body = carriedBody(res);
  if (body) showToast('Релиз загружен', 'success', body);
}

// Размер архива из ответа GitHub — чтобы полоса загрузки показывала
// мегабайты, а не только проценты.
let latestReleaseSize = 0;

window.zapret.getLatestReleaseInfo().then((info) => {
  latestReleaseSize = info.ok ? info.size || 0 : 0;
  if (!info.ok) {
    $('onboardVersionInfo').textContent = 'Не удалось проверить версию на GitHub — выбери вручную.';
    $('downloadLatestBtn').classList.add('hidden');
    $('manualLoadSection').classList.remove('hidden');
    $('showManualLoadBtn').classList.add('hidden');
    return;
  }
  $('onboardVersionInfo').textContent = `Версия ${info.version} · ${(info.size / 1024 / 1024).toFixed(1)} МБ`;
  $('downloadLatestBtn').disabled = false;
});

$('downloadLatestBtn').onclick = async () => {
  loadError.textContent = '';
  $('downloadLatestBtn').disabled = true;
  $('showManualLoadBtn').classList.add('hidden');
  $('downloadProgressWrap').classList.remove('hidden');
  $('downloadProgressFill').style.width = '0%';
  $('downloadProgressText').textContent = 'Скачиваю…';

  // Бэкенд шлёт голый процент (curl --progress-bar даёт только его), а не
  // { received, total }: деструктуризация числа давала undefined, и полоса
  // стояла на нуле с надписью «Скачиваю…» до самого конца.
  const off = window.zapret.onDownloadProgress((p) => {
    const pct = typeof p === 'number' ? p : p && p.total ? Math.round((p.received / p.total) * 100) : NaN;
    if (Number.isNaN(pct)) return;
    $('downloadProgressFill').style.width = pct + '%';
    const mb = (b) => (b / 1024 / 1024).toFixed(1);
    $('downloadProgressText').textContent =
      pct >= 100
        ? 'Скачано, распаковываю…'
        : latestReleaseSize
        ? `${pct}% · ${mb((latestReleaseSize * pct) / 100)} из ${mb(latestReleaseSize)} МБ`
        : `${pct}%`;
  });

  const res = await window.zapret.downloadLatestRelease();
  off();
  $('downloadProgressWrap').classList.add('hidden');
  $('downloadLatestBtn').disabled = false;
  $('showManualLoadBtn').classList.remove('hidden');

  if (!res.ok) {
    loadError.textContent = res.error || 'Не удалось скачать';
    showToast(res.error || 'Не удалось скачать', 'error');
    return;
  }
  showToast('Zapret скачан и загружен', 'success', carriedBody(res));
  await afterReleaseLoaded();
};

$('showManualLoadBtn').onclick = () => {
  const hidden = $('manualLoadSection').classList.toggle('hidden');
  $('showManualLoadBtn').textContent = hidden ? 'Уже скачан — выбрать вручную →' : 'Скрыть';
};

$('pickFolderBtn').onclick = async () => {
  const p = await window.zapret.pickFolder();
  if (p) loadFromPath(p);
};

$('pickArchiveBtn').onclick = async () => {
  const p = await window.zapret.pickArchive();
  if (p) loadFromPath(p);
};

// Перетаскивание обслуживает Tauri, а не DOM: события dragover/drop до
// страницы не доходят, а File.path — свойство Electron, которого в WebView2
// нет. Отсюда приходят настоящие пути на диске.
window.zapret.onFileDrop((e) => {
  if (e.type === 'enter' || e.type === 'over') {
    if (!dropZone.classList.contains('hidden')) dropZone.classList.add('drag-over');
    return;
  }
  dropZone.classList.remove('drag-over');
  if (e.type !== 'drop') return;

  const p = (e.paths || [])[0];
  if (!p) {
    showToast('Не увидела файл в перетаскивании', 'error');
    return;
  }
  loadFromPath(p);
});

// ─────────── Старт ───────────

applyUiMode();
loadOverview();

(async () => {
  await refreshState();
  // Команда отдаёт голый bool, а не { done } — иначе мастер показывался бы
  // при каждом запуске, потому что у булева нет свойства done.
  const onboardingDone = await window.zapret.getOnboardingDone();
  // Не ждём: окно «Что нового» не должно задерживать остальную загрузку.
  maybeShowWhatsNew(onboardingDone);

  if (currentState.rootPath) {
    await afterReleaseLoaded();
    // Edge case: onboarding somehow not done but a release already exists
    // (e.g. state was reset by hand) — the pre-release wizard doesn't apply
    // any more, fall back to the plain informational tour offer instead.
    if (!onboardingDone) $('onboardTourOverlay').classList.remove('hidden');
  } else if (!onboardingDone) {
    showWizardChoice();
  }
})();

setInterval(refreshState, 4000);
