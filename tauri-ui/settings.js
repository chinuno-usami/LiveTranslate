// LiveTranslate 设置页（Tauri v1 + 静态前端，无构建链）
const tauriApi = window.__TAURI__ || {};
const invoke = tauriApi.invoke || (tauriApi.tauri && tauriApi.tauri.invoke);

const els = {
  profileSelect: document.getElementById('profile-select'),
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
};

const state = {
  schema: null,
  capabilities: { steamvr: false, osc: false },
  profiles: null,
  rawPath: null,
  rawText: '',
  devices: [],
  languages: [],
  sources: [],
  currentGroup: 'sources',
  dirty: new Map(),
  originalSourcesJson: '[]',
  previewTimer: null,
};

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
    state.rawText = raw.text || '';
    state.sources = deepClone(sources.sources || []);
    state.originalSourcesJson = JSON.stringify(state.sources);
    state.devices = devices.devices || [];
    state.languages = languages.options || [];
    state.dirty.clear();

    els.configPath.textContent = raw.path || '（内置默认配置，首次保存时创建）';
    renderProfileBar();
    renderNav();
    renderContent();
    updateDirtyIndicator();
    setStatus('就绪');
    els.restartBanner.classList.add('hidden');
  } catch (error) {
    console.error(error);
    showError(`加载设置失败: ${error}`);
    setStatus('加载失败', 'err');
  }
}

async function refreshSchemaOnly() {
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
  renderProfileBar();
  state.rawPath = raw.path;
  state.rawText = raw.text || '';
  state.sources = deepClone(sources.sources || []);
  state.originalSourcesJson = JSON.stringify(state.sources);
  state.devices = devices.devices || [];
  state.languages = languages.options || [];
  renderContent();
}

// ---------------- 配置档 ----------------

function renderProfileBar() {
  const status = state.profiles || { enabled: false, profiles: [], active: null };
  const options = (status.profiles || [])
    .map(
      (profile) =>
        `<option value="${escapeHtml(profile.name)}"${profile.name === status.active ? ' selected' : ''}>` +
        `${escapeHtml(profile.name)}${profile.valid ? '' : '（无效）'}</option>`,
    )
    .join('');
  els.profileSelect.innerHTML = options || '<option value="">（无）</option>';
  els.profileSelect.disabled = !status.enabled;

  for (const button of [
    els.profileNew,
    els.profileDup,
    els.profileRename,
    els.profileDelete,
    els.profileOpen,
  ]) {
    button.disabled = !status.enabled;
  }
  els.profileReason.textContent = status.enabled
    ? ''
    : status.reason || '当前配置路径不支持配置档';
}

function selectedProfile() {
  return els.profileSelect.value || null;
}

async function switchProfile(name) {
  if (!name) return;
  if (state.dirty.size > 0) {
    const ok = window.confirm(
      `当前有 ${state.dirty.size} 项未保存的修改，切换配置档将丢弃它们。继续？`,
    );
    if (!ok) {
      renderProfileBar();
      return;
    }
  }
  if (!window.confirm(`切换到配置档「${name}」将重启采集并替换当前配置，继续？`)) {
    renderProfileBar();
    return;
  }
  try {
    const result = await invoke('switch_profile', { name });
    setStatus(result.message || '已切换配置档', 'ok');
    state.profiles = await invoke('get_profiles');
    renderProfileBar();
    // 旧配置档的未保存编辑不能带进新配置档
    state.dirty.clear();
    await refreshSchemaOnly();
    updateDirtyIndicator();
  } catch (error) {
    console.error(error);
    showError(`切换配置档失败: ${error}`);
    renderProfileBar();
  }
}

