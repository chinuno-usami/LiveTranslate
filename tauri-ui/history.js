// Tauri 全局 API（build.withGlobalTauri = true）
const tauriApi = window.__TAURI__ || {};
const invoke = tauriApi.invoke || (tauriApi.tauri && tauriApi.tauri.invoke);

const PAGE_SIZE = 50;

const els = {
  refreshBtn: document.getElementById('refresh-btn'),
  loadMoreBtn: document.getElementById('load-more-btn'),
  clearBtn: document.getElementById('clear-btn'),
  errorBanner: document.getElementById('error-banner'),
  statusBanner: document.getElementById('status-banner'),
  emptyState: document.getElementById('empty-state'),
  timeline: document.getElementById('timeline'),
  listStatus: document.getElementById('list-status'),
  detailModal: document.getElementById('detail-modal'),
  detailClose: document.getElementById('detail-close'),
  detailTime: document.getElementById('detail-time'),
  detailSourceChip: document.getElementById('detail-source-chip'),
  detailError: document.getElementById('detail-error'),
  detailSource: document.getElementById('detail-source'),
  detailTranslated: document.getElementById('detail-translated'),
  detailId: document.getElementById('detail-id'),
  confirmModal: document.getElementById('confirm-modal'),
  confirmTitle: document.getElementById('confirm-title'),
  confirmMessage: document.getElementById('confirm-message'),
  confirmOk: document.getElementById('confirm-ok'),
  confirmCancel: document.getElementById('confirm-cancel'),
};

/** @type {Array<{id:string,created_at:number,source_id:string,label:string,source:string,translated:string}>} */
let records = [];
/** 上一页最后一条的游标；null 表示第一页。翻页期间有新记录写入也不会重复/漏条 */
let cursor = null;
let hasMore = false;
let loading = false;
let selectedId = null;
let detailLoading = false;

function showError(message) {
  if (!els.errorBanner) return;
  if (!message) {
    els.errorBanner.classList.add('hidden');
    els.errorBanner.textContent = '';
    return;
  }
  els.errorBanner.textContent = String(message);
  els.errorBanner.classList.remove('hidden');
}

function showStatus(message) {
  if (!els.statusBanner) return;
  if (!message) {
    els.statusBanner.classList.add('hidden');
    els.statusBanner.textContent = '';
    return;
  }
  els.statusBanner.textContent = String(message);
  els.statusBanner.classList.remove('hidden');
}

function setListStatus(message) {
  if (els.listStatus) {
    els.listStatus.textContent = message;
  }
}

