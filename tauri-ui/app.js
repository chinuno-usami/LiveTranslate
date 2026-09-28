const { invoke } = window.__TAURI__.tauri;
const { listen } = window.__TAURI__.event;

const els = {
  statusDot: document.getElementById('status-dot'),
  statusText: document.getElementById('status-text'),
  emptyState: document.getElementById('empty-state'),
  subtitleLines: document.getElementById('subtitle-lines'),
  startBtn: document.getElementById('start-btn'),
  stopBtn: document.getElementById('stop-btn'),
  throughBtn: document.getElementById('through-btn'),
  closeBtn: document.getElementById('close-btn'),
};

let clickThrough = false;

function setStatus(running, message) {
  els.statusText.textContent = message;
  els.statusDot.classList.toggle('running', running);
  els.statusDot.classList.toggle('idle', !running);
}

function renderLines(lines) {
  if (!lines || lines.length === 0) {
    els.subtitleLines.style.display = 'none';
    els.emptyState.style.display = 'block';
    els.subtitleLines.innerHTML = '';
    return;
  }

  els.emptyState.style.display = 'none';
  els.subtitleLines.style.display = 'flex';
  els.subtitleLines.innerHTML = lines
    .map((line) => `<div class="subtitle-line">${escapeHtml(line)}</div>`)
    .join('');
}

function escapeHtml(text) {
  return text
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
    .replaceAll('\n', '<br />');
}

async function loadInitialConfig() {
  const config = await invoke('get_overlay_config');
  document.documentElement.style.setProperty('--font-size', `${config.font_size}px`);
  document.documentElement.style.setProperty('--text-color', config.text_color);
  document.documentElement.style.setProperty('--stroke-color', config.stroke_color);
  clickThrough = Boolean(config.click_through);
  updateClickThroughLabel();
}

async function loadInitialStatus() {
  const status = await invoke('get_status');
  setStatus(status.running, status.message);
}

function updateClickThroughLabel() {
  els.throughBtn.textContent = `穿透: ${clickThrough ? '开' : '关'}`;
}

async function bindActions() {
  els.startBtn.addEventListener('click', async () => {
    await invoke('start_capture');
  });

  els.stopBtn.addEventListener('click', async () => {
    await invoke('stop_capture');
  });

  els.throughBtn.addEventListener('click', async () => {
    clickThrough = !clickThrough;
    await invoke('set_click_through', { enabled: clickThrough });
    updateClickThroughLabel();
  });

  els.closeBtn.addEventListener('click', async () => {
    await invoke('close_overlay');
  });
}

async function bindEvents() {
  await listen('subtitle://update', (event) => {
    renderLines(event.payload.lines || []);
  });

  await listen('status://update', (event) => {
    setStatus(event.payload.running, event.payload.message);
  });

  await listen('config://subtitle', (event) => {
    const config = event.payload;
    document.documentElement.style.setProperty('--font-size', `${config.font_size}px`);
    document.documentElement.style.setProperty('--text-color', config.text_color);
    document.documentElement.style.setProperty('--stroke-color', config.stroke_color);
    clickThrough = Boolean(config.click_through);
    updateClickThroughLabel();
  });

  await listen('error://message', (event) => {
    setStatus(false, String(event.payload || '出现错误'));
  });
}

window.addEventListener('DOMContentLoaded', async () => {
  try {
    await bindEvents();
    await bindActions();
    await loadInitialConfig();
    await loadInitialStatus();
    await invoke('start_capture');
  } catch (error) {
    console.error(error);
    setStatus(false, `启动失败: ${error}`);
  }
});
