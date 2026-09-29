// Tauri 全局 API
// 注意: 需要 tauri.conf.json 中的 build.withGlobalTauri = true 才会注入 window.__TAURI__
const tauriApi = window.__TAURI__ || {};
const invoke = tauriApi.invoke || (tauriApi.tauri && tauriApi.tauri.invoke);
const listen = tauriApi.event && tauriApi.event.listen;

const els = {
  toolbar: document.getElementById('toolbar'),
  statusDot: document.getElementById('status-dot'),
  statusText: document.getElementById('status-text'),
  emptyState: document.getElementById('empty-state'),
  subtitleLines: document.getElementById('subtitle-lines'),
  deviceSelect: document.getElementById('device-select'),
  languageSelect: document.getElementById('language-select'),
  fontSlider: document.getElementById('font-slider'),
  fontSizeValue: document.getElementById('font-size-value'),
  sourceToggle: document.getElementById('source-toggle'),
  helpBtn: document.getElementById('help-btn'),
  helpModal: document.getElementById('help-modal'),
  helpClose: document.getElementById('help-close'),
  startBtn: document.getElementById('start-btn'),
  stopBtn: document.getElementById('stop-btn'),
  throughBtn: document.getElementById('through-btn'),
  closeBtn: document.getElementById('close-btn'),
};

const MIN_FONT = 12;
const MAX_FONT = 72;

let clickThrough = false;

/// 应用字号（同步 CSS 变量、滑块、数值显示）
function applyFontSize(px) {
  const size = Math.min(MAX_FONT, Math.max(MIN_FONT, Math.round(px)));
  document.documentElement.style.setProperty('--font-size', `${size}px`);
  if (els.fontSizeValue) {
    els.fontSizeValue.textContent = String(size);
  }
  if (els.fontSlider && Number(els.fontSlider.value) !== size) {
    els.fontSlider.value = String(size);
  }
}

/// 应用字幕面板背景（支持任意 CSS 颜色，含 alpha；transparent 为完全透明）
function applyBackground(value) {
  const color = typeof value === 'string' && value.trim() ? value.trim() : 'rgba(8, 10, 14, 0.28)';
  document.documentElement.style.setProperty('--panel-bg', color);
}

/// 应用顶部工具栏背景（同样支持任意 CSS 颜色，含 alpha）
function applyToolbarBackground(value) {
  const color = typeof value === 'string' && value.trim() ? value.trim() : 'rgba(10, 12, 18, 0.42)';
  document.documentElement.style.setProperty('--toolbar-bg', color);
}

/// 应用"显示原文"开关
function applyShowSource(enabled) {
  if (els.sourceToggle) {
    els.sourceToggle.checked = Boolean(enabled);
  }
}

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
  applyFontSize(Number(config.font_size) || 28);
  applyShowSource(config.show_source);
  document.documentElement.style.setProperty('--text-color', config.text_color);
  document.documentElement.style.setProperty('--stroke-color', config.stroke_color);
  applyBackground(config.background);
  applyToolbarBackground(config.toolbar_background);
  clickThrough = Boolean(config.click_through);
  updateClickThroughLabel();
  applyClickThroughToToolbar();
}

async function loadInitialStatus() {
  const status = await invoke('get_status');
  setStatus(status.running, status.message);
}

function updateClickThroughLabel() {
  els.throughBtn.textContent = `穿透: ${clickThrough ? '开' : '关'}`;
}

/// 工具栏显隐
///
/// 鼠标移出窗口时收起，只保留字幕内容；移入时重新展开。
function setToolbarVisible(visible) {
  if (!els.toolbar) {
    return;
  }
  els.toolbar.classList.toggle('collapsed', !visible);
}

function isHelpOpen() {
  return els.helpModal && !els.helpModal.classList.contains('hidden');
}

/// 应用点击穿透对工具栏的影响
///
/// 穿透开启时窗口收不到鼠标事件，无法靠悬停唤出，因此直接保持收起。
function applyClickThroughToToolbar() {
  setToolbarVisible(!clickThrough);
}

function bindToolbarAutoHide() {
  const root = document.documentElement;

  const hide = () => {
    if (clickThrough || isHelpOpen()) {
      return;
    }
    setToolbarVisible(false);
  };

  const show = () => {
    if (clickThrough || isHelpOpen()) {
      return;
    }
    setToolbarVisible(true);
  };

  root.addEventListener('mouseleave', hide);
  root.addEventListener('mouseenter', show);

  // 部分 WebView 下文档根节点的 mouseleave 不稳，这里用 mouseout 兜底：
  // relatedTarget 为空表示指针真的离开了窗口
  document.addEventListener('mouseout', (event) => {
    if (!event.relatedTarget && !event.toElement) {
      hide();
    }
  });
}

