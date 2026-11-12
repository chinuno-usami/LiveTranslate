# 翻译历史 JSONL 分段存储实施计划（v6）

> 目标版本：v0.2.x（无需改动用户可见功能，只改存储层）
> 状态：计划（未实施）；v6 已按代码复核修正 v5 的缓存完整度、`next_ordinal` 承载字段、`open` 时序与两处口径错误（见 §13 修订记录）
> 来源：`5e5fb66` / `f52ef81` 代码评审发现——单文件 append-only + 保留窗口导致「满额后每条新记录都全量重写整个 JSONL」（评审 High #1），并牵连 #3 #4 #5
> 前提：**翻译历史功能从未发布**，因此不做旧数据迁移、不兼容旧游标/旧文件名，可以整体替换存储格式

---

## 1. 为什么必须改

现有实现（`src/history/store.rs`）把「保留最近 `DEFAULT_MAX_RECORDS = 5000` 条」实现在**单个 append-only 文件**上，于是裁剪只能靠「截断缓存 + 重写整个文件」：

```rust
// store.rs:118-128
let over_limit = match state.cached.as_mut() {
    Some(cached) => { insert_cached(cached, record.clone()); cached.len() > self.max_records }
    None => false,
};
if over_limit { self.trim_locked(&mut state)?; }   // trim_locked → rewrite_all：重写全部 5000 行 + rename
```

而 `trim_locked` 把缓存截断到**恰好** `max_records`，所以缓存一旦满额，**下一次 append 就必然再次超限**——即满额之后每条字幕都在持有存储 `Mutex` 的临界区内重写整个文件（≈1 MB）。实测（临时测试，按 append 取样文件 inode）：

```
inodes after each append at cap: [15320038, 15320039, 15320040, 15320041, 15320042]
```

根因不是「trim 写得慢」，而是**单文件保留窗口 ⇒ 裁剪必须重写整个窗口**。分段后：

| 操作 | 现状 | 分段后 |
| --- | --- | --- |
| 追加一条 | O(1) 写入，但满额后 O(n) 重写 | O(1) 写入（仅旋转时新建文件） |
| 裁剪最旧 | O(n) 全量重写 + rename | O(1) `unlink` 最旧段 |
| 首屏读 50 条 | 锁内拷 50 条（但可能与重写互相顶住） | 通常只解析最新段；不足一页时跨段续读 |
| 内存缓存 | 全部 5000 条常驻 | 活动段 + LRU ≤ 3 段（≤ 1500 条） |
| 顺带解决 | — | 评审 #3（重写失败缓存/磁盘不一致）、#4（锁内重写）、#5（多实例靠缓存重写导致「清空复活」） |

> 评审 #2（读路径阻塞）已在 `f52ef81` 修复，本文不涉及。

## 2. 目标与非目标

**目标**

- G1 写路径与保留窗口无关：`append` 恒为 O(1)，裁剪不产生任何重写。
- G2 读路径按需：首屏*通常*只解析最新段；若最新段不足一页，必须跨段续读（见 §6）。
- G3 排序/分页语义比现在**更严格**：不再依赖 `created_at` 单调（当前缓存明确允许乱序时间戳，见 `store.rs` 测试 `incremental_cache_keeps_descending_order`），改由 `seq` 唯一决定顺序。
- G4 隐私清空语义不变：`clear` 仍需经写线程串行（`epoch` 丢弃在途 `Append`），删除全部数据文件。
- G5 **不做旧数据迁移**：历史功能未上线，启动时清理遗留单文件即可；验收标准是「新装/升级后目录里只有 `seg-*.jsonl`」。

**非目标**

- 不做导出（SRT/VTT 等）、不做全文检索、不改实时字幕链路。
- 不改 `subtitle.max_lines` 语义，历史仍与浮窗裁剪解耦。
- 不兼容旧版 `translations.jsonl` 内容、旧 `HistoryCursor { created_at, id }`、旧 `get({id})` 调用（前后端同版本发布）。
- 不做跨进程写锁/单实例约束（多实例会互相删段，且可能同时写同一活动段而留下坏行；不会复活已删数据，见 §8.3）。

## 3. 文件布局与命名

目录：`ProjectDirs::data_local_dir()/history/`（原 `store.rs::default_history_path` 的父目录，改为 `default_history_dir`）。

```
history/
  seg-000000.jsonl          # 已封存段（满 segment_records 条，内容永不再变）
  seg-000001.jsonl          # 活动段（正在追加）
  translations.jsonl        # 遗留单文件：启动时删除（不再读取）
  translations.jsonl.tmp    # 遗留重写残留：启动时删除
```

| 约定 | 值 | 说明 |
| --- | --- | --- |
| `SEGMENT_RECORDS` | `min(500, max_records)` | 单段条数上限；同时决定读锁粒度与旋转频率 |
| 段号 | 零填充 6 位，从 0 起单调递增；进程内由 `SegState::next_ordinal` 维护，`clear` 不重置（§5/§7） | **按数字解析**，不要依赖字典序；6 位上限 999999 段 ≈ 5 亿条，超过时 `rotate` 报错并停写（`warn`，不影响实时链路） |
| 保留段数 `MAX_SEGMENTS` | `max(1, ceil(max_records / SEGMENT_RECORDS))` | 默认 `max_records = 5000` → 10 段 |
| 实际保留条数 | 硬上界 `MAX_SEGMENTS × SEGMENT_RECORDS`，最少 `(MAX_SEGMENTS - 1) × SEGMENT_RECORDS + 1` | 默认 4501~5000 条。**只有 `max_records` 是 `SEGMENT_RECORDS` 整数倍时才等价于「≤ `max_records`」**：默认 5000/500 = 10 段，恰好相等；`max_records = 5001` 时 `MAX_SEGMENTS = 11`，实际上界 5500（比 `max_records` 多 499 条），不要把口径写成「≤ max_records」。`max_records = 3` 时最少可到 1 条（不是始终满 3）。Windows 上 `drop_oldest` 失败时允许短暂超过（见 §5）。文档口径：「不超过 5000 条（按 500 条粒度裁剪）」 |

**注意 `SEGMENT_RECORDS` 必须由 `max_records` 派生**：若写死 500 而 `max_records = 3`（单测/自定义路径），`MAX_SEGMENTS` 会退化成 1，实际保留 500 条，直接违背 `max_records` 语义（现有 `trim_keeps_newest_within_max`、`history_list_page_limit` 都依赖这个语义）。

`with_dir(dir, max_records)` 必须沿用现有 `with_path` 的 `max_records.max(1)` 钳制（`store.rs:88`）：否则 `max_records = 0` 时 `segment_records = 0` → `MAX_SEGMENTS = ceil(0/0)` 直接除零 panic，且 `next_index >= segment_records`（`0 >= 0`）恒真、每条记录都触发旋转。

旋转用 `OpenOptions::new().create_new(true)`；同名已存在（多实例抢先创建）时递增段号重试最多 3 次，仍冲突则 `warn` 并丢弃本条——**绝不覆盖**已有段。不在旋转时 rename 活动文件（多实例下更难协调，且会让读者看到「文件消失」）。

## 4. 记录与游标：引入 `seq`

`TranslationRecord` 增加一个字段（不加 `#[serde(default)]`：无兼容需求，缺 `seq` 的行按坏行跳过即可）：