async function createProfile() {
  const name = window.prompt('新配置档名（1–40 字符，字母/数字/中文/_/-）');
  if (!name) return;
  const copyCurrent = window.confirm('确定 = 从当前配置复制；取消 = 创建默认配置');
  try {
    await invoke('create_profile', { name, copyCurrent });
    state.profiles = await invoke('get_profiles');
    renderProfileBar();
    setStatus(`已创建配置档「${name}」`, 'ok');
  } catch (error) {
    showError(`创建配置档失败: ${error}`);
  }
}

async function duplicateProfile() {
  const from = selectedProfile();
  if (!from) return;
  const to = window.prompt(`复制「${from}」到新配置档名`);
  if (!to) return;
  try {
    await invoke('duplicate_profile', { from, to });
    state.profiles = await invoke('get_profiles');
    renderProfileBar();
    setStatus(`已复制为「${to}」`, 'ok');
  } catch (error) {
    showError(`复制配置档失败: ${error}`);
  }
}

async function renameProfile() {
  const old = selectedProfile();
  if (!old) return;
  const next = window.prompt(`把「${old}」重命名为`, old);
  if (!next || next === old) return;
  try {
    await invoke('rename_profile', { old, new: next });
    state.profiles = await invoke('get_profiles');
    renderProfileBar();
    setStatus('已重命名配置档', 'ok');
  } catch (error) {
    showError(`重命名失败: ${error}`);
  }
}

