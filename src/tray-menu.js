// Меню трея. Вынесено из tray-menu.html отдельным файлом, чтобы
// страница обходилась без инлайновых скриптов: с ними строгая CSP
// (script-src 'self') невозможна, а окно исполняется под администратором.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

const I = (d) =>
  `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">${d}</svg>`;
const ICONS = {
  open: I('<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M2 6h12"/>'),
  stop: I('<rect x="4" y="4" width="8" height="8" rx="1.6"/>'),
  switch: I('<path d="M3 5h9l-2.5-2.5M13 11H4l2.5 2.5"/>'),
  tg: I('<path d="M14 2.5 1.8 7.2l4.4 1.6L12 4.6 7.6 9.6v3.9l2.3-2.6 2.9 2.1z"/>'),
  quit: I('<path d="M8 2v5.5"/><path d="M4.4 4.6a5 5 0 1 0 7.2 0"/>'),
};
const CHEV = '<svg class="chev" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M6 3.5 10.5 8 6 12.5"/></svg>';

// Тема — та же, что выбрана в приложении (общий localStorage); как и
// там, без сохранённого выбора — светлая.
function applyTheme() {
  let t = null;
  try { t = localStorage.getItem('zapretTheme'); } catch {}
  document.documentElement.dataset.theme = t === 'dark' ? 'dark' : 'light';
}

const STATE_LABEL = {
  ok: 'Zapret работает',
  warn: 'Работает с ошибками',
  bad: 'Цели не отвечают',
  idle: 'Обход выключен',
};

let state = null;
let switchOpen = false;

// Пороги те же, что у процентов в окне (verdictColor в renderer.js).
const pctClass = (score) => (score >= 0.85 ? 'good' : score >= 0.4 ? '' : 'poor');

function render() {
  const s = state;
  const targets = s.running && s.targets.length
    ? `<div class="targets">${s.targets
        .map((t) => `<div class="t ${t.ok ? '' : 'fail'}"><span>${esc(t.name)}</span><span class="v">${t.ok ? t.ms + ' мс' : 'нет ответа'}</span></div>`)
        .join('')}</div>`
    : '';

  const sw = s.switchTo.length
    ? `<button class="item ${switchOpen ? 'open' : ''}" data-act="switch-toggle">${ICONS.switch}<span>Переключить на</span>${CHEV}</button>` +
      (switchOpen
        ? `<div class="sub">${s.switchTo
            .map((c) => `<button class="item" data-act="switch:${esc(c.file)}"><span>${esc(c.name)}</span><span class="pct ${pctClass(c.score)}" title="Доля проверенных целей, которые ответили в последнем прогоне">${Math.round(c.score * 100)}%</span></button>`)
            .join('')}</div>`
        : '')
    : '';

  document.getElementById('card').innerHTML = `
    <div class="head ${s.state}">
      <img class="logo logo-light" src="assets/logo-for-light-64.png" alt="">
      <img class="logo logo-dark" src="assets/logo-for-dark-64.png" alt="">
      <div class="head-text">
        <div class="state"><span class="dot"></span>${STATE_LABEL[s.state] || ''}</div>
        <div class="name">${esc(s.running ? s.active || 'Вариант не выбран' : 'Klutz')}</div>
      </div>
    </div>
    ${targets}
    <div class="sep"></div>
    <div class="items">
      <button class="item" data-act="open">${ICONS.open}<span>Открыть Klutz</span></button>
      <button class="item" data-act="stop" ${s.running ? '' : 'disabled'}>${ICONS.stop}<span>Остановить обход</span></button>
      ${sw}
    </div>
    <div class="sep"></div>
    <div class="items">
      <button class="item" data-act="tg">${ICONS.tg}<span>Telegram-прокси</span><span class="toggle ${s.tgRunning ? 'on' : ''}"></span></button>
    </div>
    <div class="sep"></div>
    <div class="items">
      <button class="item danger" data-act="quit">${ICONS.quit}<span>Выйти из Klutz</span></button>
    </div>
    <div class="sep"></div>
    <div class="vers">
      <div><span>Klutz</span><b title="${esc(s.versions.app)}">${esc(s.versions.app)}</b></div>
      <div><span>zapret</span><b title="${esc(s.versions.zapret || 'не загружен')}">${esc(s.versions.zapret || '—')}</b></div>
      <div><span>TgWsProxy</span><b title="${esc(s.versions.tgws)}">${esc(s.versions.tgws)}</b></div>
    </div>`;
}

// Размер окна подгоняется под содержимое — Rust ставит окно над треем
// уже нужного размера, без мигания.
async function reportSize() {
  await new Promise((r) => requestAnimationFrame(() => r()));
  const r = document.querySelector('.wrap').getBoundingClientRect();
  await invoke('tray_menu_ready', { width: Math.ceil(r.width), height: Math.ceil(r.height) });
}

async function refresh(reset) {
  applyTheme();
  if (reset) switchOpen = false;
  state = await invoke('tray_menu_state');
  render();
  await reportSize();
}

document.getElementById('card').addEventListener('click', async (e) => {
  const btn = e.target.closest('[data-act]');
  if (!btn || btn.disabled) return;
  const act = btn.dataset.act;
  if (act === 'switch-toggle') {
    switchOpen = !switchOpen;
    render();
    await reportSize();
    return;
  }
  if (act === 'tg') {
    // Тумблер переключается сразу, меню не закрываем — видно результат.
    state.tgRunning = !state.tgRunning;
    render();
  }
  await invoke('tray_menu_action', { id: act });
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') invoke('tray_menu_hide');
});
document.addEventListener('contextmenu', (e) => e.preventDefault());

listen('tray-menu-open', () => refresh(true));
listen('tray-menu-update', () => refresh(false));
refresh(true);