```rust
pub seq: u64,   // 全局单调；写入顺序的权威排序键
```

**编码方式**：`const SEQ_INDEX_BITS: u32 = 20;`，`seq = ((ordinal as u64) << SEQ_INDEX_BITS) | (index_in_segment as u64)`；`20` 位可容纳 1 048 576 > `SEGMENT_RECORDS`，够用。

好处：

- 游标 → 段位置是 O(1)：`ordinal = seq >> SEQ_INDEX_BITS`，`index = seq & ((1 << SEQ_INDEX_BITS) - 1)`，不需要扫目录、不需要 sidecar 计数文件。
- `SEQ_INDEX_BITS` 必须在 `store.rs` 里定义成常量，读写两侧（`append` 合成 / `list_page`、`get` 拆解）共用，不要在伪码里各写一个字面量 `20`。
- 重启后无需恢复计数器：读最新段解析出的 `max(index) + 1` 即为下一个 `index`（见 §5）。
- `seq` 上界 `ordinal << 20`，只要 `ordinal < 2^32` 就在 JS 安全整数（2^53）内，前端可以直接当 `number` 传。

**排序**：`seq` 倒序 = 时间倒序（追加序），与 `created_at` 无关。`created_at` 降级为纯展示字段，系统时钟跳变不再影响分页正确性。

**结构体变更**：`TranslationRecord::new` / `from_subtitle_line` 置 `seq: 0`（占位），真实 `seq` 由存储层在写线程内分配（见 §5 的 `append` 签名），因此 `mod.rs` 的 `serde_roundtrip_preserves_fields` 等测试需要同步更新。

**前端契约变更**（同版本发布，不做兼容）：

| 位置 | 现状 | 改后 |
| --- | --- | --- |
| `HistoryCursor` | `{ created_at: i64, id: String }` | `{ seq: u64 }` |
| `history.js::cursorOf` | 取 `{created_at, id}` | 取 `{ seq: last.seq }` |
| `history.js::fetchPage` | `before: {created_at,id} \| null` | `before: { seq } \| null` |
| 详情命令 | `get_translation_record({ id })` | `get_translation_record({ seq })`（列表项已带 `seq`；不再保留 id 兜底） |
| 卡片 `data-*` | `data-id` | **保留** `data-id`（选中态/去重），**追加** `data-seq`；点击详情用 `Number(card.dataset.seq)` |
| `selectedId` / `selectRecord` | 按 `id` | **仍按 `id`**；不要改成按 `seq` 做高亮 |
| `history.js` JSDoc | 记录类型无 `seq` | 补 `seq:number` |

`id` 仍保留：列表去重（`mergeRecords` 按 id）、选中态、详情面板展示。`openDetail` 入参改为 `seq`，但先用列表缓存按 `seq` 查到记录后仍用 `record.id` 调 `selectRecord`。

**注意 `seq = 0` 是首条记录的合法值**（`ordinal = 0`、`index = 0`）：现有 `openDetail(id)` 开头的 `if (!id) return;`（`history.js:331`）必须改成 `if (!Number.isFinite(seq)) return;`（或 `seq == null` 判断），否则**第一条记录的详情永远打不开**。同理 `selectRecord(record.id)` 保持不变（UUID 字符串非空，无此问题）。

`openDetail` 内部还有两个隐式调用点必须同步改：`const cached = records.find((r) => r.id === id)` → 按 `seq` 查（`r.seq === seq`），以及 `invoke('get_translation_record', { id })`（`history.js:360`）→ `{ seq }`。卡片点击处也要从 `card.getAttribute('data-id')` 改成 `Number(card.dataset.seq)`。

## 5. 写路径

`SegState` 放在 `HistoryStore` 内（`Mutex<SegState>`），由 `HistoryWriter` 写线程与 list/get/clear 共享；**只有写线程**持有活动段文件句柄，读者只按路径打开已封存/活动段并立即关闭：

```rust
struct SegState {
    dir: PathBuf,
    max_records: usize,
    segment_records: usize,          // min(500, max_records)
    max_segments: usize,             // max(1, ceil(max_records / segment_records))
    segments: Vec<u32>,              // 升序；可有空洞；最后一个是活动段
    next_ordinal: u32,               // 下一个可用新段号：open 时 = max(扫描段号)+1；clear 不重置
    active: Option<ActiveSegment>,   // { file: File, ordinal: u32, next_index: u32 }
    lru: SegmentCache,               // ordinal -> (records, complete)（升序，最多 3 段）
}

// SegmentCache：ordinal -> (Vec<TranslationRecord>, complete: bool)。
// `complete = true` 表示 Vec 覆盖该段磁盘上全部可解析行；`complete = false` 只是部分视图。
//
// push(ord, record)（只由写线程调用）:
//   1. 命中且 complete    → 追加到 Vec 末尾；
//   2. 未命中 / complete=false → 先整段解析（≤ segment_records 行）再用解析结果 + 本条追加，
//      然后置 complete = true；
//   3. 绝不「未命中就插入单元素 Vec」——那会造出只有 1 条的假段，读者命中后整段记录消失（见下方要点）。
//
// segment(ord)（读写共用）: complete=false 的条目等同于未命中，必须重新整段解析。

// 接收 owned 记录，分配 seq 后返回含 seq 的副本（供 LRU / 测试断言）
fn append(&mut self, record: TranslationRecord) -> AppResult<TranslationRecord> {
    if self.active.as_ref().map_or(true, |a| a.next_index as usize >= self.segment_records) {
        self.rotate()?;              // 新建 seg-{next_ordinal}，见下
    }
    let active = self.active.as_mut().expect("rotate 已保证存在");
    let stored = TranslationRecord {
        seq: ((active.ordinal as u64) << SEQ_INDEX_BITS) | active.next_index as u64,
        ..record
    };
    write_line(&mut active.file, &stored)?;   // 单行一次 write_all
    active.next_index += 1;                   // 只有写成功才推进；失败则本条丢弃、seq 不消耗
    // 追加到段缓存；miss / 不完整条目由 push 内部先整段解析，不要塞单条记录
    self.lru.push(active.ordinal, stored.clone());
    if self.segments.len() > self.max_segments {
        // 删段失败不得让本条 append 失败：只 warn，下次 append 再试
        let _ = self.drop_oldest();
    }
    Ok(stored)
}
```

