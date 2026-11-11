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
  sourcesBtn: document.getElementById('sources-btn'),
  sourcesPopover: document.getElementById('sources-popover'),
  sourcesList: document.getElementById('sources-list'),
  sourcesClose: document.getElementById('sources-close'),
  addSourceBtn: document.getElementById('add-source-btn'),
  settingsBtn: document.getElementById('settings-btn'),
  languageSelect: document.getElementById('language-select'),
  fontSlider: document.getElementById('font-slider'),
  fontSizeValue: document.getElementById('font-size-value'),
  sourceToggle: document.getElementById('source-toggle'),
  sourceToggleLabel: document.getElementById('source-toggle-label'),
  translateToggle: document.getElementById('translate-toggle'),
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
let isCapturing = false;

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

/// 应用"翻译"开关；关闭即仅识别模式，此时"原文"开关无意义，置灰
function applyTranslateEnabled(enabled) {
  const on = enabled !== false;
  if (els.translateToggle) {
    els.translateToggle.checked = on;
  }
  if (els.sourceToggle) {
    els.sourceToggle.disabled = !on;
  }
  if (els.sourceToggleLabel) {
    els.sourceToggleLabel.classList.toggle('disabled', !on);
  }
}

function setStatus(running, message) {
  isCapturing = Boolean(running);
  els.statusText.textContent = message;
  els.statusDot.classList.toggle('running', isCapturing);
  els.statusDot.classList.toggle('idle', !isCapturing);
}

/// 显示操作或运行提示错误，但不篡改当前真实采集中的圆点状态
function showActionError(message) {
  const text = String(message || '出现错误');
  els.statusText.textContent = text;
  els.statusText.title = text;
  els.statusDot.classList.toggle('running', isCapturing);
  els.statusDot.classList.toggle('idle', !isCapturing);
}

/// 根据 source_id 稳定地映射到一个来源颜色（用于 chip）
const SOURCE_COLORS = ['#7DD3FC', '#F0ABFC', '#FDE68A', '#6EE7B7', '#FCA5A5', '#C4B5FD'];
function colorForSource(id) {
  let hash = 0;
  const text = String(id || '');
  for (let i = 0; i < text.length; i += 1) {
    hash = (hash * 31 + text.charCodeAt(i)) >>> 0;
  }
  return SOURCE_COLORS[hash % SOURCE_COLORS.length];
}

/// 渲染结构化字幕条目；后端行文本已含 [来源] 前缀，这里改用 chip 展示
function renderLines(entries) {
  const list = Array.isArray(entries) ? entries : [];
  if (list.length === 0) {
    els.subtitleLines.style.display = 'none';
    els.emptyState.style.display = 'block';
    els.subtitleLines.innerHTML = '';
    return;
  }

  els.emptyState.style.display = 'none';
  els.subtitleLines.style.display = 'flex';
  els.subtitleLines.innerHTML = list
    .map((entry) => {
      const label = entry.label ? String(entry.label) : '';
      const prefix = label ? `[${label}] ` : '';
      const lines = (entry.lines || []).map((line) => {
        const text = prefix && line.startsWith(prefix) ? line.slice(prefix.length) : line;
        return formatSubtitleHtml(text);
      });
      const chip = label
        ? `<span class="source-chip" style="--chip-color:${colorForSource(entry.source_id)}">${escapeHtml(label)}</span>`
        : '';
      const body = lines
        .map((line, index) => `<div class="subtitle-line${index === 0 ? '' : ' subtitle-line-sub'}">${line}</div>`)
        .join('');
      return `<div class="subtitle-entry">${chip}${body}</div>`;
    })
    .join('');
}