function escapeHtml(value) {
  return String(value ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function summarize(text, maxChars) {
  const raw = String(text ?? '').replace(/\s+/g, ' ').trim();
  if (!raw) return '（空）';
  if (raw.length <= maxChars) return raw;
  return `${raw.slice(0, Math.max(0, maxChars - 1))}…`;
}

function formatTime(ms) {
  const n = Number(ms);
  if (!Number.isFinite(n) || n <= 0) return '未知时间';
  const d = new Date(n);
  if (Number.isNaN(d.getTime())) return '未知时间';
  try {
    return new Intl.DateTimeFormat('zh-CN', {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
    }).format(d);
  } catch (_) {
    return d.toLocaleString();
  }
}

/** `<time datetime>` 用 ISO-8601；无效时间返回空字符串。 */
function formatDateTimeAttr(ms) {
  const n = Number(ms);
  if (!Number.isFinite(n) || n <= 0) return '';
  const d = new Date(n);
  if (Number.isNaN(d.getTime())) return '';
  return d.toISOString();
}

function sourceLabel(record) {
  const label = String(record.label || '').trim();
  if (label) return label;
  const id = String(record.source_id || '').trim();
  return id || '未知来源';
}

function updateChrome() {
  const count = records.length;
  if (els.emptyState) {
    els.emptyState.classList.toggle('hidden', count > 0);
  }
  if (els.loadMoreBtn) {
    els.loadMoreBtn.disabled = loading || !hasMore;
  }
  if (els.refreshBtn) {
    els.refreshBtn.disabled = loading;
  }
  if (els.clearBtn) {
    els.clearBtn.disabled = loading || count === 0;
  }
  if (loading) {
    setListStatus('加载中…');
  } else if (count === 0) {
    setListStatus('暂无记录');
  } else if (hasMore) {
    setListStatus(`已显示 ${count} 条，可继续加载`);
  } else {
    setListStatus(`共 ${count} 条`);
  }
}

function timelineItemHtml(record) {
  const selected = record.id === selectedId ? ' selected' : '';
  const chip = escapeHtml(sourceLabel(record));
  const time = escapeHtml(formatTime(record.created_at));
  const datetime = escapeHtml(formatDateTimeAttr(record.created_at));
  const source = escapeHtml(summarize(record.source, 96));
  const translated = escapeHtml(summarize(record.translated, 96));
  return `
        <li class="timeline-item">
          <button
            type="button"
            class="timeline-card${selected}"
            data-id="${escapeHtml(record.id)}"
            aria-pressed="${record.id === selectedId ? 'true' : 'false'}"
          >
            <div class="timeline-meta">
              <time class="timeline-time" datetime="${datetime}">${time}</time>
              <span class="source-chip">${chip}</span>
            </div>
            <div class="timeline-texts">
              <div class="timeline-row source">
                <span class="label">原文</span>
                <p class="text">${source}</p>
              </div>
              <div class="timeline-row translated">
                <span class="label">译文</span>
                <p class="text">${translated}</p>
              </div>
            </div>
          </button>
        </li>`;
}

/** 整表重绘（首屏 / 刷新 / 清空） */
function renderTimeline() {
  if (!els.timeline) return;
  els.timeline.innerHTML = records.map(timelineItemHtml).join('');
  updateChrome();
}

/** 追加渲染：只插入新条目，避免翻页时整表重建（O(n²)） */
function appendTimeline(items) {
  if (!els.timeline || items.length === 0) return;
  els.timeline.insertAdjacentHTML(
    'beforeend',
    items.map(timelineItemHtml).join(''),
  );
  updateChrome();
}

function selectRecord(id) {
  selectedId = id || null;
  if (!els.timeline) return;
  els.timeline.querySelectorAll('.timeline-card').forEach((card) => {
    const on = card.getAttribute('data-id') === selectedId;
    card.classList.toggle('selected', on);
    card.setAttribute('aria-pressed', on ? 'true' : 'false');
  });
}

function isDetailOpen() {
  return els.detailModal && !els.detailModal.classList.contains('hidden');
}

// ---------------- 页内确认对话框 ----------------
// 不用 window.confirm：Tauri v1 把它重写成返回 Promise 的异步 IPC（且 dialog 未在
// allowlist 里开放），守卫既不阻塞也拦不住，点「清空」会直接删掉历史。
// 与设置页保持一致，使用自绘模态。

let activeConfirmCancel = null;

function uiConfirm({
  title = '提示',
  message = '',
  okText = '确定',
  cancelText = '取消',
} = {}) {
  const { confirmModal } = els;
  if (!confirmModal || !els.confirmOk || !els.confirmCancel) {
    console.warn('确认对话框未就绪，已取消本次操作');
    return Promise.resolve(false);
  }
  // 同一时刻只保留一个确认框；旧的按取消处理
  if (activeConfirmCancel) {
    activeConfirmCancel();
  }

  return new Promise((resolve) => {
    const previousFocus = document.activeElement;
    els.confirmTitle.textContent = title;
    els.confirmMessage.textContent = message;
    els.confirmOk.textContent = okText;
    els.confirmCancel.textContent = cancelText;
    confirmModal.classList.remove('hidden');

    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      activeConfirmCancel = null;
      confirmModal.classList.add('hidden');
      els.confirmOk.removeEventListener('click', onOk);
      els.confirmCancel.removeEventListener('click', onCancel);
      confirmModal.removeEventListener('click', onBackdrop);
      window.removeEventListener('keydown', onKeyDown, true);
      if (previousFocus && typeof previousFocus.focus === 'function') {
        previousFocus.focus();
      }
      resolve(result);
    };

    const onOk = () => finish(true);
    const onCancel = () => finish(false);
    const onBackdrop = (event) => {
      if (event.target === confirmModal) {
        finish(false);
      }
    };
    const onKeyDown = (event) => {
      if (event.isComposing || event.keyCode === 229) {
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        finish(false);
      } else if (event.key === 'Enter' && document.activeElement !== els.confirmCancel) {
        event.preventDefault();
        event.stopPropagation();
        finish(true);
      }
    };

    activeConfirmCancel = () => finish(false);
    els.confirmOk.addEventListener('click', onOk);
    els.confirmCancel.addEventListener('click', onCancel);
    confirmModal.addEventListener('click', onBackdrop);
    window.addEventListener('keydown', onKeyDown, true);
    // 破坏性操作默认聚焦「取消」，避免误按回车直删
    requestAnimationFrame(() => {
      els.confirmCancel.focus();
    });
  });
}

function showDetailError(message) {
  if (!els.detailError) return;
  if (!message) {
    els.detailError.classList.add('hidden');
    els.detailError.textContent = '';
    return;
  }
  els.detailError.textContent = String(message);
  els.detailError.classList.remove('hidden');
}

function fillDetail(record) {
  if (!record) return;
  if (els.detailTime) {
    const ms = Number(record.created_at);
    els.detailTime.textContent = formatTime(ms);
    els.detailTime.dateTime = formatDateTimeAttr(ms);
  }
  if (els.detailSourceChip) {
    els.detailSourceChip.textContent = sourceLabel(record);
  }
  if (els.detailSource) {
    const text = String(record.source ?? '');
    els.detailSource.textContent = text.trim() ? text : '（空）';
  }
  if (els.detailTranslated) {
    const text = String(record.translated ?? '');
    els.detailTranslated.textContent = text.trim() ? text : '（空）';
  }
  if (els.detailId) {
    els.detailId.textContent = String(record.id || '');
  }
}

function closeDetail() {
  if (!els.detailModal) return;
  els.detailModal.classList.add('hidden');
  showDetailError('');
}

async function openDetail(id) {
  if (!id) return;
  selectRecord(id);
  const cached = records.find((r) => r.id === id) || null;
  if (cached) {
    fillDetail(cached);
  } else {
    fillDetail({
      id,
      created_at: 0,
      source_id: '',
      label: '',
      source: '加载中…',
      translated: '加载中…',
    });
  }
  showDetailError('');
  if (els.detailModal) {
    els.detailModal.classList.remove('hidden');
  }

  if (typeof invoke !== 'function') {
    if (!cached) {
      showDetailError('Tauri invoke 不可用，无法加载详情');
    }
    return;
  }

  detailLoading = true;
  try {
    const record = await invoke('get_translation_record', { id });
    if (!record) {
      showDetailError('记录不存在或已被清理');
      return;
    }
    fillDetail(record);
    showDetailError('');
  } catch (error) {
    console.error('加载翻译详情失败', error);
    if (!cached) {
      showDetailError(`加载详情失败: ${error}`);
    } else {
      showDetailError(`刷新详情失败，已显示本地缓存: ${error}`);
    }
  } finally {
    detailLoading = false;
  }
}

async function fetchPage(before) {
  if (typeof invoke !== 'function') {
    throw new Error('Tauri invoke 不可用');
  }
  const page = await invoke('list_translation_history', {
    before: before ? { created_at: before.created_at, id: before.id } : null,
    limit: PAGE_SIZE,
  });
  if (!page || !Array.isArray(page.items)) {
    throw new Error('历史列表返回格式无效');
  }
  return { items: page.items, hasMore: Boolean(page.has_more) };
}

/** 按 id 去重后追加到本地列表（游标分页已避免重复，这里只是兼保险） */
function mergeRecords(items) {
  const seen = new Set(records.map((record) => record.id));
  const fresh = [];
  for (const record of items) {
    if (!record || !record.id || seen.has(record.id)) continue;
    seen.add(record.id);
    fresh.push(record);
  }
  records = records.concat(fresh);
  return fresh;
}

function cursorOf(items) {
  if (!items.length) return null;
  const last = items[items.length - 1];
  return { created_at: last.created_at, id: last.id };
}

async function loadInitial() {
  if (loading) return;
  loading = true;
  showError('');
  updateChrome();
  try {
    const page = await fetchPage(null);
    records = [];
    mergeRecords(page.items);
    cursor = cursorOf(page.items);
    hasMore = page.hasMore;
    if (selectedId && !records.some((r) => r.id === selectedId)) {
      selectedId = null;
      closeDetail();
    }
    renderTimeline();
    showStatus('');
  } catch (error) {
    console.error('加载翻译历史失败', error);
    records = [];
    cursor = null;
    hasMore = false;
    renderTimeline();
    showError(`加载历史失败: ${error}`);
  } finally {
    loading = false;
    updateChrome();
  }
}

async function loadMore() {
  if (loading || !hasMore) return;
  loading = true;
  showError('');
  updateChrome();
  try {
    const page = await fetchPage(cursor);
    const fresh = mergeRecords(page.items);
    if (page.items.length > 0) {
      cursor = cursorOf(page.items);
    }
    hasMore = page.hasMore;
    appendTimeline(fresh);
  } catch (error) {
    console.error('加载更多历史失败', error);
    showError(`加载更多失败: ${error}`);
  } finally {
    loading = false;
    updateChrome();
  }
}

async function clearHistory() {
  if (loading) return;
  const confirmed = await uiConfirm({
    title: '清空翻译历史',
    message: '确定清空全部本地翻译历史？此操作不可撤销。',
    okText: '清空',
    cancelText: '取消',
  });
  if (!confirmed) {
    return;
  }
  loading = true;
  showError('');
  updateChrome();
  try {
    await invoke('clear_translation_history');
    records = [];
    cursor = null;
    hasMore = false;
    selectedId = null;
    closeDetail();
    renderTimeline();
    showStatus('已清空本地翻译历史');
  } catch (error) {
    console.error('清空历史失败', error);
    showError(`清空失败: ${error}`);
  } finally {
    loading = false;
    updateChrome();
  }
}

function bindUi() {
  if (els.refreshBtn) {
    els.refreshBtn.addEventListener('click', () => {
      loadInitial();
    });
  }
  if (els.loadMoreBtn) {
    els.loadMoreBtn.addEventListener('click', () => {
      loadMore();
    });
  }
  if (els.clearBtn) {
    els.clearBtn.addEventListener('click', () => {
      clearHistory();
    });
  }
  if (els.timeline) {
    els.timeline.addEventListener('click', (event) => {
      const card = event.target.closest('.timeline-card');
      if (!card) return;
      openDetail(card.getAttribute('data-id'));
    });
  }
  if (els.detailClose) {
    els.detailClose.addEventListener('click', () => {
      closeDetail();
    });
  }
  if (els.detailModal) {
    els.detailModal.addEventListener('click', (event) => {
      if (event.target === els.detailModal) {
        closeDetail();
      }
    });
  }
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && isDetailOpen() && !detailLoading) {
      closeDetail();
    }
  });
}

async function bindShownEvent() {
  const listen = tauriApi.event && tauriApi.event.listen;
  if (typeof listen !== 'function') {
    return;
  }
  await listen('history://shown', () => {
    loadInitial();
  });
}

bindUi();
bindShownEvent().catch((error) => {
  console.warn('订阅 history://shown 失败', error);
});
loadInitial();