- 保持 `f52ef81` 的「入队只搬运字幕行；`id` / `created_at` 等在写线程用 `from_subtitle_line` 构造」。**`seq` 不在构造时填真实值**（占位 0），由 `HistoryStore::append` 在持锁写入时分配——这依赖「只有写线程调用 append」的前提。`writer.rs` 生产代码需从 `store.append(&record)` 改为 `store.append(record)`（owned）。
- `rotate()` 顺序：`file.flush()` → drop 句柄 → `create_new` 新段（冲突时递增重试）→ 目录 fsync（Unix 可选，Windows 忽略）。任一步失败只 `warn`（返回 `Err` 由 worker 吞掉），本条记录丢弃、`next_index` 不推进；下一次 append 会重试旋转，不影响实时链路。
- 半写入（进程崩溃留下半行）按坏行跳过；因为 `next_index` 只在 `write_all` 成功后推进，正常路径**不会出现两条可解析记录共用同一个 `seq`**。唯一例外是「本条已写出完整行、但 `write_all` 返回错误，随后 `set_len` 回滚也失败」：残行可解析而 `next_index` 未推进 → 下一条复用同一个 `seq`。此时按坏行容忍（`get` 的 `binary_search` 只会命中其中一条），不要把它写成无条件保证。
- **缓存完整度是硬不变式**：写路径的 `push` 与读路径的 `segment` 必须对「完整/部分」达成一致。典型踩坑路径：首屏解析活动段（complete）→ 用户连续翻页读到 seg-2/1/0 → LRU 淘汰活动段 → 下一次 `append` 未命中 → 若此时插入单元素 Vec，之后任何读者都会看到「该段只有最新 1 条」，其余记录在 UI 上消失（磁盘仍在）。所以 miss 分支只能「先整段解析」，不允许插入部分条目；同理 `drop_oldest` 删段时必须连条目一起删（见下文 `drop_oldest` 要点）。
- **写失败后要回滚文件长度**：`write_all` 失败时可能已经写出半行，紧随其后的下一次 append 会与残行首尾相连，把「残行 + 下一条」合成一条坏行 → 下一条静默丢失（正常路径 `seq` 不重复，但数据丢了）。实现时在写前记 `active.file.metadata()?.len()`，失败后 `set_len(len)` 截回（best-effort，截回失败只 `warn`）；这样「写失败只丢本条」才是真的。
- `drop_oldest()` 在 Windows 上可能因句柄未关而失败（`ERROR_SHARING_VIOLATION`）：重试 1~2 次，仍失败则保留该段并在**下次 append 时**再试（保留检查在每次 `append` 都会执行，不是只在旋转时；只会短暂多占 ≤ `SEGMENT_RECORDS` 条）。**失败只记 warn，不向上返回 `Err`**，避免已经落盘的本条被当成写入失败。
- `drop_oldest()` 成功时**必须同时从 `segments` 和 `lru` 移除该 ordinal**：只 unlink 文件而留着缓存，`segment(ord)` 会命中幽灵数据（已删段的记录仍能读出来），且 `oldest_ordinal()` 会继续指向不存在的段。
- **超限段文件**（手工/损坏导致单段可解析行 > `segment_records`）：启动恢复时仍按文件内 `max(index)+1` 设 `next_index`；若已 ≥ `segment_records`，首次 `append` 旋转。保留窗口仍按「段数」裁剪，不在段内截断——接受该段短暂偏大。
- **启动/打开时的恢复规则**（写在 `open` 里，不在 `append`）：
  1. `create_dir_all(dir)`；失败 → `open` 返回错误 → 既有降级「禁用持久化」。
  2. 删除遗留 `translations.jsonl`、`translations.jsonl.tmp`（`NotFound` 忽略；失败只 `warn`）。**这会丢弃开发机上的旧数据**，发版说明里写一句。
  3. 扫描 `seg-<digits>.jsonl` 解析段号得 `segments`（允许空洞；不匹配的其它文件忽略），并置 `next_ordinal = max(段号) + 1`（目录为空时为 0）。分页用 `ord -= 1` 遍历时依赖「空/缺失段跳过 + oldest 下界」（见 §6）。**`clear` 之后不清零 `next_ordinal`**，这是 §6 旧游标守卫能生效的前提。
  4. 段数 > `max_segments` → 从最旧开始删到 `max_segments`（O(1)/段）。**这把裁剪时机从「首次 append」提前到「open」**：现有 `first_append_after_restart_trims_existing_overflow` 的语义随之改变，需改名并改断言（§10 测试 17）。
  5. 打开最新段（`OpenOptions::new().append(true)`）为 `active`，`next_index = max(解析到的 index) + 1`（无可解析行为 0，空尾段直接复用而不新建）；若 `next_index >= segment_records`，首次 `append` 时旋转。
- **`rotate` 用 `next_ordinal` 取新段号**（`seg-{next_ordinal:06}`，成功创建后 `next_ordinal += 1`），不要用 `segments.last() + 1`：`clear` 后 `segments` 为空，依赖它必然回到 `seg-0`，与 §6/§7 的段号单调前提冲突。
- **`open` 必须 eager 且可失败**：`open(dir, max_records) -> AppResult<SegState>` 在构造存储时同步完成 §5 步骤 1~5，由 `OptionalHistoryWriter::try_spawn_default_with_store`（`writer.rs:207`）在 `HistoryWriter::spawn` **之前**调用，保证清理期间没有并发读者（§7）。测试用的 `with_dir(dir, max_records)` 保持不可失败签名，但内部同样 eager open：错误先存进状态（例如 `init: Result<SegState, AppError>`），由首次 `append` / `list_page` / `get` 返回 `Err`，与现有「`open` 失败 → 禁用持久化」的降级一致。**不要改成懒 open**：那会把删文件/裁剪挪到写路径上，并让「写线程启动前完成」的时序说明失效。
- 现有队列/epoch/`flush_blocking` 机制不变，只是 job 的处理体换成 `SegState::append`。

## 6. 读路径

同一 `Mutex<SegState>`（读者与写线程共用，语义与现在一致）。锁粒度约定：

- **单段解析** ≤ `segment_records` 行；
- **单次 `list_page` 临界区**可能跨多段（冷 LRU / 首页不足一页 / `has_more` 探测），上界按段数（默认 ≤ 11），**不要再声称「至多解析 500 行」**。

公开 API 与 §4 一致，入参用 `HistoryCursor`（内部再取 `seq`）：

```rust
pub fn list_page(
    &self,
    before: Option<&HistoryCursor>,
    limit: usize,
) -> AppResult<HistoryPage> {
    let limit = limit.max(1);
    let mut state = self.lock()?;
    let Some(newest) = state.newest_ordinal() else {
        return Ok(HistoryPage::default());     // 目录里一个段都没有
    };
    let oldest = state.oldest_ordinal().expect("有 newest 必有 oldest");
    // 游标落在已裁剪掉的旧段 / 越界高位：返回空页，不把新数据伪装成「更早」
    // （前提：clear 后段号继续单调、不归零，见 §7；段号若归零则同段号旧游标挡不住）
    let mut ord = match before {
        Some(cursor) => {
            let ord = (cursor.seq >> SEQ_INDEX_BITS) as u32;
            if ord > newest || ord < oldest {
                return Ok(HistoryPage::default());
            }
            ord
        }
        None => newest,
    };
    let mut upper = before.map(|c| c.seq);     // 严格早于；None = 无上界
    let mut out = Vec::with_capacity(limit);

    loop {
        let records = state.segment(ord);      // 缺失/不可读段 → 空切片（§7 降级）
        // 段内按 seq 二分定位：不能用数组下标（坏行会让位置错位 → 翻页重复/漏条）
        let end = match upper {
            Some(seq) if (seq >> SEQ_INDEX_BITS) as u32 == ord => {
                records.partition_point(|r| r.seq < seq)   // 段内升序
            }
            _ => records.len(),
        };
        let mut i = end;
        while i > 0 {
            if out.len() == limit {
                // 本段还有未读记录 → 一定还有更早的
                return Ok(HistoryPage { items: out, has_more: true });
            }
            i -= 1;
            out.push(records[i].clone());
        }
        if ord == oldest { break; }
        ord -= 1;
        upper = None;
        if out.len() == limit {
            // 正好读满：peek 更旧段，避免「后面全是空/缺失段」时 has_more 假阳性。
            // 区间必须含 ord 自身：此时 ord 已是「下一个待读段」，
            // 写成半开区间 oldest..ord 会漏掉它 → has_more 假阴性 → 更旧记录永远翻不到。
            let has_more = (oldest..=ord)
                .rev()
                .any(|o| !state.segment(o).is_empty());
            return Ok(HistoryPage { items: out, has_more });
        }
    }
    Ok(HistoryPage { items: out, has_more: false })
}
```