/// 拖动窗口
///
/// 不用 data-tauri-drag-region：它走 Tauri 的 Window 模块（受 allowlist 门控），
/// 且双击会触发最大化，不适合浮窗。这里改为调用自定义命令。
async function bindDrag() {
  if (typeof invoke !== 'function') {
    return;
  }

  const shell = document.getElementById('app');
  shell.addEventListener('mousedown', (event) => {
    if (event.button !== 0) {
      return;
    }
    // 点在交互控件上时不拖动
    if (event.target.closest('button, select, input, textarea, a, label')) {
      return;
    }
    event.preventDefault();
    invoke('start_dragging').catch((error) => {
      console.error('start_dragging 失败', error);
    });
  });
}

/// 窗口边缘拖拽缩放
///
/// Tauri v1 没有 start_resize_dragging，只能在 JS 里跟踪鼠标位移，
/// 再反复调用 Rust 命令设置窗口尺寸/位置。热区比原生大，且压住面板边框。
const RESIZE_MIN_WIDTH = 320;
const RESIZE_MIN_HEIGHT = 120;

function bindResize() {
  if (typeof invoke !== 'function') {
    return;
  }

  const handles = document.querySelectorAll('.resize-handle');
  if (handles.length === 0) {
    return;
  }

  // 串行推送窗口几何：set_size/set_position 不能并发，否则会互相覆盖。
  // 同一帧内多次移动只保留最新值（latest-wins）；返回的 Promise 会在所有
  // 待处理值都应用完之后才 resolve，抬手写回配置前需要等它。
  let pendingBounds = null;
  let draining = null;
  function pushBounds(bounds) {
    pendingBounds = bounds;
    if (draining) {
      return draining;
    }
    draining = (async () => {
      while (pendingBounds) {
        const next = pendingBounds;
        pendingBounds = null;
        try {
          await invoke('set_window_bounds', next);
        } catch (error) {
          console.error('set_window_bounds 失败', error);
        }
      }
      draining = null;
    })();
    return draining;
  }

  handles.forEach((handle) => {
    handle.addEventListener('mousedown', async (event) => {
      if (event.button !== 0) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();

      let start;
      try {
        const [x, y, width, height] = await invoke('window_geometry');
        start = { x, y, width, height };
      } catch (error) {
        console.error('window_geometry 失败', error);
        return;
      }

      const dir = handle.dataset.resize || '';
      // 用 screen 坐标：缩放时窗口自身在移动，client 坐标会跟着漂移
      const startX = event.screenX;
      const startY = event.screenY;

      // 按下鼠标后系统会把鼠标事件锁定到本窗口，指针移出窗口仍能收到 move/up
      let lastBounds = null;
      const onMove = (moveEvent) => {
        const dx = moveEvent.screenX - startX;
        const dy = moveEvent.screenY - startY;

        let width = start.width;
        let height = start.height;

        if (dir.includes('e')) {
          width = start.width + dx;
        }
        if (dir.includes('s')) {
          height = start.height + dy;
        }
        if (dir.includes('w')) {
          width = start.width - dx;
        }
        if (dir.includes('n')) {
          height = start.height - dy;
        }

        if (width < RESIZE_MIN_WIDTH) {
          width = RESIZE_MIN_WIDTH;
        }
        if (height < RESIZE_MIN_HEIGHT) {
          height = RESIZE_MIN_HEIGHT;
        }

        // 拖左/上边时，窗口左上角要跟着走，保证对边固定
        const x = dir.includes('w') ? start.x + (start.width - width) : start.x;
        const y = dir.includes('n') ? start.y + (start.height - height) : start.y;

        lastBounds = {
          x: Math.round(x),
          y: Math.round(y),
          width: Math.round(width),
          height: Math.round(height),
        };
        pushBounds(lastBounds);
      };

      const onUp = () => {
        window.removeEventListener('mousemove', onMove, true);
        window.removeEventListener('mouseup', onUp, true);

        // 只是点了一下没拖动，不必写回
        if (!lastBounds) {
          return;
        }

        // 等最后一次尺寸真正应用后再把几何写回配置，避免存到拖拽中的旧值
        pushBounds(lastBounds).then(() => {
          invoke('persist_window_geometry').catch((error) => {
            console.error('persist_window_geometry 失败', error);
          });
        });
      };

      window.addEventListener('mousemove', onMove, true);
      window.addEventListener('mouseup', onUp, true);
    });
  });
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

/// 加载识别语言候选（随 ASR 后端不同，语言码格式也不同）
async function loadLanguages() {
  try {
    const payload = await invoke('list_languages');
    const options = payload.options || [];
    els.languageSelect.innerHTML = '';

    if (options.length === 0) {
      const opt = document.createElement('option');
      opt.value = payload.current || '';
      opt.textContent = payload.current || '未知';
      els.languageSelect.appendChild(opt);
      return;
    }

    for (const item of options) {
      const opt = document.createElement('option');
      opt.value = item.code;
      opt.textContent = item.label;
      if (item.code === payload.current) {
        opt.selected = true;
      }
      els.languageSelect.appendChild(opt);
    }
  } catch (error) {
    console.error('加载识别语言失败', error);
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
    applyClickThroughToToolbar();
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

  // 识别语言：切换后立即生效，无需重启
  els.languageSelect.addEventListener('change', async () => {
    const code = els.languageSelect.value;
    try {
      await invoke('set_language', { code });
    } catch (error) {
      console.error('切换语言失败', error);
      setStatus(false, `切换语言失败: ${error}`);
    }
  });

  els.closeBtn.addEventListener('click', async () => {
    await invoke('close_overlay');
  });

  // 字号滑块：拖动时实时预览，松手后持久化到配置文件
  els.fontSlider.addEventListener('input', () => {
    applyFontSize(Number(els.fontSlider.value));
  });
  els.fontSlider.addEventListener('change', async () => {
    const size = Number(els.fontSlider.value);
    try {
      await invoke('set_font_size', { size });
    } catch (error) {
      console.error('保存字号失败', error);
    }
  });

  // 显示原文开关
  els.sourceToggle.addEventListener('change', async () => {
    const enabled = els.sourceToggle.checked;
    try {
      await invoke('set_show_source', { enabled });
    } catch (error) {
      console.error('切换原文失败', error);
      setStatus(false, `切换原文失败: ${error}`);
    }
  });

  // 快捷键说明弹窗
  els.helpBtn.addEventListener('click', () => {
    els.helpModal.classList.remove('hidden');
  });
  els.helpClose.addEventListener('click', () => {
    els.helpModal.classList.add('hidden');
    applyClickThroughToToolbar();
  });
  els.helpModal.addEventListener('click', (event) => {
    if (event.target === els.helpModal) {
      els.helpModal.classList.add('hidden');
      applyClickThroughToToolbar();
    }
  });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      els.helpModal.classList.add('hidden');
      applyClickThroughToToolbar();
    }
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
    applyFontSize(Number(config.font_size) || 28);
    applyShowSource(config.show_source);
    document.documentElement.style.setProperty('--text-color', config.text_color);
    document.documentElement.style.setProperty('--stroke-color', config.stroke_color);
    applyBackground(config.background);
    applyToolbarBackground(config.toolbar_background);
    clickThrough = Boolean(config.click_through);
    updateClickThroughLabel();
    applyClickThroughToToolbar();
  });

  // 托盘 / 快捷键切换点击穿透时同步按钮文案与工具栏
  await listen('click-through://update', (event) => {
    clickThrough = Boolean(event.payload);
    updateClickThroughLabel();
    applyClickThroughToToolbar();
  });

  // 后端提示（配置缺失 / ASR / 翻译失败等），直接显示在状态区
  await listen('notice://message', (event) => {
    const message = String(event.payload || '');
    if (message) {
      els.statusText.textContent = message;
      els.statusText.title = message;
    }
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
    await bindDrag();
  } catch (error) {
    console.error('bindDrag 失败', error);
  }

  try {
    bindResize();
  } catch (error) {
    console.error('bindResize 失败', error);
  }

  try {
    bindToolbarAutoHide();
  } catch (error) {
    console.error('bindToolbarAutoHide 失败', error);
  }

  try {
    await bindEvents();
  } catch (error) {
    console.error('bindEvents 失败', error);
  }

  for (const step of [loadInitialConfig, loadInitialStatus, loadDevices, loadLanguages]) {
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
