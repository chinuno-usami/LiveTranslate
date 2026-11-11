// LiveTranslate 设置页（Tauri v1 + 静态前端，无构建链）
const tauriApi = window.__TAURI__ || {};
const invoke = tauriApi.invoke || (tauriApi.tauri && tauriApi.tauri.invoke);

const els = {
  profileSelect: document.getElementById('profile-select'),
  profileSwitch: document.getElementById('profile-switch'),
  profileNew: document.getElementById('profile-new'),
  profileDup: document.getElementById('profile-dup'),
  profileRename: document.getElementById('profile-rename'),
  profileDelete: document.getElementById('profile-delete'),
  profileOpen: document.getElementById('profile-open'),
  profileReason: document.getElementById('profile-reason'),
  search: document.getElementById('search'),
  reload: document.getElementById('reload-btn'),
  cancel: document.getElementById('cancel-btn'),
  save: document.getElementById('save-btn'),
  restartBanner: document.getElementById('restart-banner'),
  restartBtn: document.getElementById('restart-btn'),
  errorBanner: document.getElementById('error-banner'),
  nav: document.getElementById('group-nav'),
  content: document.getElementById('group-content'),
  status: document.getElementById('save-status'),
  configPath: document.getElementById('config-path'),
  dialogModal: document.getElementById('dialog-modal'),
  dialogTitle: document.getElementById('dialog-title'),
  dialogMessage: document.getElementById('dialog-message'),
  dialogInput: document.getElementById('dialog-input'),
  dialogCancel: document.getElementById('dialog-cancel'),
  dialogOk: document.getElementById('dialog-ok'),
};

const state = {
  schema: null,
  capabilities: { steamvr: false, osc: false },
  profiles: null,
  rawPath: null,
  rawText: '',
  originalRawText: '',
  rawDirty: false,
  devices: [],
  languages: [],
  sources: [],
  currentGroup: 'sources',
  dirty: new Map(),
  invalidInputs: new Map(),
  rawInputs: new Map(),
  originalSourcesJson: '[]',
  previewTimer: null,
  previewToken: 0,
  busy: false,
  hasLoaded: false,
};

function setBusy(busy) {
  state.busy = Boolean(busy);
  const targets = [
    els.save,
    els.cancel,
    els.reload,
    els.profileSelect,
    els.profileSwitch,
    els.profileNew,
    els.profileDup,
    els.profileRename,
    els.profileDelete,
    els.profileOpen,
  ];
  for (const el of targets) {
    if (el) el.disabled = state.busy;
  }
  if (!state.busy) {
    updateProfileActionButtons();
  }
}

// ---------------- 页内模态对话框 ----------------

let activeDialogCancel = null;

function closeActiveDialog() {
  if (activeDialogCancel) {
    const cancelFn = activeDialogCancel;
    activeDialogCancel = null;
    cancelFn();
  }
}

function openDialog({
  mode,
  title = '提示',
  message = '',
  defaultValue = '',
  placeholder = '',
  okText = '确定',
  cancelText = '取消',
  danger = false,
  dismissValue,
}) {
  closeActiveDialog();

  return new Promise((resolve) => {
    const previousFocus = document.activeElement;
    const isPrompt = mode === 'prompt';
    const resolvedDismiss =
      dismissValue !== undefined ? dismissValue : isPrompt ? null : false;

    els.dialogTitle.textContent = title;
    els.dialogMessage.textContent = message;
    els.dialogOk.textContent = okText;
    els.dialogCancel.textContent = cancelText;
    els.dialogOk.classList.toggle('primary', !danger);
    els.dialogOk.classList.toggle('danger-fill', Boolean(danger));

    if (isPrompt) {
      els.dialogInput.value = defaultValue == null ? '' : String(defaultValue);
      els.dialogInput.placeholder = placeholder;
      els.dialogInput.classList.remove('hidden');
    } else {
      els.dialogInput.value = '';
      els.dialogInput.classList.add('hidden');
    }

    els.dialogModal.classList.remove('hidden');

    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      activeDialogCancel = null;
      els.dialogModal.classList.add('hidden');
      els.dialogOk.removeEventListener('click', onOk);
      els.dialogCancel.removeEventListener('click', onCancel);
      els.dialogModal.removeEventListener('click', onBackdrop);
      window.removeEventListener('keydown', onKeyDown, true);
      if (previousFocus && typeof previousFocus.focus === 'function') {
        previousFocus.focus();
      }
      resolve(result);
    };

    const onOk = () => {
      finish(isPrompt ? els.dialogInput.value.trim() : true);
    };
    const onCancel = () => {
      finish(isPrompt ? null : false);
    };
    const onDismiss = () => {
      finish(resolvedDismiss);
    };
    const onBackdrop = (event) => {
      if (event.target === els.dialogModal) {
        onDismiss();
      }
    };
    const onKeyDown = (event) => {
      if (event.isComposing || event.keyCode === 229) {
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        onDismiss();
      } else if (event.key === 'Enter' && document.activeElement !== els.dialogCancel) {
        event.preventDefault();
        event.stopPropagation();
        onOk();
      }
    };

    activeDialogCancel = onDismiss;
    els.dialogOk.addEventListener('click', onOk);
    els.dialogCancel.addEventListener('click', onCancel);
    els.dialogModal.addEventListener('click', onBackdrop);
    window.addEventListener('keydown', onKeyDown, true);

    requestAnimationFrame(() => {
      if (isPrompt) {
        els.dialogInput.focus();
        els.dialogInput.select();
      } else {
        els.dialogOk.focus();
      }
    });
  });
}

function uiConfirm(message, opts = {}) {
  return openDialog({ mode: 'confirm', message, ...opts });
}

function uiChoice(message, opts = {}) {
  return openDialog({ mode: 'confirm', message, dismissValue: null, ...opts });
}

function uiPrompt(message, defaultValue = '', opts = {}) {
  return openDialog({ mode: 'prompt', message, defaultValue, ...opts });
}

function clearPreviewTimer() {
  if (state.previewTimer) {
    clearTimeout(state.previewTimer);
    state.previewTimer = null;
  }
}

function clearLocalEdits() {
  state.dirty.clear();
  state.invalidInputs.clear();
  state.rawInputs.clear();
  state.rawDirty = false;
}

function dirtyCount() {
  const modified = new Set([...state.dirty.keys(), ...state.invalidInputs.keys()]);
  return modified.size + (state.rawDirty ? 1 : 0);
}

function escapeControlChars(value) {
  return String(value == null ? '' : value)
    .replaceAll('\\', '\\\\')
    .replaceAll('\n', '\\n')
    .replaceAll('\r', '\\r')
    .replaceAll('\t', '\\t');
}

function unescapeControlChars(value) {
  return String(value == null ? '' : value).replace(/\\([\\nrt])/g, (_, ch) => {
    if (ch === 'n') return '\n';
    if (ch === 'r') return '\r';
    if (ch === 't') return '\t';
    return '\\';
  });
}