要点：

- **游标是「严格早于」**：`before = Some(cursor)` 时不能把该 `seq` 自己的记录再读一次（现有 `list_page` 用 `partition_point(... >= cursor)` 就是这个语义，别写成 `0..=index`）。
- **上下界都要挡**：`ord > newest`（清空/重建后的高位游标）与 `ord < oldest`（retention 已 unlink 的旧游标）一律空页。缺下界时 `ord -= 1` 会在 `u32` 下溢到 `u32::MAX`。
- **段内定位按 `seq` 二分**，不按数组下标：段内坏行/半行会让「第 i 条」与 `index` 错位。
- **单页可跨段**：最新段刚旋转完可能只有 1 条，`limit = 50` 时必须续读更旧段（否则首屏只有 1 条）。「只打开最新段」是常态优化，不是语义保证。
- **空段/缺失段必须跳过**，不能 `count - 1` 下溢或 `records[0]` 越界（旋转后崩溃会留下 0 条的活动段；多实例/扫描空洞会留下缺口）。
- `has_more` 由存储层判定：本段还有未读 → `true`；正好读满则 **peek 是否存在非空更旧段** → 决定真假；读尽 → `false`。不要用「本页是否满」或「是否还有更小 ordinal」单独猜。
- **peek 区间必须包含 `ord` 自身**（`oldest..=ord`）。`ord -= 1` 之后 `ord` 已经指向「下一个待读段」，半开区间 `oldest..ord` 会跳过它：例如 seg0=5 条、seg1=20 条、seg2=10 条、`limit = 30` 时正好读完 seg1 满页，peek 区间为空 → `has_more = false`，seg0 的 5 条永远翻不到。假阴性比原来的假阳性更糟（数据不可见），`has_more` 的 peek 一定要有对应单测（§10 测试 16）。
- **peek 的解析成本要算进去**：`state.segment(o).is_empty()` 最坏要解析 `max_segments - 1` 个段（默认 9 × 500 行），远超 LRU 的 3 段容量（会把刚用过的段挤掉），别把它算进「首屏只解析最新段」的话术。想省解析可改用 `metadata().len() > 0` 判非空，但**必须维持不变式：`items` 为空的页 `has_more` 必须为 `false`**——前端 `loadMore` 只在 `items.length > 0` 时推进游标（`tauri-ui/history.js`），「空页 + `has_more = true`」会让用户点了没反应；因此若走 stat 判断，解析为空的那一页必须回落 `has_more = false`。
- **`clear` 后段号必须继续单调（不归零）**：这是 `ord > newest || ord < oldest` 守卫能挡住旧游标的前提。若 `clear` 把段号归零，清空前落在 `seg-0` 的游标在清空后仍会放行（新数据被当成「更早」返回），`get(seq)` 也会命中复用 `seq` 的另一条记录。归零唯一的好处是「省心」，代价是一个正确性洞，所以**默认不归零**（§7）。剩余限制：重启且目录为空时计数器会回到 0（`next_ordinal` 没持久化），但此时前端游标也随 webview 重载失效，不构成实际问题；要彻底根治（跨进程 / 持久化段号）需给游标加清空代数（generation）或 sidecar 记录 `next ordinal`。
- 段解析容错沿用 `load_all` 的策略：空行跳过、坏行 `warn` 后跳过（半写入的活动段末行即属此类）。

`get`（只按 `seq`，O(1 段 + log n)）：

```rust
pub fn get(&self, seq: u64) -> AppResult<Option<TranslationRecord>> {
    let mut state = self.lock()?;
    let Some(newest) = state.newest_ordinal() else { return Ok(None) };
    let oldest = state.oldest_ordinal().expect("有 newest 必有 oldest");
    let ord = (seq >> SEQ_INDEX_BITS) as u32;
    if ord > newest || ord < oldest { return Ok(None) }  // 清空 / 段已裁掉
    let records = state.segment(ord);                // 缺失段 → 空切片；LRU 命中或解析 ≤ segment_records 行
    Ok(records.binary_search_by_key(&seq, |r| r.seq).ok().map(|i| records[i].clone()))
}
```

- 段内记录按 `seq` 升序排列，`binary_search_by_key` 直接可用。
- 不再保留「按 id 扫全部分段」的兜底路径（`id` 只用于展示、去重、选中态）。
- 实现注意借用：`segment()` 会按需填 `lru`（需要 `&mut state`），返回 `&[TranslationRecord]` 会挡住后续可变操作；建议返回 `Rc<Vec<TranslationRecord>>`（或让 `segment()` 内部先完成 LRU 填充再返回不可变引用），避免为了借用把整段再 clone 一次。

## 7. 启动清理、清空与降级

**启动清理**（无迁移，见 §5 步骤 1~5）：删遗留单文件 → 扫描段 → 裁剪超限段 → 打开活动段并恢复 `next_index`。全部在写线程启动前完成（`OptionalHistoryWriter::try_spawn_default_with_store` 内），保证没有并发读者。

**清空**（`clear_translation_history`）

- 删除目录下所有 `seg-*.jsonl` + 遗留 `translations.jsonl(.tmp)`；清空 LRU 与 `segments`，丢弃活动句柄；**`SegState::next_ordinal`（§5 的字段）保留、不归零**——段号单调是 §6 游标守卫能挡住「清空前的旧游标 / 旧 `seq`」的前提（归零会让新数据继续占用 `seg-0`，旧游标放行 → 新记录被当成「更早」）。清空后第一条新记录从 `seg-(N+1)` 起，§10 测试 13 的断言按此写。
- **部分失败语义**：`NotFound` 忽略；sharing violation 重试 1 次后继续删其余文件并 `warn`；若最后仍有残留段则返回错误（前端提示「清空失败，请重试」），语义是「尽力删除 + 如实报告」，不要用 `?` 在第一个失败处停下。
- `epoch` 机制保留：抬升后 worker 丢弃在途 `Append`，否则清空后会被在途记录重建出一个新段。
- 顺带修评审 #6：**在命令侧给 `clear_translation_history` 加超时**（`tokio::time::timeout` 包住 `spawn_blocking` 的 `.await`），**不要改 `writer.rs::clear_blocking` 的阻塞语义**（`std::sync::mpsc::SyncSender` 没有 `send_timeout`，那是 crossbeam）。超时只让 UI 脱离 `loading`，worker 里的 `Clear` 仍会继续执行并删除文件——要在代码注释里写明这一点，避免被当成「取消失败」。

**降级路径**