function escapeHtml(text) {
  return String(text == null ? '' : text)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function formatSubtitleHtml(text) {
  return escapeHtml(text).replaceAll('\n', '<br />');
}

async function loadInitialConfig() {
  const config = await invoke('get_overlay_config');
  lastConfig = { ...config };
  if (config.asr_backend) {
    lastBackend = config.asr_backend;
  }
  if (config.asr_language) {
    lastLanguage = config.asr_language;
  }
  applyFontSize(Number(config.font_size) || 28);
  applyShowSource(config.show_source);
  applyTranslateEnabled(config.translate_enabled);
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
    // 点在交互控件或浮层（输入源面板 / 帮助）上时不拖动
    if (
      event.target.closest(
        'button, select, input, textarea, a, label, .popover, .modal, .help-modal, #sources-popover',
      )
    ) {
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

      let ended = false;
      let onMove = null;
      let lastBounds = null;
      const onUp = () => {
        ended = true;
        if (onMove) {
          window.removeEventListener('mousemove', onMove, true);
        }
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
      // 先挂 mouseup：若用户在 window_geometry IPC 返回前就已松手，不至于泄漏监听
      window.addEventListener('mouseup', onUp, true);

      let start;
      try {
        const [x, y, width, height] = await invoke('window_geometry');
        start = { x, y, width, height };
      } catch (error) {
        window.removeEventListener('mouseup', onUp, true);
        console.error('window_geometry 失败', error);
        return;
      }
      if (ended) {
        return;
      }

      const dir = handle.dataset.resize || '';
      // 用 screen 坐标：缩放时窗口自身在移动，client 坐标会跟着漂移
      const startX = event.screenX;
      const startY = event.screenY;

      // 按下鼠标后系统会把鼠标事件锁定到本窗口，指针移出窗口仍能收到 move/up
      onMove = (moveEvent) => {
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

      window.addEventListener('mousemove', onMove, true);
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

let lastSourcesPayload = null;
/// 后端当前生效的浮窗配置（用于失败时回滚乐观更新）
let lastConfig = {};
/// 后端当前生效的 ASR 后端与识别语言
let lastBackend = '';
let lastLanguage = '';

function sourceRow(source, devices) {
  const options = devices
    .map(
      (device) =>
        `<option value="${escapeHtml(device.spec)}"${device.spec === source.device ? ' selected' : ''}>` +
        `${escapeHtml(device.kind)} | ${escapeHtml(device.name)}</option>`,
    )
    .join('');
  const known = devices.some((device) => device.spec === source.device);
  const fallback = known
    ? ''
    : `<option value="${escapeHtml(source.device)}" selected>${escapeHtml(source.device)}（未找到）</option>`;
  const dot = source.running ? 'running' : 'idle';

  return `
    <div class="source-row" data-id="${escapeHtml(source.id)}">
      <span class="status-dot ${dot}"></span>
      <span class="source-label" title="${escapeHtml(source.message || '')}">${escapeHtml(source.label)}</span>
      <select class="device-select source-device" title="选择设备（切换会重启采集）">
        ${fallback}${options}
      </select>
      <label class="source-toggle" title="该源是否翻译">
        <input type="checkbox" class="source-translate" ${source.translate ? 'checked' : ''} />
        <span>翻译</span>
      </label>
      <label class="source-toggle" title="是否参与采集（改动会重启采集）">
        <input type="checkbox" class="source-enabled" ${source.enabled ? 'checked' : ''} />
        <span>启用</span>
      </label>
    </div>`;
}

/// 按 id 就地更新状态点与控件状态；若设置页增删重排了输入源则整块刷新
function updateSourceRows(sources) {
  const rows = Array.from(els.sourcesList.querySelectorAll('.source-row'));
  const sameShape =
    rows.length === sources.length &&
    rows.every((row, index) => row.dataset.id === String(sources[index].id));
  if (!sameShape) {
    refreshSources();
    return;
  }
  if (lastSourcesPayload) {
    lastSourcesPayload = { ...lastSourcesPayload, sources };
  }
  for (const source of sources) {
    const row = els.sourcesList.querySelector(
      `.source-row[data-id="${CSS.escape(String(source.id))}"]`,
    );
    if (!row) continue;
    const dot = row.querySelector('.status-dot');
    if (dot) dot.className = `status-dot ${source.running ? 'running' : 'idle'}`;
    const label = row.querySelector('.source-label');
    if (label) {
      label.textContent = source.label || '';
      label.title = source.message || '';
    }
    const translate = row.querySelector('.source-translate');
    if (translate) translate.checked = Boolean(source.translate);
    const enabled = row.querySelector('.source-enabled');
    if (enabled) enabled.checked = Boolean(source.enabled);
    const device = row.querySelector('.source-device');
    if (device && document.activeElement !== device && device.value !== source.device) {
      if (Array.from(device.options).some((opt) => opt.value === source.device)) {
        device.value = source.device;
      } else {
        refreshSources();
        return;
      }
    }
  }
}

async function refreshSources() {
  let payload;
  let devicesPayload;
  try {
    [payload, devicesPayload] = await Promise.all([
      invoke('list_sources'),
      invoke('list_devices_command'),
    ]);
  } catch (error) {
    console.error('加载输入源失败', error);
    return;
  }
  lastSourcesPayload = payload;
  renderSources(payload, devicesPayload.devices || []);
}

function renderSources(payload, devices) {
  const sources = (payload && payload.sources) || [];
  if (sources.length === 0) {
    els.sourcesList.innerHTML = '<div class="sources-empty">没有输入源</div>';
    return;
  }
  els.sourcesList.innerHTML = sources.map((source) => sourceRow(source, devices)).join('');

  els.sourcesList.querySelectorAll('.source-row').forEach((row) => {
    const id = row.dataset.id;
    row.querySelector('.source-device').addEventListener('change', async (event) => {
      try {
        await invoke('set_source_device', { id, spec: event.target.value });
      } catch (error) {
        console.error('切换设备失败', error);
        showActionError(`切换设备失败: ${error}`);
      }
      refreshSources();
    });
    row.querySelector('.source-translate').addEventListener('change', async (event) => {
      try {
        await invoke('set_source_translate', { id, enabled: event.target.checked });
      } catch (error) {
        console.error('切换翻译失败', error);
        showActionError(`切换翻译失败: ${error}`);
        event.target.checked = !event.target.checked;
      }
      refreshSources();
    });
    row.querySelector('.source-enabled').addEventListener('change', async (event) => {
      try {
        await invoke('set_source_enabled', { id, enabled: event.target.checked });
      } catch (error) {
        console.error('切换启用失败', error);
        showActionError(`切换启用失败: ${error}`);
      }
      refreshSources();
    });
  });
}

/// 加载识别语言候选（随 ASR 后端不同，语言码格式也不同）
async function loadLanguages() {
  try {
    const payload = await invoke('list_languages');
    const options = payload.options || [];
    lastLanguage = payload.current || '';
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
    try {
      await invoke('start_capture');
    } catch (error) {
      console.error('启动采集失败', error);
      showActionError(`启动采集失败: ${error}`);
    }
  });

  els.stopBtn.addEventListener('click', async () => {
    try {
      await invoke('stop_capture');
    } catch (error) {
      console.error('停止采集失败', error);
      showActionError(`停止采集失败: ${error}`);
    }
  });

  els.throughBtn.addEventListener('click', async () => {
    const next = !clickThrough;
    try {
      await invoke('set_click_through', { enabled: next });
      clickThrough = next;
      updateClickThroughLabel();
      applyClickThroughToToolbar();
    } catch (error) {
      console.error('切换点击穿透失败', error);
      showActionError(`切换穿透失败: ${error}`);
    }
  });

  els.sourcesBtn.addEventListener('click', async () => {
    const hidden = els.sourcesPopover.classList.toggle('hidden');
    els.sourcesBtn.setAttribute('aria-expanded', hidden ? 'false' : 'true');
    if (!hidden) {
      await refreshSources();
    }
  });
  els.sourcesClose.addEventListener('click', () => {
    els.sourcesPopover.classList.add('hidden');
    els.sourcesBtn.setAttribute('aria-expanded', 'false');
  });
  els.addSourceBtn.addEventListener('click', async () => {
    els.sourcesPopover.classList.add('hidden');
    els.sourcesBtn.setAttribute('aria-expanded', 'false');
    try {
      await invoke('open_settings');
    } catch (error) {
      console.error('打开设置失败', error);
      showActionError(`打开设置失败: ${error}`);
    }
  });
  els.settingsBtn.addEventListener('click', async () => {
    try {
      await invoke('open_settings');
    } catch (error) {
      console.error('打开设置失败', error);
      showActionError(`打开设置失败: ${error}`);
    }
  });

  // 识别语言：切换后立即生效，无需重启
  els.languageSelect.addEventListener('change', async () => {
    const code = els.languageSelect.value;
    try {
      await invoke('set_language', { code });
      lastLanguage = code;
    } catch (error) {
      console.error('切换语言失败', error);
      if (lastLanguage) els.languageSelect.value = lastLanguage;
      showActionError(`切换语言失败: ${error}`);
    }
  });

  els.closeBtn.addEventListener('click', async () => {
    try {
      await invoke('close_overlay');
    } catch (error) {
      console.error('关闭浮窗失败', error);
      showActionError(`关闭浮窗失败: ${error}`);
    }
  });

  // 字号滑块：拖动时实时预览，松手后持久化到配置文件
  els.fontSlider.addEventListener('input', () => {
    applyFontSize(Number(els.fontSlider.value));
  });
  els.fontSlider.addEventListener('change', async () => {
    const size = Number(els.fontSlider.value);
    try {
      await invoke('set_font_size', { size });
      lastConfig.font_size = size;
    } catch (error) {
      console.error('保存字号失败', error);
      // 浮窗已经按新字号渲染了，回退到后端仍生效的值，避免“幽灵设置”
      applyFontSize(lastConfig.font_size || 28);
      showActionError(`保存字号失败: ${error}`);
    }
  });

  // 显示原文开关
  els.sourceToggle.addEventListener('change', async () => {
    const enabled = els.sourceToggle.checked;
    const previous = Boolean(lastConfig.show_source);
    try {
      await invoke('set_show_source', { enabled });
      lastConfig.show_source = enabled;
    } catch (error) {
      console.error('切换原文失败', error);
      applyShowSource(previous);
      showActionError(`切换原文失败: ${error}`);
    }
  });

  // 翻译开关：关闭即仅识别模式，立即生效
  els.translateToggle.addEventListener('change', async () => {
    const enabled = els.translateToggle.checked;
    const previous = lastConfig.translate_enabled !== false;
    applyTranslateEnabled(enabled);
    try {
      await invoke('set_translate_enabled', { enabled });
      lastConfig.translate_enabled = enabled;
    } catch (error) {
      console.error('切换翻译失败', error);
      applyTranslateEnabled(previous);
      showActionError(`切换翻译失败: ${error}`);
    }
  });

  // 快捷键说明弹窗
  els.helpBtn.addEventListener('click', () => {
    // 打开模态前收起输入源弹层，避免两个浮层同时可交互
    els.sourcesPopover.classList.add('hidden');
    els.sourcesBtn.setAttribute('aria-expanded', 'false');
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
      els.sourcesPopover.classList.add('hidden');
      els.sourcesBtn.setAttribute('aria-expanded', 'false');
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
    renderLines(event.payload.entries || []);
  });

  await listen('source-status://update', (event) => {
    const payload = event.payload || { sources: [] };
    const count = (payload.sources || []).length;
    if (els.sourcesBtn) {
      els.sourcesBtn.textContent = count > 0 ? `源 (${count})` : '源';
    }
    if (!els.sourcesPopover.classList.contains('hidden')) {
      // 只做行级局部更新：重建整块 DOM 会摧毁正在展开的 <select> 与焦点，
      // 而源状态变化恰好发生在用户刚操作完之后。设备列表也不该每次重新枚举。
      updateSourceRows(payload.sources || []);
    }
  });

  await listen('output-status://update', (event) => {
    const payload = event.payload || {};
    const messages = [];
    if (payload.steamvr && payload.steamvr.enabled) {
      messages.push(`SteamVR: ${payload.steamvr.message || (payload.steamvr.connected ? '已连接' : '等待连接')}`);
    }
    if (payload.osc && payload.osc.enabled) {
      messages.push(`OSC: ${payload.osc.message || '已启用'}`);
    }
    if (messages.length > 0) {
      els.statusText.title = messages.join(' / ');
    } else {
      els.statusText.removeAttribute('title');
    }
  });

  await listen('status://update', (event) => {
    setStatus(event.payload.running, event.payload.message);
  });

  await listen('config://subtitle', async (event) => {
    const config = event.payload;
    lastConfig = { ...config };
    applyFontSize(Number(config.font_size) || 28);
    applyShowSource(config.show_source);
    applyTranslateEnabled(config.translate_enabled);
    document.documentElement.style.setProperty('--text-color', config.text_color);
    document.documentElement.style.setProperty('--stroke-color', config.stroke_color);
    applyBackground(config.background);
    applyToolbarBackground(config.toolbar_background);
    clickThrough = Boolean(config.click_through);
    updateClickThroughLabel();
    applyClickThroughToToolbar();

    const backendChanged = Boolean(config.asr_backend && config.asr_backend !== lastBackend);
    if (config.asr_backend) {
      lastBackend = config.asr_backend;
    }
    if (backendChanged) {
      await loadLanguages();
    } else if (config.asr_language && config.asr_language !== lastLanguage) {
      lastLanguage = config.asr_language;
      if (els.languageSelect) {
        const exists = Array.from(els.languageSelect.options).some(
          (opt) => opt.value === config.asr_language,
        );
        if (!exists) {
          await loadLanguages();
        } else {
          els.languageSelect.value = config.asr_language;
        }
      }
    }
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
    showActionError(event.payload || '出现错误');
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

  for (const step of [loadInitialConfig, loadInitialStatus, loadLanguages]) {
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
    showActionError(`启动采集失败: ${error}`);
  }
});