async function deleteProfile() {
  const name = selectedProfile();
  if (!name) return;
  if (!window.confirm(`删除配置档「${name}」？此操作不可恢复。`)) return;
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
      state.currentGroup = button.dataset.group;
      els.search.value = '';
      renderNav();
      renderContent();
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
  els.content.querySelectorAll('.section-disabled input, .section-disabled select, .section-disabled textarea').forEach((element) => {
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
    return value
      .map(
        (item, index) =>
          `<input type="number" class="array-input" data-path="${path}" data-array-index="${index}" step="0.01" value="${escapeHtml(
            item,
          )}" />`,
      )
      .join('');
  }
  switch (field.kind) {
    case 'bool':
      return `<input type="checkbox" data-path="${path}" ${value ? 'checked' : ''} />`;
    case 'int':
    case 'float':
    case 'number':
      return `<input type="number" data-path="${path}" value="${escapeHtml(value)}" ${
        field.min != null ? `min="${field.min}"` : ''
      } ${field.max != null ? `max="${field.max}"` : ''} ${
        field.step != null ? `step="${field.step}"` : ''
      } />`;
    case 'secret':
      // 默认遮蔽：密钥不该在不经意间被旁人/录屏看到
      return `<input type="password" data-path="${path}" value="${escapeHtml(value)}" spellcheck="false" autocomplete="off" />
        <button class="btn small" data-secret-toggle="${path}">显示</button>`;
    case 'enum':
      return `<select data-path="${path}">${(field.options || [])
        .map(
          (option) =>
            `<option value="${escapeHtml(option.value)}"${String(option.value) === String(value) ? ' selected' : ''}>` +
            `${escapeHtml(option.label)}</option>`,
        )
        .join('')}</select>`;
    case 'multiline':
      return `<textarea data-path="${path}" rows="3">${escapeHtml(value)}</textarea>`;
    case 'toml':
      return `<textarea data-path="${path}" data-toml="1" rows="6">${escapeHtml(
        typeof value === 'string' ? value : objectToToml(value),
      )}</textarea>`;
    case 'color': {
      const text = String(value || '');
      const picker = /^#[0-9a-fA-F]{6}/.test(text) ? text.slice(0, 7) : '#ffffff';
      return `<span class="value-preview" data-color-preview="${path}" style="background:${escapeHtml(
        text,
      )}"></span>
        <input type="text" data-path="${path}" value="${escapeHtml(text)}" placeholder="#RRGGBBAA" />
        <input type="color" data-color-picker="${path}" value="${picker}" />`;
    }
    case 'device':
      return `<select data-path="${path}">${renderDeviceOptions(value)}</select>`;
    case 'language':
      return `<select data-path="${path}">${renderLanguageOptions(value)}</select>`;
    default:
      return `<input type="text" data-path="${path}" value="${escapeHtml(value)}" />`;
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

function renderLanguageOptions(current) {
  const entries = state.languages.map((language) => ({
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

function objectToToml(value) {
  if (value == null) return '';
  if (typeof value !== 'object') return String(value);
  return Object.entries(value)
    .map(([key, item]) => `${key} = ${tomlInline(item)}`)
    .join('\n');
}

function tomlInline(value) {
  if (Array.isArray(value)) {
    return `[${value.map(tomlInline).join(', ')}]`;
  }
  if (value && typeof value === 'object') {
    return `{ ${Object.entries(value)
      .map(([key, item]) => `${key} = ${tomlInline(item)}`)
      .join(', ')} }`;
  }
  if (typeof value === 'string') return JSON.stringify(value);
  return String(value);
}

// ---------------- 输入源表格 ----------------

function renderSourcesBlock(disabled) {
  const block = document.getElementById('sources-block');
  if (!block) return;
  const rows = state.sources
    .map((source, index) => {
      const languageOptions = state.languages.map(
        (language) =>
          `<option value="${escapeHtml(language.code)}"${
            language.code === source.language ? ' selected' : ''
          }>${escapeHtml(language.label)}</option>`,
      );
      if (
        source.language &&
        !state.languages.some((language) => language.code === source.language)
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
      <span class="hint">修改后点右下角「保存」写入配置；设备 / 启用变更会重启采集。</span>
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
    state.sources.push({
      id,
      label: `输入源 ${n}`,
      device: state.devices[0]?.spec || 'default',
      enabled: true,
      translate: true,
      language: '',
    });
    markSourcesDirty();
    renderSourcesBlock(disabled);
  });
  validateSources();
}

function markSourcesDirty() {
  state.dirty.set('audio.sources', deepClone(state.sources));
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
      if (devices.has(device)) errors.push(`${row}: 设备「${device}」被多个启用中的输入源占用`);
      devices.add(device);
    }
  });
  return errors;
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

function bindRawGroup() {
  const editor = document.getElementById('raw-editor');
  if (editor) {
    editor.addEventListener('input', () => {
      state.rawText = editor.value;
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
    saveRaw.addEventListener('click', async () => {
      try {
        const result = await invoke('save_config_raw', { text: state.rawText });
        setStatus(result.message || '已保存', 'ok');
        await refreshSchemaOnly();
      } catch (error) {
        showError(`保存失败: ${error}`);
        setStatus('保存失败', 'err');
      }
    });
  }
}

// ---------------- 字段事件 / 脏数据 ----------------

function bindFieldEvents() {
  els.content.querySelectorAll('[data-path]').forEach((element) => {
    const handler = () => {
      const path = element.dataset.path;
      let value;
      if (element.type === 'checkbox') {
        value = element.checked;
      } else if (element.type === 'number') {
        value = element.value === '' ? 0 : Number(element.value);
      } else {
        value = element.value;
      }

      if (element.dataset.arrayIndex !== undefined) {
        const inputs = els.content.querySelectorAll(
          `[data-path="${CSS.escape(path)}"][data-array-index]`,
        );
        value = Array.from(inputs).map((input) => Number(input.value));
      }

      state.dirty.set(path, value);
      updateDirtyIndicator();

      if (element.type === 'text' && path && isColorPath(path)) {
        updateColorPreview(path, element.value);
      }

      schedulePreview(path);
    };
    element.addEventListener(
      element.type === 'checkbox' || element.tagName === 'SELECT' ? 'change' : 'input',
      handler,
    );
  });

  // 颜色选择器：同步到同路径的文本框
  els.content.querySelectorAll('[data-color-picker]').forEach((picker) => {
    picker.addEventListener('input', () => {
      const path = picker.dataset.colorPicker;
      const text = els.content.querySelector(`[data-path="${CSS.escape(path)}"]`);
      if (text) {
        text.value = picker.value;
      }
      state.dirty.set(path, picker.value);
      updateColorPreview(path, picker.value);
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
    preview.style.background = value || 'transparent';
  }
}

function updateDirtyIndicator() {
  const count = state.dirty.size;
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

function schedulePreview(changedPath) {
  const hot =
    changedPath &&
    (changedPath.startsWith('subtitle.') ||
      changedPath.startsWith('output.') ||
      changedPath === 'translate.enabled');
  if (!hot) return;
  clearTimeout(state.previewTimer);
  state.previewTimer = setTimeout(async () => {
    const changes = [...state.dirty]
      .filter(
        ([path]) =>
          path.startsWith('subtitle.') ||
          path.startsWith('output.') ||
          path === 'translate.enabled',
      )
      .map(([path, value]) => ({ path, value }));
    if (changes.length === 0) return;
    try {
      await invoke('preview_settings', { changes });
    } catch (error) {
      // 预览失败不阻塞编辑（例如标准版拒绝 output.*）
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
  const errors = [];
  for (const change of changes) {
    const field = findField(change.path);
    if (!field || !['int', 'float', 'number'].includes(field.kind)) continue;
    const value = change.value;
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
  const changes = [...state.dirty].map(([path, value]) => ({ path, value }));
  const errors = [...collectSourceErrors(state.sources), ...validateNumbers(changes)];
  if (errors.length > 0) {
    showError(`校验失败：${errors.join('；')}`);
    setStatus('校验失败', 'err');
    return;
  }
  if (changes.length === 0) {
    setStatus('没有需要保存的修改');
    return;
  }

  els.save.disabled = true;
  try {
    let result;
    try {
      result = await invoke('save_settings', { changes });
    } catch (error) {
      console.error(error);
      showError(`保存失败: ${error}`);
      setStatus('保存失败', 'err');
      return;
    }
    state.dirty.clear();
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
    els.save.disabled = false;
  }
}

async function cancel() {
  state.dirty.clear();
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
}

async function reload() {
  try {
    const result = await invoke('reload_config');
    setStatus(result.message || '已重新加载', 'ok');
    await loadAll();
  } catch (error) {
    showError(`重新加载失败: ${error}`);
  }
}

// ---------------- 绑定 ----------------

function bindTopbar() {
  els.profileSelect.addEventListener('change', () => switchProfile(els.profileSelect.value));
  els.profileNew.addEventListener('click', createProfile);
  els.profileDup.addEventListener('click', duplicateProfile);
  els.profileRename.addEventListener('click', renameProfile);
  els.profileDelete.addEventListener('click', deleteProfile);
  els.profileOpen.addEventListener('click', () =>
    invoke('open_profiles_dir').catch((error) => showError(`打开目录失败: ${error}`)),
  );

  els.search.addEventListener('input', () => renderContent());
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
  if (state.dirty.size > 0) {
    const keep = window.confirm(
      `有 ${state.dirty.size} 项未保存的修改。确定 = 保留并继续编辑；取消 = 丢弃并重新加载。`,
    );
    if (keep) return;
  }
  state.dirty.clear();
  await loadAll();
}

window.addEventListener('DOMContentLoaded', async () => {
  if (typeof invoke !== 'function') {
    document.body.innerHTML =
      '<div class="banner error">未检测到 Tauri API，请确认 build.withGlobalTauri = true。</div>';
    return;
  }
  bindTopbar();
  await loadAll();
  // 设置窗口关闭只是 hide()，前端状态会一直活着；
  // 不重新拉取的话，用户会拿启动时的快照去覆盖浮窗里的改动。
  if (window.__TAURI__?.event?.listen) {
    await window.__TAURI__.event.listen('settings://shown', () => {
      reloadOnShow().catch((error) => {
        console.error(error);
        showError(`重新加载失败: ${error}`);
      });
    });
  }
});