| 场景 | 行为 |
| --- | --- |
| 目录不可创建 / 打开失败 | `open` 返回错误 → 既有 `warn` + 禁用持久化 |
| 写线程启动失败 | 既有 `Disconnected` 路径不变；`clear` 回退为 `store.clear()` |
| 单个段文件缺失（多实例删了 / 用户手删） | `segment()` 返回空切片，`warn` 一次；分页跳过该段继续 |
| 单个段不可读（权限/占用） | 同上，当空段处理，其它段照常 |
| 最新段为空（rotate 后崩溃） | 复用为活动段（`next_index = 0`），不再新建 |
| 段内坏行/半行 | 逐行跳过；段仍参与分页（可能少几条） |
| `segments` 里的段文件被外部删除（多实例 / 用户手删） | 条目仍留在 `segments`，`oldest` 可能指向已缺失的段；按空段跳过，`list_page` 可能在读到该段处提前收尾（不会 panic） |
| 目录被外部删空 | 重新扫描 → `segments` 为空 → `list` 返回空页 |

## 8. 并发与平台细节

1. **写者唯一**：只有写线程持有活动段句柄；读者只 `open` + 一次性读完 + 立即关闭，避免 Windows 上「删不掉被打开的文件」。
2. **锁粒度**：所有段操作在同一 `Mutex<SegState>` 内串行（与现状一致）。单次临界区从「5000 行重写」降到「单段 ≤ `segment_records` 行解析 / 1 行追加」；`list_page` 可能跨多段，但仍远小于整窗重写，评审 #4 的订正自然消失。
3. **多实例**：两个实例各持自己的 `segments` 视图。A 清空后 B 不会把整份缓存重写回去，所以评审 #5 的「清空复活」不复存在。已知且接受的代价：
   - 互相删掉对方的旧段（隐私上更安全）；
   - Unix 上 B 的活动段句柄可能指向已被 A `unlink` 的 inode，B 后续写入静默进入不可见文件（**静默丢写**，不是复活；`next_index` 照常推进，直到旋转后重新可见）；
   - Windows 上 A 的 `clear` 会因 B 持有句柄而部分失败 → 由 §7 的「部分失败语义」提示重试；
   - **两个进程可同时 `append` 同一活动 `seg-N.jsonl`**：无跨进程锁时可能行级交错/半行损坏，读侧按坏行跳过。这是明确接受的代价，不是待修 bug（若将来要杜绝，另开跨进程文件锁/单实例约束）。
4. **目录扫描成本**：段数 ≤ `max_segments + 1`（默认 11），启动扫一次；旋转/删段后增量维护 `segments`。
5. **`create_new` + 数字段号**：不用时间戳命名（多实例/时钟回拨会造成同秒冲突和排序歧义）。段号 6 位上限见 §3，超过即报错停写。
6. **不做跨进程锁**：上表列出的多实例行为是明确接受的取舍，不是待修 bug。
7. **进程内写者唯一**：进程内只有 `HistoryWriter` 写线程调用 `append`；list/get/clear 共享同一把 `Mutex`，但不并发写活动句柄。

## 9. 任务拆分（建议按此顺序落地）

| # | 任务 | 主要文件 / 符号 | 说明 |
| --- | --- | --- | --- |
| P1 | `seq` 字段 + 分段读写骨架 + 构造/访问器 | `src/history/mod.rs`、`store.rs`（内部状态名统一 `SegState`，公共类型仍叫 `HistoryStore`） | **先加 `TranslationRecord.seq` / `HistoryCursor { seq }`**，再落 `with_dir` / `dir()` / `default_history_dir()`；`append(record) -> AppResult<TranslationRecord>`、`rotate`（用 `next_ordinal`）/ `drop_oldest` / `segment` / `list_page(&HistoryCursor)` / `get`；段大小由 `max_records` 派生；`with_dir` 内部 eager open（错误延迟到首次访问），`SegmentCache` 条目带 `complete` 标记 |
| P2 | 写线程适配 | `src/history/writer.rs`（生产路径 + 测试） | `store.append(&record)` → `store.append(record)`；测试改 `with_dir`；模块注释去掉「单文件 / created_at 游标」表述 |
| P3 | 启动清理 + 清空 | `store.rs::open` / `clear` | 删遗留单文件、扫段（允许空洞）、**在 `open` 里**裁剪超限段（替换 `first_append_after_restart_trims_existing_overflow` 的语义）、恢复 `next_index` 与 `next_ordinal = max(段号)+1`；`clear` 的部分失败语义 + **`next_ordinal` 不归零** |
| P4 | 命令层适配 | `src/main.rs`（`list_translation_history` / `get_translation_record` / `clear_translation_history` / `history_list_page_limit`） | 页大小钳制逻辑不变；`get_translation_record` 入参 `seq: u64`；日志改 `store.dir()`；测试构造改 `with_dir` |
| P5 | 前端游标 | `tauri-ui/history.js`（`cursorOf` / `fetchPage` / `timelineItemHtml` / `openDetail` / `selectRecord`） | `before: {seq}`、追加 `data-seq`、`openDetail(Number(seq))`；`openDetail` 内部还要改 `invoke('get_translation_record', { seq })` 与 `records.find(r => r.seq === seq)`；**`selectedId` / `selectRecord` / `mergeRecords` 仍按 id** |
| P6 | 测试 | `store.rs` / `writer.rs` / `main.rs` / `mod.rs` 测试模块 | 见 §10；含 15+ 处 `with_path` → `with_dir` 的机械改造。**注意 `max_records` 的取值陷阱**：`max_records ≤ 500` 时 `max_segments = 1`（旋转即删旧段），需要观察「两段并存」的用例必须用 `501` / `1000` / `1500`（§10 测试 1/3/2/16 分别用到） |
| P7 | 文档 | `README.md`「翻译历史文件位置」、`docs/V0.2_FEATURE_PLAN.md` §1.5、`src/history/store.rs` 与 `writer.rs` 顶部模块注释 | 路径改 `history/seg-*.jsonl`；上限改「不超过 5000 条（按 500 条粒度裁剪）」；模块注释里的「按 created_at 倒序游标分页」「删 jsonl/tmp」一并改 |
| P8 | clear 超时（评审 #6） | **`src/main.rs` 的 `clear_translation_history`**（`spawn_blocking` 外包 `tokio::time::timeout`） | **不要**改 `writer.rs::clear_blocking` 的阻塞语义；`writer` 最多加注释说明超时只解脱 UI。可独立先合 |

预计改动量：`store.rs` 约 +500/−260（含 15+ 测试调用点）、`mod.rs` +10、`writer.rs` 生产路径小改 + 测试 ±40、`main.rs` ±60、`history.js` ±30。

## 10. 测试计划

**单测（新增/改造，必须覆盖边界）**