function escapeHtml(text) {
  return String(text == null ? '' : text)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function deepClone(value) {
  return JSON.parse(JSON.stringify(value));
}

const STATUS_CLASS = { ok: 'ok', err: 'err' };

function setStatus(message, kind) {
  els.status.textContent = message;
  els.status.className = STATUS_CLASS[kind] || '';
}

function showError(message) {
  if (!message) {
    els.errorBanner.classList.add('hidden');
    els.errorBanner.textContent = '';
    return;
  }
  els.errorBanner.textContent = String(message);
  els.errorBanner.classList.remove('hidden');
}

function currentGroup() {
  return (state.schema?.groups || []).find((group) => group.id === state.currentGroup) || null;
}

// ---------------- 加载 ----------------

async function loadAll() {
  setStatus('加载中…');
  showError('');
  try {
    const [schema, capabilities, profiles, raw, sources, devices, languages] = await Promise.all([
      invoke('get_settings_schema'),
      invoke('get_capabilities'),
      invoke('get_profiles'),
      invoke('get_config_raw'),
      invoke('get_sources_config'),
      invoke('list_devices_command'),
      invoke('list_languages'),
    ]);
    state.schema = schema;
    state.capabilities = capabilities;
    state.profiles = profiles;
    state.rawPath = raw.path;
    if (!state.rawDirty) {
      state.rawText = raw.text || '';
      state.originalRawText = raw.text || '';
    }
    // 输入源表：本地有未保存的改动时以本地为准，否则会被这份快照覆盖
    const keepLocalSources = state.dirty.has('audio.sources');
    const freshSources = deepClone(sources.sources || []);
    if (!keepLocalSources) state.sources = freshSources;
    state.originalSourcesJson = JSON.stringify(freshSources);
    state.devices = devices.devices || [];
    state.languages = languages.options || [];
    // 注意：这里**不能** state.dirty.clear()。
    // 设置窗口在 await 期间仍然可交互，清空会把用户刚输入的内容吞掉；
    // 丢弃修改只应该在 reloadOnShow 的确认框里显式发生。
    pruneDirty();

    els.configPath.textContent = raw.path || '（内置默认配置，首次保存时创建）';
    if (schema?.warning) {
      showError(schema.warning);
    }
    renderProfileBar();
    renderNav();
    renderContent();
    updateDirtyIndicator();
    if (dirtyCount() === 0) {
      setStatus('就绪');
    }
    els.restartBanner.classList.add('hidden');
  } catch (error) {
    console.error(error);
    showError(`加载设置失败: ${error}`);
    setStatus('加载失败', 'err');
  }
}

/**
 * 丢掉已经不存在于 schema 的脏数据（配置项改名/删除后残留）。
 * 保留仍有效的，包括加载期间刚输入的那些。
 */
function pruneDirty() {
  const known = new Set(['audio.sources']);
  for (const group of state.schema?.groups || []) {
    for (const field of group.fields || []) known.add(field.path);
  }
  for (const path of [...state.dirty.keys()]) {
    if (!known.has(path)) state.dirty.delete(path);
  }
  for (const path of [...state.invalidInputs.keys()]) {
    if (!known.has(path)) state.invalidInputs.delete(path);
  }
  for (const path of [...state.rawInputs.keys()]) {
    if (!known.has(path)) state.rawInputs.delete(path);
  }
}

async function refreshSchemaOnly(preferredProfile) {
  const [schema, raw, sources, devices, languages, profiles] = await Promise.all([
    invoke('get_settings_schema'),
    invoke('get_config_raw'),
    invoke('get_sources_config'),
    invoke('list_devices_command'),
    invoke('list_languages'),
    invoke('get_profiles'),
  ]);
  state.schema = schema;
  state.profiles = profiles;
  renderProfileBar(preferredProfile);
  state.rawPath = raw.path;
  if (!state.rawDirty) {
    state.rawText = raw.text || '';
  }
  els.configPath.textContent = raw.path || '（内置默认配置，首次保存时创建）';
  if (schema?.warning) {
    showError(schema.warning);
  } else {
    showError('');
  }
  state.sources = deepClone(sources.sources || []);
  state.originalSourcesJson = JSON.stringify(state.sources);
  state.devices = devices.devices || [];
  state.languages = languages.options || [];
  if (!state.rawDirty) {
    state.originalRawText = raw.text || '';
  }
  state.invalidInputs.clear();
  state.rawInputs.clear();
  pruneDirty();
  renderContent();
}

// ---------------- 配置档 ----------------

function updateProfileActionButtons() {
  const status = state.profiles || { enabled: false, profiles: [], active: null };
  const enabled = Boolean(status.enabled);
  const selected = selectedProfile();
  els.profileSelect.disabled = !enabled;
  if (els.profileSwitch) {
    els.profileSwitch.disabled = !enabled || !selected || selected === status.active;
  }
  els.profileNew.disabled = !enabled;
  els.profileDup.disabled = !enabled || !selected;
  els.profileRename.disabled = !enabled || !selected || selected === 'default';
  els.profileDelete.disabled = !enabled || !selected || selected === 'default';
  els.profileOpen.disabled = !enabled;
}

function renderProfileBar(preferredSelected) {
  const status = state.profiles || { enabled: false, profiles: [], active: null };
  const list = status.profiles || [];
  const target =
    preferredSelected && list.some((profile) => profile.name === preferredSelected)
      ? preferredSelected
      : status.active;
  const options = list
    .map(
      (profile) =>
        `<option value="${escapeHtml(profile.name)}"${profile.name === target ? ' selected' : ''}>` +
        `${escapeHtml(profile.name)}${profile.name === status.active ? '（当前）' : ''}${
          profile.valid ? '' : '（无效）'
        }</option>`,
    )
    .join('');
  els.profileSelect.innerHTML =
    options || `<option value="">${status.enabled ? '（无）' : '（配置档不可用）'}</option>`;
  updateProfileActionButtons();
  els.profileReason.textContent = status.enabled
    ? ''
    : status.reason || '当前配置路径不支持配置档';
}

function selectedProfile() {
  return els.profileSelect.value || null;
}

async function switchProfile(name) {
  if (!name || name === state.profiles?.active) return;
  const count = dirtyCount();
  if (count > 0) {
    const ok = await uiConfirm(`当前有 ${count} 项未保存的修改，切换配置档将丢弃它们。继续？`, {
      title: '切换配置档',
      okText: '丢弃并切换',
      danger: true,
    });
    if (!ok) {
      return;
    }
  }
  const confirmed = await uiConfirm(
    `切换到配置档「${name}」将替换当前配置（采集运行中会自动重启），继续？`,
    { title: '切换配置档', okText: '切换' },
  );
  if (!confirmed) {
    return;
  }
  clearPreviewTimer();
  state.previewToken += 1;
  setBusy(true);
  try {
    const result = await invoke('switch_profile', { name });
    // 旧配置档的未保存编辑不能带进新配置档
    clearLocalEdits();
    await refreshSchemaOnly(name);
    updateDirtyIndicator();
    setStatus(result.message || '已切换配置档', 'ok');
  } catch (error) {
    console.error(error);
    showError(`切换配置档失败: ${error}`);
    renderProfileBar(name);
  } finally {
    setBusy(false);
  }
}

async function createProfile() {
  const name = await uiPrompt('新配置档名（1–40 字符，字母/数字/中文/_/-）', '', {
    title: '新建配置档',
    placeholder: '例如：vrchat',
    okText: '下一步',
  });
  if (!name) return;
  const copyCurrent = await uiChoice(
    '是否复制当前配置作为新配置档的初始内容？\n（选择「创建默认」则使用内置默认配置，按 Esc 取消创建）',
    {
      title: '初始配置来源',
      okText: '复制当前配置',
      cancelText: '创建默认',
    },
  );
  if (copyCurrent === null) return;
  try {
    await invoke('create_profile', { name, copyCurrent });
    const [profiles, raw] = await Promise.all([invoke('get_profiles'), invoke('get_config_raw')]);
    state.profiles = profiles;
    state.rawPath = raw.path;
    els.configPath.textContent = raw.path || '（内置默认配置，首次保存时创建）';
    renderProfileBar(name);
    setStatus(`已创建配置档「${name}」（可点击「切换」启用）`, 'ok');
  } catch (error) {
    showError(`创建配置档失败: ${error}`);
  }
}

async function duplicateProfile() {
  const from = selectedProfile();
  if (!from) return;
  const to = await uiPrompt(`复制「${from}」到新配置档名`, `${from}-copy`, {
    title: '复制配置档',
    okText: '复制',
  });
  if (!to) return;
  try {
    await invoke('duplicate_profile', { from, to });
    state.profiles = await invoke('get_profiles');
    renderProfileBar(to);
    setStatus(`已复制为「${to}」（可点击「切换」启用）`, 'ok');
  } catch (error) {
    showError(`复制配置档失败: ${error}`);
  }
}

async function renameProfile() {
  const old = selectedProfile();
  if (!old || old === 'default') return;
  const next = await uiPrompt(`把「${old}」重命名为`, old, {
    title: '重命名配置档',
    okText: '重命名',
  });
  if (!next || next === old) return;
  try {
    await invoke('rename_profile', { old, new: next });
    state.profiles = await invoke('get_profiles');
    renderProfileBar(next);
    setStatus(`已重命名为「${next}」`, 'ok');
  } catch (error) {
    showError(`重命名失败: ${error}`);
  }
}

async function deleteProfile() {
  const name = selectedProfile();
  if (!name || name === 'default') return;
  const isActive = name === state.profiles?.active;
  if (isActive) {
    const fallback =
      (state.profiles?.profiles || []).some((p) => p.name === 'default')
        ? 'default'
        : (state.profiles?.profiles || []).find((p) => p.name !== name)?.name || 'default';
    const dirtyHint = dirtyCount() > 0 ? '，并丢弃当前未保存的修改' : '';
    const confirmed = await uiConfirm(
      `配置档「${name}」正在使用，删除前将自动切换回「${fallback}」${dirtyHint}。此操作不可恢复，确定删除？`,
      { title: '删除当前配置档', okText: '切换并删除', danger: true },
    );
    if (!confirmed) {
      return;
    }
    clearPreviewTimer();
    try {
      await invoke('switch_profile', { name: fallback });
      clearLocalEdits();
      await invoke('delete_profile', { name });
      await loadAll();
      setStatus(`已切换回「${fallback}」并删除配置档「${name}」`, 'ok');
    } catch (error) {
      await loadAll();
      showError(`删除配置档失败: ${error}`);
      setStatus('删除失败', 'err');
    }
    return;
  }

  const confirmed = await uiConfirm(`删除配置档「${name}」？此操作不可恢复。`, {
    title: '删除配置档',
    okText: '删除',
    danger: true,
  });
  if (!confirmed) return;
  try {
    await invoke('delete_profile', { name });
    state.profiles = await invoke('get_profiles');
    renderProfileBar();
    setStatus(`已删除配置档「${name}」`, 'ok');
  } catch (error) {
    showError(`删除配置档失败: ${error}`);
  }
}

// ---------------- 导航 / 搜索 ----------------

function switchToGroup(groupId) {
  const groups = state.schema?.groups || [];
  if (!groups.some((group) => group.id === groupId)) return;
  state.currentGroup = groupId;
  els.search.value = '';
  renderNav();
  renderContent();
  updateDirtyIndicator();
}

function renderNav() {
  const groups = state.schema?.groups || [];
  els.nav.innerHTML = groups
    .map(
      (group) =>
        `<button class="nav-item${group.id === state.currentGroup ? ' active' : ''}" data-group="${escapeHtml(group.id)}">` +
        `${escapeHtml(group.title)}</button>`,
    )
    .join('');
  els.nav.querySelectorAll('.nav-item').forEach((button) => {
    button.addEventListener('click', () => {
      switchToGroup(button.dataset.group);
    });
  });
}

function renderContent() {
  const query = els.search.value.trim().toLowerCase();
  if (query) {
    renderSearch(query);
  } else {
    renderGroup(currentGroup());
  }
}

function renderGroup(group) {
  if (!group) {
    els.content.innerHTML = '<div class="hint">没有可显示的设置分组</div>';
    return;
  }
  const disabled =
    (group.id === 'steamvr' && !state.capabilities.steamvr) ||
    (group.id === 'osc' && !state.capabilities.osc);

  const header = `
    <div class="group-header">
      <h2>${escapeHtml(group.title)}</h2>
      <p>${escapeHtml(group.description)}</p>
    </div>`;

  let note = '';
  if (disabled) {
    note = `<div class="capability-note">当前构建为<b>标准版</b>，未包含 ${
      group.id === 'steamvr' ? 'SteamVR' : 'OSC'
    } 支持。该分组已置灰，请下载 Full 版后再配置。</div>`;
  }

  let body = '';
  if (group.custom === 'sources_table') {
    body += '<div id="sources-block"></div>';
  }
  if (group.custom === 'raw_toml') {
    body += renderRawGroup();
  }

  const normalFields = (group.fields || []).filter((field) => !field.advanced);
  const advancedFields = (group.fields || []).filter((field) => field.advanced);
  body += normalFields.map(renderField).join('');
  if (advancedFields.length > 0) {
    body += `<details class="advanced"><summary>高级选项（${advancedFields.length}）</summary>${advancedFields
      .map(renderField)
      .join('')}</details>`;
  }

  els.content.innerHTML = `<div class="${disabled ? 'section-disabled' : ''}">${header}${note}${body}</div>`;

  if (group.custom === 'sources_table') {
    renderSourcesBlock(disabled);
  }
  if (group.custom === 'raw_toml') {
    bindRawGroup();
  }
  bindFieldEvents();
  if (disabled) {
    els.content
      .querySelectorAll('input, select, textarea, button')
      .forEach((element) => {
        element.disabled = true;
      });
  }
}

function renderSearch(query) {
  const matches = [];
  for (const group of state.schema?.groups || []) {
    for (const field of group.fields || []) {
      const haystack = `${field.label} ${field.help} ${field.path} ${group.title}`.toLowerCase();
      if (haystack.includes(query)) {
        matches.push({ group, field });
      }
    }
  }
  if (matches.length === 0) {
    els.content.innerHTML = `<div class="group-header"><h2>搜索：${escapeHtml(query)}</h2><p>没有匹配的设置项</p></div>`;
    return;
  }
  const disabledGroups = new Set();
  if (!state.capabilities.steamvr) disabledGroups.add('steamvr');
  if (!state.capabilities.osc) disabledGroups.add('osc');

  els.content.innerHTML = `
    <div class="group-header"><h2>搜索：${escapeHtml(query)}</h2><p>匹配 ${matches.length} 项</p></div>
    ${matches
      .map(
        ({ group, field }) =>
          `<div class="${disabledGroups.has(group.id) ? 'section-disabled' : ''}"><div class="hint">${escapeHtml(
            group.title,
          )}</div>${renderField(field)}</div>`,
      )
      .join('')}`;
  bindFieldEvents();
  els.content
    .querySelectorAll(
      '.section-disabled input, .section-disabled select, .section-disabled textarea, .section-disabled button',
    )
    .forEach((element) => {
      element.disabled = true;
    });
}

// ---------------- 字段渲染 ----------------

function fieldValue(field) {
  return state.dirty.has(field.path) ? state.dirty.get(field.path) : field.value;
}

function renderField(field) {
  const value = fieldValue(field);
  const restart = field.restart_required
    ? '<span class="restart-tag">需重启采集</span>'
    : '';
  const label = `
    <div class="field-label">
      <span class="label">${escapeHtml(field.label)}${restart}</span>
      ${field.help ? `<span class="help">${escapeHtml(field.help)}</span>` : ''}
      <span class="help">${escapeHtml(field.path)}</span>
    </div>`;
  return `<div class="field" data-field-path="${escapeHtml(field.path)}">${label}<div class="field-control">${renderControl(
    field,
    value,
  )}</div></div>`;
}

function renderControl(field, value) {
  const path = escapeHtml(field.path);
  if (Array.isArray(value)) {
    const rawValues = state.rawInputs.get(field.path);
    return value
      .map((item, index) => {
        const displayItem =
          Array.isArray(rawValues) && index < rawValues.length ? rawValues[index] : item;
        const invalidClass =
          state.invalidInputs.has(field.path) && displayItem === '' ? ' invalid' : '';
        return `<input type="number" class="array-input${invalidClass}" data-path="${path}" data-array-index="${index}" step="0.01" ${
          field.min != null ? `min="${field.min}"` : ''
        } ${field.max != null ? `max="${field.max}"` : ''} value="${escapeHtml(
          displayItem,
        )}" />`;
      })
      .join('');
  }
  switch (field.kind) {
    case 'bool':
      return `<input type="checkbox" data-path="${path}" ${value ? 'checked' : ''} />`;
    case 'int':
    case 'float':
    case 'number': {
      const displayValue = state.rawInputs.has(field.path)
        ? state.rawInputs.get(field.path)
        : value;
      const invalidAttr = state.invalidInputs.has(field.path) ? ' class="invalid"' : '';
      return `<input type="number"${invalidAttr} data-path="${path}" value="${escapeHtml(displayValue)}" ${
        field.min != null ? `min="${field.min}"` : ''
      } ${field.max != null ? `max="${field.max}"` : ''} ${
        field.step != null ? `step="${field.step}"` : ''
      } />`;
    }
    case 'secret':
      // 默认遮蔽：密钥不该在不经意间被旁人/录屏看到
      return `<input type="password" data-path="${path}" value="${escapeHtml(value)}" spellcheck="false" autocomplete="off" />
        <button class="btn small" data-secret-toggle="${path}">显示</button>`;
    case 'enum': {
      const options = field.options || [];
      const matched = options.some((option) => String(option.value) === String(value));
      const invalidOption =
        !matched && value != null && String(value) !== ''
          ? `<option value="${escapeHtml(value)}" selected>${escapeHtml(value)}（无效）</option>`
          : '';
      return `<select data-path="${path}" class="${matched ? '' : 'invalid'}">${invalidOption}${(options
        .map(
          (option) =>
            `<option value="${escapeHtml(option.value)}"${
              matched && String(option.value) === String(value) ? ' selected' : ''
            }>` +
            `${escapeHtml(option.label)}</option>`,
        )
        .join(''))}</select>`;
    }
    case 'multiline':
      return `<textarea data-path="${path}" rows="3">${escapeHtml(value)}</textarea>`;
    case 'toml':
      return `<textarea data-path="${path}" data-toml="1" rows="6">${escapeHtml(
        typeof value === 'string' ? value : objectToToml(value),
      )}</textarea>`;
    case 'color': {
      const text = String(value || '');
      const picker = /^#[0-9a-fA-F]{6}/.test(text) ? text.slice(0, 7) : '#ffffff';
      return `<span class="value-preview" data-color-preview="${path}" style="background:${cssColor(
        text,
      )}"></span>
        <input type="text" data-path="${path}" value="${escapeHtml(text)}" placeholder="#RRGGBBAA" />
        <input type="color" data-color-picker="${path}" value="${picker}" />`;
    }
    case 'device':
      return `<select data-path="${path}">${renderDeviceOptions(value)}</select>`;
    case 'language':
      return `<select data-path="${path}">${renderLanguageOptions(value, field.options)}</select>`;
    default: {
      const displayValue =
        field.path === 'output.osc.separator' ? escapeControlChars(value) : value;
      return `<input type="text" data-path="${path}" value="${escapeHtml(displayValue)}" />`;
    }
  }
}

/**
 * 只允许纯色语法进入 inline style。
 * HTML 转义挡不住 `red; position:fixed; inset:0` 这种值（可能来自共享/导入的 TOML），
 * 那样会盖住整个设置页。
 */
function cssColor(value) {
  const text = String(value || '').trim();
  if (/^#[0-9a-fA-F]{3,8}$/.test(text)) return text;
  // 允许 CSS Color 4 空格分隔写法，只去掉分号以防注入多声明
  if (/^rgba?\([\d.,%\s/]+\)$/.test(text)) return text.replace(/;/g, '');
  return 'transparent';
}

function valuesEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

function baselineForPath(path) {
  if (path === 'audio.sources') {
    try {
      return JSON.parse(state.originalSourcesJson);
    } catch (_error) {
      return [];
    }
  }
  return findField(path)?.value;
}

function setFieldDirty(path, value) {
  if (valuesEqual(value, baselineForPath(path))) {
    state.dirty.delete(path);
  } else {
    state.dirty.set(path, value);
  }
}

function renderDeviceOptions(current) {
  const entries = state.devices.map((device) => ({
    value: device.spec,
    label: `${device.kind} | ${device.name}`,
  }));
  if (!entries.some((entry) => entry.value === current)) {
    entries.unshift({ value: current || 'default', label: current || 'default' });
  }
  return entries
    .map(
      (entry) =>
        `<option value="${escapeHtml(entry.value)}"${entry.value === current ? ' selected' : ''}>` +
        `${escapeHtml(entry.label)}</option>`,
    )
    .join('');
}

function renderLanguageOptions(current, fieldOptions) {
  const entries =
    Array.isArray(fieldOptions) && fieldOptions.length > 0
      ? fieldOptions.map((option) => ({ value: option.value, label: option.label }))
      : state.languages.map((language) => ({
          value: language.code,
          label: language.label,
        }));
  if (current && !entries.some((entry) => entry.value === current)) {
    entries.unshift({ value: current, label: current });
  }
  return entries
    .map(
      (entry) =>
        `<option value="${escapeHtml(entry.value)}"${entry.value === current ? ' selected' : ''}>` +
        `${escapeHtml(entry.label)}</option>`,
    )
    .join('');
}

function formatTomlKey(key) {
  const text = String(key);
  return /^[A-Za-z0-9_-]+$/.test(text) ? text : JSON.stringify(text);
}

function objectToToml(value) {
  if (value == null) return '';
  if (typeof value !== 'object') return String(value);
  return Object.entries(value)
    .map(([key, item]) => `${formatTomlKey(key)} = ${tomlInline(item)}`)
    .join('\n');
}

function tomlInline(value) {
  if (Array.isArray(value)) {
    return `[${value.map(tomlInline).join(', ')}]`;
  }
  if (value && typeof value === 'object') {
    return `{ ${Object.entries(value)
      .map(([key, item]) => `${formatTomlKey(key)} = ${tomlInline(item)}`)
      .join(', ')} }`;
  }
  if (typeof value === 'string') return JSON.stringify(value);
  return String(value);
}

// ---------------- 输入源表格 ----------------

function effectiveAsrBackend() {
  if (state.dirty.has('asr.backend')) {
    return String(state.dirty.get('asr.backend') || '');
  }
  const backendField = findField('asr.backend');
  return String(backendField?.value || 'openai_compatible');
}

function effectiveSourceLanguageOptions() {
  const backend = effectiveAsrBackend();
  const fieldPath = backend === 'edge' ? 'asr.edge.language' : 'asr.language';
  const field = findField(fieldPath);
  if (Array.isArray(field?.options) && field.options.length > 0) {
    return field.options.map((option) => ({ code: option.value, label: option.label }));
  }
  return state.languages;
}

function renderSourcesBlock(disabled) {
  const block = document.getElementById('sources-block');
  if (!block) return;
  const candidateLanguages = effectiveSourceLanguageOptions();
  const rows = state.sources
    .map((source, index) => {
      const languageOptions = candidateLanguages.map(
        (language) =>
          `<option value="${escapeHtml(language.code)}"${
            language.code === source.language ? ' selected' : ''
          }>${escapeHtml(language.label)}</option>`,
      );
      if (
        source.language &&
        !candidateLanguages.some((language) => language.code === source.language)
      ) {
        languageOptions.unshift(
          `<option value="${escapeHtml(source.language)}" selected>${escapeHtml(
            source.language,
          )}</option>`,
        );
      }
      const languages = [
        `<option value=""${!source.language ? ' selected' : ''}>跟随全局</option>`,
        ...languageOptions,
      ].join('');
      return `<tr data-index="${index}">
        <td><input type="text" data-source-field="id" value="${escapeHtml(source.id)}" /></td>
        <td><input type="text" data-source-field="label" value="${escapeHtml(source.label)}" /></td>
        <td><select data-source-field="device">${renderDeviceOptions(source.device)}</select></td>
        <td><select data-source-field="language">${languages}</select></td>
        <td class="col-small"><input type="checkbox" data-source-field="translate" ${
          source.translate ? 'checked' : ''
        } /></td>
        <td class="col-small"><input type="checkbox" data-source-field="enabled" ${
          source.enabled ? 'checked' : ''
        } /></td>
        <td class="col-small">
          <button class="btn icon-btn" data-source-move="up" ${index === 0 ? 'disabled' : ''}>↑</button>
          <button class="btn icon-btn" data-source-move="down" ${
            index === state.sources.length - 1 ? 'disabled' : ''
          }>↓</button>
          <button class="btn icon-btn danger" data-source-remove="1">✕</button>
        </td>
      </tr>`;
    })
    .join('');

  block.innerHTML = `
    <div class="sources-error" id="sources-error"></div>
    <table class="sources-table">
      <thead><tr>
        <th>ID</th><th>展示名</th><th>设备</th><th>语言覆盖</th>
        <th class="col-small">翻译</th><th class="col-small">启用</th><th class="col-small">操作</th>
      </tr></thead>
      <tbody>${rows || ''}</tbody>
    </table>
    <div class="sources-actions">
      <button class="btn small" id="add-source-row">＋ 添加输入源</button>
      <span class="hint">修改后点右下角「保存」写入配置；设备 / 启用变更会重启采集；
        同一设备只能被一个启用中的源采集（loopback 与 loopback:0 是同一个设备）。</span>
    </div>`;

  if (disabled) {
    block.querySelectorAll('input, select, button').forEach((element) => {
      element.disabled = true;
    });
  }

  const tbody = block.querySelector('tbody');
  tbody.addEventListener('input', (event) => {
    const row = event.target.closest('tr[data-index]');
    const field = event.target.dataset.sourceField;
    if (!row || !field) return;
    const source = state.sources[Number(row.dataset.index)];
    if (!source) return;
    source[field] = event.target.type === 'checkbox' ? event.target.checked : event.target.value;
    markSourcesDirty();
  });
  tbody.addEventListener('change', (event) => {
    const row = event.target.closest('tr[data-index]');
    const field = event.target.dataset.sourceField;
    if (!row || !field) return;
    const source = state.sources[Number(row.dataset.index)];
    if (!source) return;
    source[field] = event.target.type === 'checkbox' ? event.target.checked : event.target.value;
    markSourcesDirty();
  });
  tbody.addEventListener('click', (event) => {
    const row = event.target.closest('tr[data-index]');
    if (!row) return;
    const index = Number(row.dataset.index);
    if (event.target.dataset.sourceRemove) {
      state.sources.splice(index, 1);
      markSourcesDirty();
      renderSourcesBlock(disabled);
    } else if (event.target.dataset.sourceMove === 'up' && index > 0) {
      [state.sources[index - 1], state.sources[index]] = [
        state.sources[index],
        state.sources[index - 1],
      ];
      markSourcesDirty();
      renderSourcesBlock(disabled);
    } else if (
      event.target.dataset.sourceMove === 'down' &&
      index < state.sources.length - 1
    ) {
      [state.sources[index + 1], state.sources[index]] = [
        state.sources[index],
        state.sources[index + 1],
      ];
      markSourcesDirty();
      renderSourcesBlock(disabled);
    }
  });

  document.getElementById('add-source-row').addEventListener('click', () => {
    let n = state.sources.length + 1;
    let id = `source${n}`;
    while (state.sources.some((source) => source.id === id)) {
      n += 1;
      id = `source${n}`;
    }
    // 默认挑一个没被其它源占用的设备：复用 devices[0] 会立刻触发
    // “设备被多个启用中的输入源占用”，导致整页任何字段都存不进去。
    const used = new Set(state.sources.map((source) => String(source.device || '')));
    const freeDevice = (state.devices || []).find((device) => !used.has(device.spec));
    state.sources.push({
      id,
      label: `输入源 ${n}`,
      device: freeDevice?.spec || '',
      enabled: Boolean(freeDevice),
      translate: true,
      language: '',
    });
    markSourcesDirty();
    renderSourcesBlock(disabled);
  });
  validateSources();
}

function markSourcesDirty() {
  setFieldDirty('audio.sources', deepClone(state.sources));
  updateDirtyIndicator();
  validateSources();
}

/**
 * 输入源校验（纯函数）。
 * 不依赖 DOM：设置页切到别的分组时 #sources-error 不在文档里，
 * 旧实现会因此返回“无错误”，把非法配置直接交给后端。
 */
function collectSourceErrors(sources) {
  const errors = [];
  if (!Array.isArray(sources) || sources.length === 0) {
    errors.push('至少需要保留 1 个输入源（不需要采集时可取消勾选「启用」）');
    return errors;
  }
  const ids = new Set();
  const devices = new Set();
  sources.forEach((source, index) => {
    const row = `输入源 #${index + 1}`;
    const id = String(source.id || '');
    if (!id) errors.push(`${row}: ID 不能为空`);
    if (!/^[A-Za-z0-9_-]{1,32}$/.test(id)) {
      errors.push(`${row}: ID「${id}」只能包含字母、数字、下划线、短横线，且不超过 32 字符`);
    }
    if (ids.has(id)) errors.push(`${row}: ID「${id}」重复`);
    ids.add(id);
    if (!String(source.label || '').trim()) errors.push(`${row}: 展示名不能为空`);
    const device = String(source.device || '').trim();
    if (source.enabled && !device) errors.push(`${row}: 启用中的输入源必须指定设备`);
    if (source.enabled && device) {
      // 与后端 canonical_device_spec 一致：loopback 与 loopback:0 是同一个端点
      const key = canonicalDeviceSpec(device);
      if (devices.has(key)) {
        errors.push(`${row}: 设备「${device}」被多个启用中的输入源占用`);
      }
      devices.add(key);
    }
    if (
      effectiveAsrBackend() === 'edge' &&
      String(source.language || '').trim().toLowerCase() === 'auto'
    ) {
      errors.push(`${row}: Edge 语音识别不支持将语言覆盖设为 auto，请选择具体语言或跟随全局`);
    }
  });
  return errors;
}

/**
 * 归一化设备 spec，仅用于「同一设备」判重。
 * 与后端 `config::canonical_device_spec` 保持一致。
 */
function canonicalDeviceSpec(spec) {
  const value = String(spec || '')
    .trim()
    .toLowerCase();
  if (value === '' || value === 'default') return 'default';
  if (value === 'mic' || value === 'microphone') return 'mic:0';
  if (value === 'loopback') return 'loopback:0';
  return value;
}

function validateSources() {
  const errors = collectSourceErrors(state.sources);
  const box = document.getElementById('sources-error');
  if (box) box.textContent = errors.join('；');
  return errors;
}

// ---------------- 原始 TOML ----------------

function renderRawGroup() {
  return `
    <div class="field">
      <div class="field-label"><span class="label">配置文件</span>
        <span class="help">${escapeHtml(state.rawPath || '首次保存时创建')}</span></div>
      <div class="field-control">
        <button class="btn small" id="open-config-file">打开配置文件</button>
        <button class="btn small" id="open-config-dir">打开配置目录</button>
      </div>
    </div>
    <textarea id="raw-editor" class="raw-editor" spellcheck="false">${escapeHtml(state.rawText)}</textarea>
    <div class="raw-actions">
      <button class="btn primary" id="save-raw-btn">保存原始配置</button>
      <span class="hint">保存前会完整解析并校验，非法内容不会落盘。</span>
    </div>`;
}

async function saveRawConfig() {
  if (state.dirty.size > 0 || state.invalidInputs.size > 0) {
    const confirmed = await uiConfirm(
      '保存原始 TOML 将覆盖其它分组尚未保存的表单修改，继续？',
      { title: '覆盖未保存修改', okText: '继续保存', danger: true },
    );
    if (!confirmed) {
      return;
    }
  }
  clearPreviewTimer();
  state.previewToken += 1;
  const saveRawBtn = document.getElementById('save-raw-btn');
  if (saveRawBtn) saveRawBtn.disabled = true;
  setBusy(true);
  try {
    const result = await invoke('save_config_raw', { text: state.rawText });
    clearLocalEdits();
    state.originalRawText = state.rawText;
    showError('');
    if (result.restart_required) {
      els.restartBanner.classList.remove('hidden');
    } else {
      els.restartBanner.classList.add('hidden');
    }
    setStatus(result.message || '已保存', 'ok');
    await refreshSchemaOnly();
    updateDirtyIndicator();
  } catch (error) {
    showError(`保存失败: ${error}`);
    setStatus('保存失败', 'err');
  } finally {
    if (saveRawBtn) saveRawBtn.disabled = false;
    setBusy(false);
  }
}

function bindRawGroup() {
  const editor = document.getElementById('raw-editor');
  if (editor) {
    editor.addEventListener('input', () => {
      state.rawText = editor.value;
      state.rawDirty = editor.value !== state.originalRawText;
      updateDirtyIndicator();
    });
  }
  const openFile = document.getElementById('open-config-file');
  if (openFile) {
    openFile.addEventListener('click', () => {
      invoke('open_config_file').catch((error) => showError(`打开配置文件失败: ${error}`));
    });
  }
  const openDir = document.getElementById('open-config-dir');
  if (openDir) {
    openDir.addEventListener('click', () => {
      invoke('open_config_folder').catch((error) => showError(`打开目录失败: ${error}`));
    });
  }
  const saveRaw = document.getElementById('save-raw-btn');
  if (saveRaw) {
    saveRaw.addEventListener('click', saveRawConfig);
  }
}

// ---------------- 字段事件 / 脏数据 ----------------

function bindFieldEvents() {
  els.content.querySelectorAll('[data-path]').forEach((element) => {
    const handler = () => {
      const path = element.dataset.path;
      let value;
      if (element.dataset.arrayIndex !== undefined) {
        const inputs = Array.from(
          els.content.querySelectorAll(`[data-path="${CSS.escape(path)}"][data-array-index]`),
        );
        const values = inputs.map((input) => input.value);
        const hasBad = inputs.some(
          (input) => (input.validity && input.validity.badInput) || input.value === '',
        );
        inputs.forEach((input) => {
          input.classList.toggle(
            'invalid',
            Boolean((input.validity && input.validity.badInput) || input.value === ''),
          );
        });
        if (hasBad) {
          state.rawInputs.set(path, values);
          state.invalidInputs.set(path, findField(path)?.label || path);
          if (state.dirty.delete(path)) {
            schedulePreview(path);
          }
          updateDirtyIndicator();
          return;
        }
        state.rawInputs.delete(path);
        state.invalidInputs.delete(path);
        value = values.map(Number);
      } else if (element.type === 'checkbox') {
        value = element.checked;
      } else if (element.type === 'number') {
        // 清空或非法数字输入框时持久记录在 rawInputs/invalidInputs，切换分组或搜索也不会静默复原通过保存
        if ((element.validity && element.validity.badInput) || element.value === '') {
          element.classList.add('invalid');
          state.rawInputs.set(path, '');
          state.invalidInputs.set(path, findField(path)?.label || path);
          if (state.dirty.delete(path)) {
            schedulePreview(path);
          }
          updateDirtyIndicator();
          return;
        }
        element.classList.remove('invalid');
        state.rawInputs.delete(path);
        state.invalidInputs.delete(path);
        value = Number(element.value);
      } else {
        value =
          path === 'output.osc.separator' ? unescapeControlChars(element.value) : element.value;
      }

      setFieldDirty(path, value);
      updateDirtyIndicator();

      if (element.type === 'text' && path && isColorPath(path)) {
        updateColorPreview(path, element.value);
        const trimmed = element.value.trim();
        if (/^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(trimmed)) {
          const picker = els.content.querySelector(`[data-color-picker="${CSS.escape(path)}"]`);
          if (picker) picker.value = trimmed.slice(0, 7);
        }
      }

      schedulePreview(path);
    };
    element.addEventListener(
      element.type === 'checkbox' || element.tagName === 'SELECT' ? 'change' : 'input',
      handler,
    );
  });

  // 颜色选择器：同步到同路径的文本框（原值含 #RRGGBBAA 时保留 alpha 分量）
  els.content.querySelectorAll('[data-color-picker]').forEach((picker) => {
    picker.addEventListener('input', () => {
      const path = picker.dataset.colorPicker;
      const text = els.content.querySelector(`[data-path="${CSS.escape(path)}"]`);
      const current = text ? text.value.trim() : '';
      const alpha = /^#[0-9a-fA-F]{8}$/.test(current) ? current.slice(7, 9) : '';
      const nextColor = `${picker.value}${alpha}`;
      if (text) {
        text.value = nextColor;
      }
      setFieldDirty(path, nextColor);
      updateColorPreview(path, nextColor);
      updateDirtyIndicator();
      schedulePreview(path);
    });
  });

  els.content.querySelectorAll('[data-secret-toggle]').forEach((button) => {
    button.addEventListener('click', () => {
      const path = button.dataset.secretToggle;
      const input = els.content.querySelector(`[data-path="${CSS.escape(path)}"]`);
      if (!input) return;
      const hidden = input.type === 'password';
      input.type = hidden ? 'text' : 'password';
      button.textContent = hidden ? '隐藏' : '显示';
    });
  });
}

function isColorPath(path) {
  return /(color|background)$/.test(path);
}

function updateColorPreview(path, value) {
  const preview = els.content.querySelector(`[data-color-preview="${CSS.escape(path)}"]`);
  if (preview) {
    preview.style.background = cssColor(value);
  }
}

function updateDirtyIndicator() {
  const count = dirtyCount();
  els.save.textContent = count > 0 ? `保存（${count} 项修改）` : '保存';
  els.save.classList.toggle('primary', count > 0);
  const wasDirty = state.wasDirty === true;
  state.wasDirty = count > 0;
  if (count > 0 && !wasDirty) {
    setStatus(`有 ${count} 项未保存的修改`);
  } else if (count === 0 && wasDirty) {
    setStatus('就绪');
  } else if (count > 0) {
    // 计数变化时刷新文案（旧实现会永久冻结在第一次的数字上）
    els.status.textContent = `有 ${count} 项未保存的修改`;
  }
}

// 实时预览只覆盖浮窗视觉（subtitle.*）；output.* 在后端故意不预览，
// 避免未保存就连上 VR / 往 VRChat 发包，所以也别往里发无效的 preview 请求。
function isPreviewablePath(path) {
  return Boolean(path && path.startsWith('subtitle.'));
}

// 保存/重载失败后脏值仍在，但预览已被 cancel_preview 回滚，需要重新推回浮窗
function reschedulePreview() {
  if (state.busy) return;
  const previewable = [...state.dirty.keys()].find(isPreviewablePath);
  if (previewable) schedulePreview(previewable);
}

function schedulePreview(changedPath) {
  if (!isPreviewablePath(changedPath) || state.busy) return;
  clearPreviewTimer();
  const token = ++state.previewToken;
  state.previewTimer = setTimeout(async () => {
    state.previewTimer = null;
    if (token !== state.previewToken || state.busy) return;
    const changes = [...state.dirty]
      .filter(([path]) => isPreviewablePath(path))
      .map(([path, value]) => ({ path, value }));
    if (changes.length === 0) {
      await invoke('cancel_preview').catch((error) => {
        console.warn('cancel_preview 失败', error);
      });
      return;
    }
    try {
      await invoke('preview_settings', { changes });
    } catch (error) {
      // 预览失败不阻塞编辑
      console.warn('preview_settings 失败', error);
    }
  }, 300);
}

// ---------------- 保存 / 取消 / 重载 ----------------

function findField(path) {
  const normalized = path.replace(/\[\]$/, '');
  for (const group of state.schema?.groups || []) {
    for (const field of group.fields || []) {
      if (field.path === path || field.path === normalized) return field;
    }
  }
  return null;
}

/**
 * 数值字段的本地校验：HTML 的 min/max 只约束步进箭头，手输仍然可以越界。
 * 一条非法值会让后端整批保存失败，用户还得逐项排查。
 */
function validateNumbers(changes) {
  const errors = [...state.invalidInputs.values()].map((label) => `${label} 请输入有效数字`);
  for (const change of changes) {
    const field = findField(change.path);
    if (!field || !['int', 'float', 'number'].includes(field.kind)) continue;
    const value = change.value;
    // world_position[] / line_alphas[] 这类字段的 dirty 值是整个数组，
    // 之前这里按标量判断，导致数组字段一改就「必须是数字」，整批保存被挡。
    if (Array.isArray(value)) {
      if (value.some((item) => typeof item !== 'number' || !Number.isFinite(item))) {
        errors.push(`${field.label} 必须是数字`);
        continue;
      }
      if (field.min != null && value.some((item) => item < field.min)) {
        errors.push(`${field.label} 不能小于 ${field.min}`);
      }
      if (field.max != null && value.some((item) => item > field.max)) {
        errors.push(`${field.label} 不能大于 ${field.max}`);
      }
      continue;
    }
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      errors.push(`${field.label} 必须是数字`);
      continue;
    }
    if (field.kind === 'int' && !Number.isInteger(value)) {
      errors.push(`${field.label} 必须是整数`);
    }
    if (field.min != null && value < field.min) {
      errors.push(`${field.label} 不能小于 ${field.min}`);
    }
    if (field.max != null && value > field.max) {
      errors.push(`${field.label} 不能大于 ${field.max}`);
    }
  }
  return errors;
}

async function save() {
  if (
    state.currentGroup === 'advanced' &&
    state.rawDirty &&
    state.dirty.size === 0 &&
    state.invalidInputs.size === 0
  ) {
    await saveRawConfig();
    return;
  }
  const changes = [...state.dirty].map(([path, value]) => ({ path, value }));
  const errors = [...collectSourceErrors(state.sources), ...validateNumbers(changes)];
  if (errors.length > 0) {
    showError(`校验失败：${errors.join('；')}`);
    setStatus('校验失败', 'err');
    return;
  }
  if (changes.length === 0) {
    if (state.rawDirty) {
      await saveRawConfig();
      return;
    }
    // 降级保护态下磁盘已是合法恢复配置，但仍需一次空保存来 clear_config_degraded
    if (state.schema?.warning) {
      clearPreviewTimer();
      state.previewToken += 1;
      let saved = false;
      setBusy(true);
      try {
        const result = await invoke('save_settings', { changes: [] });
        saved = true;
        clearLocalEdits();
        setStatus(result.message || '已确认恢复配置并退出保护态', 'ok');
        showError('');
        await refreshSchemaOnly();
        updateDirtyIndicator();
      } catch (error) {
        console.error(error);
        showError(`保存失败: ${error}`);
        setStatus('保存失败', 'err');
      } finally {
        setBusy(false);
        if (!saved) reschedulePreview();
      }
      return;
    }
    setStatus('没有需要保存的修改');
    return;
  }
  if (state.rawDirty) {
    const confirmed = await uiConfirm(
      '保存表单修改将丢弃「高级 / 原始配置」中尚未保存的 TOML 编辑，继续？',
      { title: '丢弃原始配置修改', okText: '继续保存', danger: true },
    );
    if (!confirmed) {
      return;
    }
  }

  clearPreviewTimer();
  state.previewToken += 1;
  let saved = false;
  setBusy(true);
  try {
    let result;
    try {
      result = await invoke('save_settings', { changes });
      saved = true;
    } catch (error) {
      console.error(error);
      showError(`保存失败: ${error}`);
      setStatus('保存失败', 'err');
      return;
    }
    clearLocalEdits();
    state.originalSourcesJson = JSON.stringify(state.sources);
    setStatus(result.message || '已保存', 'ok');
    showError('');
    if (result.restart_required) {
      els.restartBanner.classList.remove('hidden');
    } else {
      els.restartBanner.classList.add('hidden');
    }
    updateDirtyIndicator();
    // 刷新失败不能报成“保存失败”：配置已经落盘了
    try {
      await refreshSchemaOnly();
      updateDirtyIndicator();
    } catch (error) {
      console.warn(error);
      showError('已保存，但界面刷新失败，请点「重新加载」');
    }
  } finally {
    setBusy(false);
    if (!saved) reschedulePreview();
  }
}

async function cancel() {
  clearPreviewTimer();
  state.previewToken += 1;
  setBusy(true);
  try {
    clearLocalEdits();
    state.sources = JSON.parse(state.originalSourcesJson);
    try {
      await invoke('cancel_preview');
    } catch (error) {
      console.warn(error);
    }
    els.restartBanner.classList.add('hidden');
    showError('');
    try {
      await refreshSchemaOnly();
    } catch (error) {
      console.warn(error);
      showError('已恢复配置，但界面刷新失败，请点「重新加载」');
    }
    updateDirtyIndicator();
    setStatus('已放弃未保存的修改');
  } finally {
    setBusy(false);
  }
}

async function reload() {
  const count = dirtyCount();
  if (count > 0) {
    const confirmed = await uiConfirm(
      `当前有 ${count} 项未保存的修改，重新加载将丢弃它们。继续？`,
      { title: '重新加载配置', okText: '丢弃并重载', danger: true },
    );
    if (!confirmed) {
      return;
    }
  }
  clearPreviewTimer();
  state.previewToken += 1;
  let reloaded = false;
  setBusy(true);
  await invoke('cancel_preview').catch(() => {});
  try {
    const result = await invoke('reload_config');
    // 仅在后端重载成功后再清脏标记，失败时保留表单与「保存（N 项）」状态
    reloaded = true;
    clearLocalEdits();
    await loadAll();
    setStatus(result.message || '已重新加载', 'ok');
  } catch (error) {
    showError(`重新加载失败: ${error}`);
    updateDirtyIndicator();
  } finally {
    setBusy(false);
    // 重载失败时脏值仍在，但 cancel_preview 已经把浮窗回滚了，重新推一次预览
    if (!reloaded) reschedulePreview();
  }
}

// ---------------- 绑定 ----------------

function bindTopbar() {
  els.profileSelect.addEventListener('change', () => updateProfileActionButtons());
  if (els.profileSwitch) {
    els.profileSwitch.addEventListener('click', () => switchProfile(selectedProfile()));
  }
  els.profileNew.addEventListener('click', createProfile);
  els.profileDup.addEventListener('click', duplicateProfile);
  els.profileRename.addEventListener('click', renameProfile);
  els.profileDelete.addEventListener('click', deleteProfile);
  els.profileOpen.addEventListener('click', () =>
    invoke('open_profiles_dir').catch((error) => showError(`打开目录失败: ${error}`)),
  );

  els.search.addEventListener('input', () => {
    renderContent();
  });
  els.save.addEventListener('click', save);
  els.cancel.addEventListener('click', cancel);
  els.reload.addEventListener('click', reload);
  els.restartBtn.addEventListener('click', async () => {
    try {
      await invoke('restart_pipeline');
      els.restartBanner.classList.add('hidden');
      setStatus('已重启采集', 'ok');
    } catch (error) {
      showError(`重启采集失败: ${error}`);
    }
  });
}

async function reloadOnShow() {
  clearPreviewTimer();
  const count = dirtyCount();
  let keptDirty = false;
  if (count > 0) {
    const choice = await uiChoice(`当前有 ${count} 项未保存的修改，是否保留并继续编辑？`, {
      title: '未保存的修改',
      okText: '保留修改',
      cancelText: '丢弃并重新加载',
    });
    if (choice === null) {
      // 用户按 Esc 或直接关闭设置窗取消了确认：不继续重载、更不能重新下发预览
      return;
    }
    if (choice === false) {
      clearLocalEdits();
    } else {
      keptDirty = true;
    }
  }
  await loadAll();
  if (keptDirty) {
    const previewable = [...state.dirty.keys()].find(isPreviewablePath);
    if (previewable) {
      schedulePreview(previewable);
    }
  }
}

window.addEventListener('DOMContentLoaded', async () => {
  if (typeof invoke !== 'function') {
    document.body.innerHTML =
      '<div class="banner error">未检测到 Tauri API，请确认 build.withGlobalTauri = true。</div>';
    return;
  }
  bindTopbar();
  // 先挂好事件监听再探测初始可见性，消除首次打开设置窗恰逢异步探测期间的空白窗竞态
  if (window.__TAURI__?.event?.listen) {
    await window.__TAURI__.event.listen('settings://shown', (event) => {
      const targetGroup = event?.payload?.group || null;
      const wasVisible = Boolean(event?.payload?.was_visible);
      const task = async () => {
        if (!state.hasLoaded) {
          if (targetGroup) state.currentGroup = targetGroup;
          state.hasLoaded = true;
          await loadAll();
        } else if (!wasVisible) {
          if (targetGroup) {
            state.currentGroup = targetGroup;
            els.search.value = '';
          }
          await reloadOnShow();
        } else if (targetGroup && targetGroup !== state.currentGroup) {
          switchToGroup(targetGroup);
        }
      };
      task().catch((error) => {
        console.error(error);
        showError(`重新加载失败: ${error}`);
      });
    });
    await window.__TAURI__.event.listen('settings://hidden', () => {
      clearPreviewTimer();
      closeActiveDialog();
    });
    await window.__TAURI__.event.listen('source-status://update', async () => {
      if (!state.hasLoaded || state.dirty.has('audio.sources')) return;
      try {
        const sources = await invoke('get_sources_config');
        // await 期间用户可能已开始编辑输入源，需二次检查避免覆盖未保存改动
        if (state.dirty.has('audio.sources')) return;
        const fresh = deepClone(sources.sources || []);
        const nextJson = JSON.stringify(fresh);
        if (nextJson !== state.originalSourcesJson) {
          state.sources = fresh;
          state.originalSourcesJson = nextJson;
          if (state.currentGroup === 'sources' && !els.search.value) {
            renderContent();
          }
        }
      } catch (error) {
        console.warn('同步输入源配置失败', error);
      }
    });
  }
  // 设置窗口在 app 启动时就创建（hidden），通过后端命令判断可见性（不依赖 window allowlist），
  // 不可见时不做初始化查询，等第一次 settings://shown 再加载。
  const visible = await invoke('is_window_visible').catch(() => true);
  if (visible && !state.hasLoaded) {
    state.hasLoaded = true;
    await loadAll();
  } else if (!state.hasLoaded) {
    setStatus('就绪');
  }
});
