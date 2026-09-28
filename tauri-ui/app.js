// Tauri 全局 API
// 注意: 需要 tauri.conf.json 中的 build.withGlobalTauri = true 才会注入 window.__TAURI__
const tauriApi = window.__TAURI__ || {};
const invoke = tauriApi.invoke || (tauriApi.tauri && tauriApi.tauri.invoke);
const listen = tauriApi.event && tauriApi.event.listen;

const els = {
  statusDot: document.getElementById('status-dot'),
  statusText: document.getElementById('status-text'),
  emptyState: document.getElementById('empty-state'),
  subtitleLines: document.getElementById('subtitle-lines'),
  deviceSelect: document.getElementById('device-select'),
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

/// 在面板上直接显示致命错误，避免“界面正常但点不动”的静默失败
function fatal(message) {
  console.error(message);
  if (els.subtitleLines && els.emptyState) {
    els.emptyState.style.display = 'block';
    els.emptyState.textContent = message;
    els.subtitleLines.style.display = 'none';
  }
  if (els.statusText) {
    els.statusText.textContent = '初始化失败';
  }
}

async function loadDevices() {
  try {
    const payload = await invoke('list_devices_command');
    const devices = payload.devices || [];
    els.deviceSelect.innerHTML = '';

    if (devices.length === 0) {
      const opt = document.createElement('option');
      opt.value = 'default';
      opt.textContent = '未找到音频设备';
      els.deviceSelect.appendChild(opt);
      return;
    }

    for (const device of devices) {
      const opt = document.createElement('option');
      opt.value = device.spec;
      opt.textContent = `${device.kind} | ${device.name}`;
      if (device.spec === payload.current) {
        opt.selected = true;
      }
      els.deviceSelect.appendChild(opt);
    }
  } catch (error) {
    console.error('加载设备失败', error);
  }
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

  els.deviceSelect.addEventListener('change', async () => {
    const spec = els.deviceSelect.value;
    try {
      await invoke('set_device', { spec });
    } catch (error) {
      console.error('切换设备失败', error);
      setStatus(false, `切换设备失败: ${error}`);
    }
  });

  els.closeBtn.addEventListener('click', async () => {
    await invoke('close_overlay');
  });
}

async function bindEvents() {
  if (typeof listen !== 'function') {
    console.warn('Tauri event.listen 不可用，跳过事件订阅');
    return;
  }

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

  // 系统托盘和快捷键事件
  await listen('tray://command', (event) => {
    const command = event.payload;
    if (command === 'start') {
      els.startBtn.click();
    } else if (command === 'stop') {
      els.stopBtn.click();
    }
  });

  await listen('shortcut://command', (event) => {
    const command = event.payload;
    if (command === 'start') {
      els.startBtn.click();
    } else if (command === 'stop') {
      els.stopBtn.click();
    }
  });
}

window.addEventListener('DOMContentLoaded', async () => {
  if (typeof invoke !== 'function') {
    fatal(
      '未检测到 Tauri API (window.__TAURI__)。' +
        '请确认 tauri.conf.json 中 build.withGlobalTauri = true。'
    );
    return;
  }

  // 动作绑定与事件订阅互相独立，任何一个失败都不影响另一个
  try {
    await bindActions();
  } catch (error) {
    console.error('bindActions 失败', error);
  }

  try {
    await bindEvents();
  } catch (error) {
    console.error('bindEvents 失败', error);
  }

  for (const step of [loadInitialConfig, loadInitialStatus, loadDevices]) {
    try {
      await step();
    } catch (error) {
      console.error(`${step.name} 失败`, error);
    }
  }

  try {
    await invoke('start_capture');
  } catch (error) {
    console.error('start_capture 失败', error);
    setStatus(false, `启动采集失败: ${error}`);
  }
});