1. `rotate_creates_next_segment_at_capacity`：**`with_dir(dir, 501)`**（`segment_records = 500`、`max_segments = 2`），写满 500 条后落到 `seg-000001`，旧段不再被追加。**不要用 `max_records = 500`**：该值下 `max_segments = 1`，一旋转就把 `seg-000000` 删掉，没有可断言的旧段。
2. `append_at_capacity_does_not_rewrite_any_file`（回归 #1）：用 `with_dir(dir, 1500)` 写到 1501 条（`seg-0` 已被 unlink，`seg-1`/`seg-2` 各 500 条已封存，`seg-3` 活动 1 条），再连续 append **N ≤ 499** 条（再多就会旋转并 drop 掉 `seg-1`，基准段消失）。断言 `seg-1`/`seg-2` 内容哈希不变；`#[cfg(unix)]` 下额外断言 inode 不变。Windows 用内容哈希 + `len`（不要单靠 1s 精度的 mtime）。若坚持用 `with_dir(dir, 501)` 作基准（`segment_records = 500`、`max_segments = 2`），基准写满 501 条后再 append **N ≤ 499**（第 1000 条仍是 seg1 的最后一条；第 **1001** 条才旋转到 seg2 并 unlink `seg-0`）。
3. `retention_drops_oldest_segment_by_unlink`：覆盖 §3 派生公式——`with_dir(dir, 3)` 时 `segment_records = 3`、`max_segments = 1`。连写 5 条后应只剩最新段里的 **2** 条（旋转到 `seg-1` 后 unlink `seg-0`）。要断言「多段并存时最旧段被删」必须让 `max_segments ≥ 2`，即 `max_records > 500`（例如 `with_dir(dir, 1000)` = 500×2，写完 1001 条后 `seg-0` 被删）。**`with_dir(dir, 6)` 不行**：`max_records ≤ 500` 时 `segment_records = max_records`、`max_segments` 恒为 1，不存在「2 段 × 3」。**不要**期望「5 条后仍剩 3 条」。
4. `list_page_spans_segments`：最新段只有 3 条时，`limit = 50` 的首页要续读到上一段；`has_more` 正确。
5. `cursor_is_exclusive_regression`：首页最后一条不得出现在第二页（翻页不重复）。
6. `cursor_pagination_is_stable_across_rotation`：翻页期间触发旋转 + 新写入，不重复不丢条（改造现有 `cursor_pagination_is_stable_across_new_appends`）。
7. `cursor_beyond_newest_returns_empty`：清空/重建后残留的高位游标返回空页，不 panic、不把新数据显示成「更早」。
8. `cursor_behind_oldest_returns_empty`：retention unlink 最旧段后，仍持有已删段上的低位游标 → 空页，且不 `u32` 下溢/空转。
9. `malformed_lines_do_not_break_cursor`：段内注入坏行后，跨页结果仍不重不漏（第 6 节「按 seq 二分」的回归）。
10. `empty_or_missing_segment_is_skipped`：手工造 0 条活动段 / 删除中间段文件，`list_page` 不 panic、不报错、段内其它记录仍可读；读满一页后若更旧段全空，`has_more = false`。
11. `restart_assigns_unique_seq`：重启后（含末行半写入）新记录的 `seq` 不与已有记录重复，`next_index` 正确恢复。
12. `get_by_seq`：命中/未命中/清空后/游标段已裁掉均符合预期；`TranslationRecord::seq` 随记录返回。
13. `clear_removes_all_segments_and_legacy`：目录清空、`list` 空页、`translations.jsonl.tmp` 一并消失；**清空后新写入的段号严格大于清空前的最大段号**（段号不归零，见 §7）；清空前拿到的游标 / `seq` 在清空后必须返回空页 / `None`（旧游标守卫的回归）。
14. `legacy_files_are_ignored_and_removed`：放入旧 `translations.jsonl`（含坏行）+ `.tmp` → `open` 后不读取、被删除、`segments` 为空。
15. `out_of_order_created_at_does_not_break_paging`：乱序 `created_at` 下分页仍严格按 `seq` 倒序、无重复无遗漏（替代原来对 `(created_at,id)` 的排序假设）。
16. `has_more_true_when_next_older_segment_nonempty`（回归 §6 的 peek 区间）：构造「正好读满一页 + 下一个更旧段非空」，断言 `has_more = true` 且下一页能读到该段记录。最省事的做法是复用测试 1 的 `with_dir(dir, 501)`（`seg-0` 500 条 + `seg-1` 1 条），`limit = 1`：首页读完 `seg-1` 恰好满页，peek 必须看到 `seg-0` 非空。store 层不受命令层 `HISTORY_LIST_MAX_LIMIT = 200` 的钳制，也可以造自然可达的变体：`with_dir(dir, 1500)`（`segment_records = 500`、`max_segments = 3`）写 1001 条 → seg0=500 / seg1=500 / seg2=1，`limit = 501` 时首页正好读完 seg2+seg1 满页，peek 必须看到 seg0 非空。**不要**指望 `seg0=5 / seg1=20 / seg2=10` 这种形状能靠 append 造出来：`segment_records` 恒为 `min(500, max_records)`，不满 500 的段只可能是最新段，那种形状只能手写段文件。与测试 10（更旧段全空时 `has_more = false`）成对，覆盖 peek 的假阳性与假阴性两侧。
17. `open_trims_overflow_segments`（替代 `first_append_after_restart_trims_existing_overflow`）：让目录里的段数超过 `max_segments`（先用大 `max_records` 写入再用小 `max_records` 打开，或手写多余段文件），断言 **`open` 当时**就把最旧段裁到 `max_segments`（不拖到首次 `append`），且 `list_page` 拿到的条数在 `MAX_SEGMENTS × SEGMENT_RECORDS` 上界内。

**手工验证**

- 升级：放入一份旧 `translations.jsonl` → 启动后历史为空（旧数据按计划丢弃）、旧文件消失、`history/` 下出现 `seg-000000.jsonl`。
- 满额：用 `with_dir`/临时配置把 `max_records` 调到 3 跑采集（不要靠临时改 `SEGMENT_RECORDS` 常量，容易忘记改回），确认段号按序推进（`max_records ≤ 500` 时同一时刻只保留 1 段，能看到的是段号不断推进、旧段被删）、UI 翻页正常、`清空` 后目录清空。要肉眼观察「多段并存」需把 `max_records` 配到 `> 500`。
- 退出：采集过程中退出，队列里最后几条仍落盘（现有 `flush_blocking` 语义不变）。
- 三平台：`cargo fmt --check` / `clippy` / `test`；Windows 上验证「删段 + 段文件被读者短暂打开」不会长期失败。

## 11. 验收标准

- [ ] 满额后 `append` 不触发任何整文件重写（测试 2 覆盖）；单条写入耗时不随历史条数增长。
- [ ] 首屏*通常*只解析最新段；不足一页时允许跨段续读（§6）；内存常驻记录数 ≤ `segment_records × 3`。
- [ ] 分页严格按 `seq` 倒序，游标严格排他；翻页期间写入/旋转/裁剪不产生重复或遗漏；坏行、空段、缺失段、已裁掉的低位游标不导致 panic 或错页。
- [ ] `has_more` 判定正确：读满一页且下一个更旧段非空 → `true`（测试 16），更旧段全空 → `false`（测试 10），不出现「提前停止翻页导致旧记录不可见」。
- [ ] 保留条数硬上界为 `MAX_SEGMENTS × SEGMENT_RECORDS`（默认配置恰好 = 5000；`max_records` 非 500 整数倍时可超出 `max_records`，见 §3），且与 `history_list_page_limit` 一致；`drop_oldest` 失败时允许短暂超过，并在下次 append 重试。
- [ ] `clear` 不重置段号（§7）：清空后新数据从 `seg-(N+1)` 起，§6 的 `ord < oldest` 守卫能挡住清空前的旧游标 / 旧 `seq`（测试 13）；前端 `clear` 后照常重置游标，但那只是双保险、不是唯一防线。
- [ ] 启动时遗留单文件被清理且不被读取；`clear` 尽力删除全部数据文件并阻断在途写入。
- [ ] `cargo fmt --check` / `cargo clippy` / `cargo test` 全绿；README 与 `V0.2_FEATURE_PLAN` 的路径/上限描述与实际一致。

## 12. 备选方案（以及为什么不选）

| 方案 | 写法 | 裁决 |
| --- | --- | --- |
| A. 水位差 trim（`len > max + slack` 才截断到 `max`） | 改动 ~10 行；满额后每 `slack` 条一次全量重写，摊薄到 ~4 KB/条 | 若想先止血可单独合入；但保留「偶发 O(n) 重写 + 锁内长临界区」，且不解决 #3/#5。本次已确定重构，不再需要 |
| B. 分段（本方案） | 写路径彻底 O(1)，读路径按需 | 推荐。改动面集中在 `store.rs`，对外命令面只动 `seq` 相关 |
| C. 单文件 + 后台 compaction | append O(1)；compaction 期间仍 O(n)，需要与读者协调 | 复杂度接近 B，收益更差 |

## 13. 修订记录

### v1 → v2

按代码复核修正，对应 v1 的问题：

| # | v2 变更 | 原因 |
| --- | --- | --- |
| 1 | §6 游标改为严格排他（不再 `0..=index`） | v1 伪码会把游标记录再读一次 → 每页首条重复 |
| 2 | §6 段内改用 `seq` 二分定位，不用数组下标 | 坏行让「数组位置 ≠ index」→ 翻页漏条/重复 |
| 3 | §5/§6 明确 `next_index` 语义、空段跳过、游标上界 clamp | v1 的 `count_of - 1` 在空段下会下溢/越界 panic |
| 4 | §3 `SEGMENT_RECORDS = min(500, max_records)`，`MAX_SEGMENTS` 随之派生 | 否则 `max_records = 3` 时实际保留 500 条，破坏既有语义与测试 |
| 5 | §3/§11 保留上限统一为 **≤ `max_records`**（**v4 已细化**：只有整除时才成立，一般上界是 `MAX_SEGMENTS × SEGMENT_RECORDS`，见 v3→v4 #5） | v1 同时写了「≤5000」与「≤5499」，与 `MAX_SEGMENTS = 10` 矛盾 |
| 6 | 删除迁移章节（§7 重写为「启动清理」） | 历史功能未上线，不需要迁移；顺带消除 v1 的 `seq` 缺字段导致迁移读 0 条、以及「升级→降级→再升级」删数据的问题 |
| 7 | §4/§6 取消 `get` 的 id 兜底、`HistoryCursor` 不再带 id | 无兼容需求；避免最坏 10 段 × 500 行的详情扫描 |
| 8 | §7 补 `clear` 部分失败语义 | v1 的 `?` 会在第一个失败处停下，UI 显示失败而已删一半 |
| 9 | §7 `clear` 超时改为 `tokio::time::timeout` | `std::sync::mpsc::SyncSender` 没有 `send_timeout`（crossbeam 才有） |
| 10 | §9 补 `with_dir`/`dir()` 与 15+ 测试调用点 | `HistoryStore` 从文件变目录，`with_path`/`path()` 全量受影响 |
| 11 | §5 补启动恢复规则（段号空洞、空尾段复用、段号冲突递增重试、6 位上限） | v1 只写了「扫目录」，未定义这些分支 |
| 12 | §8 补 Unix 多实例静默丢写 | v1 只说「不会复活」，隐去了丢写代价 |
| 13 | §10 测试项调整（inode 仅 Unix、坏行/空段/游标回归） | 提高回归针对性 |

### v2 → v3

对照当前 `store.rs` / `writer.rs` / `main.rs` / `history.js` 再审后的补洞：

| # | v3 变更 | 原因 |
| --- | --- | --- |
| 1 | §6 增加 `ord < oldest` → 空页；`get` 同步 | v2 只 clamp 上界；retention 删段后低位游标会 `ord -= 1` 下溢 |
| 2 | §6 `list_page` 入参改回 `Option<&HistoryCursor>` | 与 §4 / 命令层 / 前端契约一致，避免伪码写成 `Option<u64>` |
| 3 | §10 测试 3 改为「5 条后剩 2 条」；可选用 `max_records=6` 演示 unlink（**v4 已撤销**：该值仍是 1 段，见 v3→v4 #4） | v2「5 条后仍剩 3 条」与 `max_segments=1` 派生公式矛盾 |
| 4 | §10 新增 `cursor_behind_oldest_returns_empty` | 覆盖低位游标 / 下溢回归 |
| 5 | §6 `has_more` 在读满后 peek 非空更旧段 | 避免后面全是空/缺失段时假阳性多刷一页 |
| 6 | §5 明确 `SegmentCache.push`（追加而非整段替换）；`drop_oldest` 失败不让 append 失败 | v2 伪码 `lru.insert` / `drop_oldest()?` 易被照抄错 |
| 7 | §5 写明 `SegState` 在 `HistoryStore`；`HistoryWriter` 命名；`append` owned 需改 writer 生产代码 | 消除「状态只在 worker 内」与共享锁的矛盾；补接口适配遗漏 |
| 8 | §8 接受「多实例同写活动段」的坏行风险；§5 补超限段/空洞段策略 | v2 未写明，实现时会犹豫是否要加文件锁 |
| 9 | §4/P5 明确 `selectedId`/`selectRecord`/`data-id` 仍按 id，仅详情走 `seq` | 避免改成半吊子「高亮也按 seq」 |
| 10 | §9 合并 P1+P2 顺序（先 `seq` 再骨架）、P8 只落 `main.rs` | 无 `seq` 无法做分段；超时包点被 P8 文件列写偏 |
| 11 | §2/§6/§8/§11 统一「首屏通常只读最新段、可跨段」；保留条数允许短暂超过 | 与跨段分页、`drop_oldest` 失败语义对齐 |
| 12 | §10 测试 2 改内容哈希为主，mtime 降级 | 1s 精度 FS 上 mtime 断言易碎 |

### v3 → v4

再按 `store.rs` / `writer.rs` / `main.rs` / `history.js` 复核 v3 引入的问题与遗留漏洞：

| # | v4 变更 | 原因 |
| --- | --- | --- |
| 1 | §6 `has_more` peek 区间 `oldest..ord` → `oldest..=ord` | `ord -= 1` 后 `ord` 已是「下一个待读段」，半开区间漏掉它 → 读满一页时 `has_more` 假阴性，更旧记录永远翻不到（seg0=5/seg1=20/seg2=10、`limit=30` 即复现） |
| 2 | §10 新增测试 16 `has_more_true_when_next_older_segment_nonempty`；§11 补验收项 | 原测试清单只有「更旧段全空 → false」，缺「非空 → true」这一侧，peek 的假阴性测不出来 |
| 3 | §10 测试 1/2 的 `with_dir(dir, 500)` → `501` | `max_records = 500` 时 `max_segments = 1`，旋转即 unlink，旧段不存在，「旧段不再被追加 / 封存段哈希不变」无从断言 |
| 4 | §10 测试 3 去掉 `with_dir(dir, 6)`（2 段 × 3），改 `1000` | `max_records = 6` 时 `segment_records = 6`、`max_segments = ceil(6/6) = 1`，不存在「2 段 × 3」；`max_records ≤ 500` 时 `max_segments` 恒为 1 |
| 5 | §3/§11 保留条数上界改为 `MAX_SEGMENTS × SEGMENT_RECORDS` | 「常态 ≤ `max_records`」只在 `max_records` 为 500 整数倍时成立（默认 5000 恰好整除）；`max_records = 5001` 实际上界 5500，口径不能写成「≤ max_records」 |
| 6 | §6/§7/§11 明确「段号复用的世代」为已知限制（**v5 已改为段号单调，见 v4→v5 #4**） | `clear` 后段号归零，同段号的旧游标会放行（新数据被当成「更早」），`get(seq)` 会命中复用 `seq` 的新记录；守卫只能挡越界游标，原表述「已清空/重建的世代 → 空页」过强 |
| 7 | §4 显式定义 `SEQ_INDEX_BITS`；`openDetail` 的守卫不能用真值判断 | 伪码直接用了未定义常量；`seq = 0` 是首条记录的合法值，沿用 `if (!id) return;` 会让首条详情打不开 |
| 8 | §5 `drop_oldest` 补「成功时须同时清 `segments` 与 `lru`」；重试时机由「下次旋转」改「下次 append」 | 只 unlink 文件不清缓存 → `segment()` 命中幽灵数据、`oldest` 指向缺失段；保留检查在每次 `append` 都执行 |
| 9 | §7 超时落点表述改为「命令侧包 `clear_translation_history`」，与 P8 对齐 | v3 §7 首句「给 `clear_blocking` 加超时」与后文、P8「不要改 `clear_blocking`」自相矛盾 |
| 10 | §7 降级表补「`segments` 残条目指向已删除文件」；§9 P6 补 `max_records` 取值陷阱 | 外部删段后 `oldest` 可能不可读；测试里 500 是个危险阈值，容易写出自我矛盾的用例 |

### v4 → v5

按 v4 复核时发现的可实施性问题修正（**v4 的 §6/§10 结论全部保留**，下面只改实现/用例细节）：

| # | v5 变更 | 原因 |
| --- | --- | --- |
| 1 | §10 测试 16 的第二个变体改成「`with_dir(dir, 1500)` 写 1001 条 + `limit = 501`」 | v4 写的 seg0=5 / seg1=20 / seg2=10「需 `max_records = 1500`」不可由 append 构造：`segment_records` 恒为 `min(500, max_records)`，不满 500 的段只可能是最新段；那种形状必须手写段文件 |
| 2 | §10 测试 2 改 `with_dir(dir, 1500)` 写 1501 条 + `N ≤ 499`（备选 `501` 时 `N ≤ 498`，**v6 已修正为 `499`，见 v5→v6 #4**） | v4 用 `501` 时基准段只剩 1 条 append 余量，超过就旋转并 unlink 基准段，「封存段哈希不变」的断言会莫名失败 |
| 3 | §6 增「peek 的解析成本」要点；明确「空页 ⇒ `has_more = false`」不变式 | v4 的 peek 用 `segment().is_empty()` 解析更旧段（最坏 9 × 500 行，超过 LRU 3 段）；若改用 `metadata().len()` 省解析，必须防止空页 + `has_more = true` 让前端 `loadMore` 原地打转 |
| 4 | §7/§6/§10 测试 13/§11：`clear` **不再把段号归零**，改为继续单调（归零降级为备选） | v4 把「段号复用」当已知限制接受，但归零只省一句实现，却让 `ord < oldest` 守卫失效（旧游标放行、`get(seq)` 命中错记录）；单调实现更简单也更正确，且前端 `clear` 重置游标已足够兜底 |
| 5 | §3 补 `with_dir` 必须沿用 `max_records.max(1)` 钳制 | `max_records = 0` 时 `MAX_SEGMENTS = ceil(0/0)` 除零 panic、每条 append 都旋转；现有 `with_path` 已有该钳制（`store.rs:88`），改目录版容易漏 |
| 6 | §10 新增测试 17 `open_trims_overflow_segments`；§5 步骤 4、§9 P3 标注语义变更 | v4 把裁剪时机从「首次 append」改成「open」，现有 `first_append_after_restart_trims_existing_overflow` 的语义变了，但测试清单里没有对应项 |
| 7 | §5 增「写失败后 `set_len` 回滚文件长度」 | `write_all` 部分写失败后，下一次 append 会与残行连成一条坏行 → 下一条静默丢失；v4 只说「只丢本条」并不成立 |
| 8 | §9 P5 / §4 补 `openDetail` 内部 `invoke('get_translation_record', { seq })` 与按 `seq` 查缓存 | v4 只写了 `openDetail(Number(seq))`，实际调用点 `history.js:360` 仍传 `{ id }`；缓存查找也要改成 `records.find(r => r.seq === seq)` |

### v5 → v6

按 v5 对照 `store.rs` / `writer.rs` / `history.js` / `main.rs` 复核（**v5 的 §6/§10 结论全部保留**）：

| # | v6 变更 | 原因 |
| --- | --- | --- |
| 1 | §5 `SegmentCache` 增 `complete` 标记，明确「miss / 不完整条目必须先整段解析」，删除「未命中插入单元素 Vec」 | v5 只在注释里说「不要把整段替换成单条」，miss 分支仍是单元素 Vec：LRU 淘汰活动段后再 `append` 会造出只有 1 条的假段，读者命中后该段其余记录在 UI 上消失（磁盘仍在），且不会自愈直到条目被再次淘汰 |
| 2 | §5 `SegState` 增 `next_ordinal: u32`；`open` 赋 `max(段号)+1`、`rotate` 用它递增、`clear` 不重置 | v5 §7 要求「`next ordinal` 不归零」但结构体没有承载字段，`rotate` 按伪码只能从 `active`/`segments` 派生 → `clear` 后必回 `seg-0`，§6 旧游标守卫与测试 13 的断言都失去依据 |
| 3 | §5 增「`open` 必须 eager 且可失败；`with_dir` 内部同样 eager，错误延迟到首次访问」 | v5 只说「清理写在 `open` 里」，未定义 `with_dir` 是否 eager；若实现成懒 `open`，删文件/裁剪会落到写路径上，§7「写线程启动前完成、无并发读者」的前提不成立 |
| 4 | §10 测试 2 的 501 备选改 `N ≤ 499`，旋转点写清是第 **1001** 条 | v5 写「`N ≤ 498`（写到第 1000 条时旋转会 unlink `seg-0`）」，与同文测试 16 的「1500 写 1001 条 → seg2=1 条」口径矛盾（第 1000 条仍是 seg1 的最后一条） |
| 5 | §4/§13 行号 `history.js:371` → `360` | 实际 `invoke('get_translation_record', { id })` 在 360 行 |
| 6 | §2 非目标补「多实例可能同时写同一活动段而留下坏行」 | 与 §8.3 新增的取舍对齐；v5 的「多实例只会互相删段」已不完整 |
| 7 | §5 唯一性论断限定为「正常路径 / 回滚成功时」 | `set_len` 回滚失败时「完整行 + 未推进的 `next_index`」会复用同一个 `seq`，原表述是无条件保证 |
