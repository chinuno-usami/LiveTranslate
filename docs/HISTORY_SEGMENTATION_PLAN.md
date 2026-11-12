# 翻译历史 JSONL 分段存储实施计划（v2）

> 目标版本：v0.2.x（无需改动用户可见功能，只改存储层）
> 状态：计划（未实施）；v2 已按代码复核修订（见 §13 修订记录）
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
| 首屏读 50 条 | 锁内拷 50 条（但可能与重写互相顶住） | 只解析最新段（≤ `SEGMENT_RECORDS` 行） |
| 内存缓存 | 全部 5000 条常驻 | 活动段 + LRU ≤ 3 段（≤ 1500 条） |
| 顺带解决 | — | 评审 #3（重写失败缓存/磁盘不一致）、#4（锁内重写）、#5（多实例靠缓存重写导致「清空复活」） |

> 评审 #2（读路径阻塞）已在 `f52ef81` 修复，本文不涉及。

## 2. 目标与非目标

**目标**

- G1 写路径与保留窗口无关：`append` 恒为 O(1)，裁剪不产生任何重写。
- G2 读路径按需：首屏只解析最新段；翻到更早记录时才打开更旧的段。
- G3 排序/分页语义比现在**更严格**：不再依赖 `created_at` 单调（当前缓存明确允许乱序时间戳，见 `store.rs` 测试 `incremental_cache_keeps_descending_order`），改由 `seq` 唯一决定顺序。
- G4 隐私清空语义不变：`clear` 仍需经写线程串行（`epoch` 丢弃在途 `Append`），删除全部数据文件。
- G5 **不做旧数据迁移**：历史功能未上线，启动时清理遗留单文件即可；验收标准是「新装/升级后目录里只有 `seg-*.jsonl`」。

**非目标**

- 不做导出（SRT/VTT 等）、不做全文检索、不改实时字幕链路。
- 不改 `subtitle.max_lines` 语义，历史仍与浮窗裁剪解耦。
- 不兼容旧版 `translations.jsonl` 内容、旧 `HistoryCursor { created_at, id }`、旧 `get({id})` 调用（前后端同版本发布）。
- 不做跨进程写锁/单实例约束（多实例只会互相删段，不会复活已删数据，见 §8）。

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
| 段号 | 零填充 6 位，从 0 起单调递增 | **按数字解析**，不要依赖字典序；6 位上限 999999 段 ≈ 5 亿条，超过时 `rotate` 报错并停写（`warn`，不影响实时链路） |
| 保留段数 `MAX_SEGMENTS` | `max(1, ceil(max_records / SEGMENT_RECORDS))` | 默认 `max_records = 5000` → 10 段 |
| 实际保留条数 | **≤ `max_records`**，最少 `max_records - SEGMENT_RECORDS + 1` | 默认 4501~5000 条。表格/文档口径统一为「不超过 5000 条（按 500 条为粒度裁剪）」 |

**注意 `SEGMENT_RECORDS` 必须由 `max_records` 派生**：若写死 500 而 `max_records = 3`（单测/自定义路径），`MAX_SEGMENTS` 会退化成 1，实际保留 500 条，直接违背 `max_records` 语义（现有 `trim_keeps_newest_within_max`、`history_list_page_limit` 都依赖这个语义）。

旋转用 `OpenOptions::new().create_new(true)`；同名已存在（多实例抢先创建）时递增段号重试最多 3 次，仍冲突则 `warn` 并丢弃本条——**绝不覆盖**已有段。不在旋转时 rename 活动文件（多实例下更难协调，且会让读者看到「文件消失」）。

## 4. 记录与游标：引入 `seq`

`TranslationRecord` 增加一个字段（不加 `#[serde(default)]`：无兼容需求，缺 `seq` 的行按坏行跳过即可）：

```rust
pub seq: u64,   // 全局单调；写入顺序的权威排序键
```

**编码方式**：`seq = ((ordinal as u64) << 20) | (index_in_segment as u64)`，其中 `20` 位可容纳 1 048 576 > `SEGMENT_RECORDS`。

好处：

- 游标 → 段位置是 O(1)：`ordinal = seq >> 20`，`index = seq & 0xFFFFF`，不需要扫目录、不需要 sidecar 计数文件。
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
| 卡片 `data-*` | `data-id` | 追加 `data-seq`，点击详情用 `Number(card.dataset.seq)` |
| `history.js` JSDoc | 记录类型无 `seq` | 补 `seq:number` |

`id` 仍保留：列表去重（`mergeRecords` 按 id）、详情面板展示。

## 5. 写路径

写线程独占活动段句柄（`HistoryWorker` 内部状态），读者只按路径打开已封存段：

```rust
struct SegState {
    dir: PathBuf,
    max_records: usize,
    segment_records: usize,          // min(500, max_records)
    max_segments: usize,             // max(1, ceil(max_records / segment_records))
    segments: Vec<u32>,              // 升序；最后一个是活动段
    active: Option<ActiveSegment>,   // { file: File, ordinal: u32, next_index: u32 }
    lru: SegmentCache,               // ordinal -> Vec<TranslationRecord>（升序，最多 3 段）
}

// 接收 owned 记录，分配 seq 后返回含 seq 的副本（供 LRU / 测试断言）
fn append(&mut self, record: TranslationRecord) -> AppResult<TranslationRecord> {
    if self.active.as_ref().map_or(true, |a| a.next_index as usize >= self.segment_records) {
        self.rotate()?;              // 新建 seg-(ordinal+1)，见下
    }
    let active = self.active.as_mut().expect("rotate 已保证存在");
    let stored = TranslationRecord {
        seq: ((active.ordinal as u64) << SEQ_INDEX_BITS) | active.next_index as u64,
        ..record
    };
    write_line(&mut active.file, &stored)?;   // 单行一次 write_all
    active.next_index += 1;                   // 只有写成功才推进；失败则本条丢弃、seq 不消耗
    self.lru.insert(active.ordinal, stored.clone());
    if self.segments.len() > self.max_segments {
        self.drop_oldest()?;                  // unlink + 从 segments/lru 移除；O(1)
    }
    Ok(stored)
}
```

- 保持 `f52ef81` 的「入队只搬运字幕行、记录（含 id/seq/时间戳）在写线程里构造」——`seq` 的分配依赖单写线程这个前提。
- `rotate()` 顺序：`file.flush()` → drop 句柄 → `create_new` 新段（冲突时递增重试）→ 目录 fsync（Unix 可选，Windows 忽略）。任一步失败只 `warn`（返回 `Err` 由 worker 吞掉），本条记录丢弃、`next_index` 不推进；下一次 append 会重试旋转，不影响实时链路。
- 半写入（进程崩溃留下半行）按坏行跳过；因为 `next_index` 只在 `write_all` 成功后推进，**不会出现两条可解析记录共用同一个 `seq`**。
- `drop_oldest()` 在 Windows 上可能因句柄未关而失败（`ERROR_SHARING_VIOLATION`）：重试 1~2 次，仍失败则保留该段并在下次旋转时再试（只会多占 ≤ `SEGMENT_RECORDS` 条，不影响正确性）。
- **启动/打开时的恢复规则**（写在 `open` 里，不在 `append`）：
  1. `create_dir_all(dir)`；失败 → `open` 返回错误 → 既有降级「禁用持久化」。
  2. 删除遗留 `translations.jsonl`、`translations.jsonl.tmp`（`NotFound` 忽略；失败只 `warn`）。**这会丢弃开发机上的旧数据**，发版说明里写一句。
  3. 扫描 `seg-<digits>.jsonl` 解析段号得 `segments`；不匹配的其它文件忽略。
  4. 段数 > `max_segments` → 从最旧开始删到 `max_segments`（O(1)/段）。
  5. 打开最新段（`OpenOptions::new().append(true)`）为 `active`，`next_index = max(解析到的 index) + 1`（无可解析行为 0，空尾段直接复用而不新建）；若 `next_index >= segment_records`，首次 `append` 时旋转。
- 现有队列/epoch/`flush_blocking` 机制不变，只是 job 的处理体换成 `SegState::append`。

## 6. 读路径

同一 `Mutex<SegState>`（读者与写线程共用，语义与现在一致，但**单次临界区有界**：至多解析 500 行）。

```rust
pub fn list_page(&self, before: Option<u64>, limit: usize) -> AppResult<HistoryPage> {
    let limit = limit.max(1);
    let mut state = self.lock()?;
    let Some(newest) = state.newest_ordinal() else {
        return Ok(HistoryPage::default());     // 目录里一个段都没有
    };
    // 游标落在已被清空/重建的世代：返回空页，不把新数据伪装成「更早」
    let mut ord = match before {
        Some(seq) => {
            let ord = (seq >> SEQ_INDEX_BITS) as u32;
            if ord > newest { return Ok(HistoryPage::default()); }
            ord
        }
        None => newest,
    };
    let mut upper = before;                    // 严格早于；None = 无上界
    let mut out = Vec::with_capacity(limit);
    let mut has_more = false;

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
        if ord == state.oldest_ordinal() { break; }
        ord -= 1;
        upper = None;
        if out.len() == limit {
            has_more = true;                   // 正好读满且还有更旧的段
            break;
        }
    }
    Ok(HistoryPage { items: out, has_more })
}
```

要点：

- **游标是「严格早于」**：`before = Some(seq)` 时不能把该 `seq` 自己的记录再读一次（现有 `list_page` 用 `partition_point(... >= cursor)` 就是这个语义，别写成 `0..=index`）。
- **段内定位按 `seq` 二分**，不按数组下标：段内坏行/半行会让「第 i 条」与 `index` 错位。
- **单页可跨段**：最新段刚旋转完可能只有 1 条，`limit = 50` 时必须续读更旧段（否则首屏只有 1 条）。这就是「只打开最新的 jsonl」只能作为常态优化、不能作为语义保证。
- **空段/缺失段必须跳过**，不能 `count - 1` 下溢或 `records[0]` 越界（旋转后崩溃会留下 0 条的活动段；多实例会删掉段）。
- `has_more` 由存储层判定（保持现状）：本段还有未读 → `true`；正好读满且还有更旧段 → `true`；读尽 → `false`。不要用「本页是否满」猜。
- 段解析容错沿用 `load_all` 的策略：空行跳过、坏行 `warn` 后跳过（半写入的活动段末行即属此类）。

`get`（只按 `seq`，O(1 段 + log n)）：

```rust
pub fn get(&self, seq: u64) -> AppResult<Option<TranslationRecord>> {
    let mut state = self.lock()?;
    let Some(newest) = state.newest_ordinal() else { return Ok(None) };
    let ord = (seq >> SEQ_INDEX_BITS) as u32;
    if ord > newest { return Ok(None) }              // 已被清空 / 段已删
    let records = state.segment(ord);                // 缺失段 → 空切片；LRU 命中或解析 ≤ 500 行
    Ok(records.binary_search_by_key(&seq, |r| r.seq).ok().map(|i| records[i].clone()))
}
```

- 段内记录按 `seq` 升序排列，`binary_search_by_key` 直接可用。
- 不再保留「按 id 扫全部分段」的兜底路径（`id` 只用于展示与去重）。
- 实现注意借用：`segment()` 会按需填 `lru`（需要 `&mut state`），返回 `&[TranslationRecord]` 会挡住后续可变操作；建议返回 `Rc<Vec<TranslationRecord>>`（或让 `segment()` 内部先完成 LRU 填充再返回不可变引用），避免为了借用把整段再 clone 一次。

## 7. 启动清理、清空与降级

**启动清理**（无迁移，见 §5 步骤 1~5）：删遗留单文件 → 扫描段 → 裁剪超限段 → 打开活动段并恢复 `next_index`。全部在写线程启动前完成（`OptionalHistoryWriter::try_spawn_default_with_store` 内），保证没有并发读者。

**清空**（`clear_translation_history`）

- 删除目录下所有 `seg-*.jsonl` + 遗留 `translations.jsonl(.tmp)`；清空 LRU 与 `segments`，丢弃活动句柄，`next ordinal` 归零（目录已空，归零更省心）。
- **部分失败语义**：`NotFound` 忽略；sharing violation 重试 1 次后继续删其余文件并 `warn`；若最后仍有残留段则返回错误（前端提示「清空失败，请重试」），语义是「尽力删除 + 如实报告」，不要用 `?` 在第一个失败处停下。
- `epoch` 机制保留：抬升后 worker 丢弃在途 `Append`，否则清空后会被在途记录重建出一个新段。
- 顺带修评审 #6：给 `clear_blocking` 加超时。**只能用命令侧 `tokio::time::timeout` 包住 `spawn_blocking`**（`std::sync::mpsc::SyncSender` 没有 `send_timeout`，那是 crossbeam）。超时只让 UI 脱离 `loading`，worker 里的 `Clear` 仍会继续执行并删除文件——要在代码注释里写明这一点，避免被当成「取消失败」。

**降级路径**

| 场景 | 行为 |
| --- | --- |
| 目录不可创建 / 打开失败 | `open` 返回错误 → 既有 `warn` + 禁用持久化 |
| 写线程启动失败 | 既有 `Disconnected` 路径不变；`clear` 回退为 `store.clear()` |
| 单个段文件缺失（多实例删了 / 用户手删） | `segment()` 返回空切片，`warn` 一次；分页跳过该段继续 |
| 单个段不可读（权限/占用） | 同上，当空段处理，其它段照常 |
| 最新段为空（rotate 后崩溃） | 复用为活动段（`next_index = 0`），不再新建 |
| 段内坏行/半行 | 逐行跳过；段仍参与分页（可能少几条） |
| 目录被外部删空 | 重新扫描 → `segments` 为空 → `list` 返回空页 |

## 8. 并发与平台细节

1. **写者唯一**：只有写线程持有活动段句柄；读者只 `open` + 一次性读完 + 立即关闭，避免 Windows 上「删不掉被打开的文件」。
2. **锁粒度**：所有段操作在同一 `Mutex<SegState>` 内串行（与现状一致），但单次临界区从「5000 行重写」降到「≤500 行解析」或「1 行追加」，评审 #4 的订正自然消失。
3. **多实例**：两个实例各持自己的 `segments` 视图。A 清空后 B 不会把整份缓存重写回去，所以评审 #5 的「清空复活」不复存在。已知且接受的代价：
   - 互相删掉对方的旧段（隐私上更安全）；
   - Unix 上 B 的活动段句柄可能指向已被 A `unlink` 的 inode，B 后续写入静默进入不可见文件（**静默丢写**，不是复活；`next_index` 照常推进，直到旋转后重新可见）；
   - Windows 上 A 的 `clear` 会因 B 持有句柄而部分失败 → 由 §7 的「部分失败语义」提示重试。
4. **目录扫描成本**：段数 ≤ `max_segments + 1`（默认 11），启动扫一次；旋转/删段后增量维护 `segments`。
5. **`create_new` + 数字段号**：不用时间戳命名（多实例/时钟回拨会造成同秒冲突和排序歧义）。段号 6 位上限见 §3，超过即报错停写。
6. **不做跨进程锁**：上表列出的多实例行为是明确接受的取舍，不是待修 bug。

## 9. 任务拆分（建议按此顺序落地）

| # | 任务 | 主要文件 / 符号 | 说明 |
| --- | --- | --- | --- |
| P1 | 分段读写骨架 + 构造/访问器改造 | `src/history/store.rs`（内部 `SegmentStore`，保留 `HistoryStore` 公共名） | `with_dir(dir, max_records)` / `dir()` / `default_history_dir()`；`append` / `rotate` / `drop_oldest` / `segment` / `list_page` / `get`；`SEGMENT_RECORDS`、`MAX_SEGMENTS` 由 `max_records` 派生 |
| P2 | `seq` 字段与游标 | `src/history/mod.rs`（`TranslationRecord.seq`）、`store.rs::HistoryCursor` | `seq: 0` 占位；`HistoryCursor { seq }`；`created_at` 保留展示 |
| P3 | 启动清理 + 清空 | `store.rs::open` / `clear` | 删遗留单文件、扫段、裁剪超限段、恢复 `next_index`；`clear` 的部分失败语义 |
| P4 | 命令层适配 | `src/main.rs`（`list_translation_history` / `get_translation_record` / `clear_translation_history` / `history_list_page_limit`） | 页大小钳制逻辑不变；`get_translation_record` 入参 `seq: u64`；`store.dir()` 日志；测试构造改 `with_dir` |
| P5 | 前端游标 | `tauri-ui/history.js`（`cursorOf` / `fetchPage` / `timelineItemHtml` / `openDetail` / `selectRecord`） | `before: {seq}`、`data-seq`、`openDetail(Number(seq))`；`mergeRecords` 仍按 id 去重 |
| P6 | 测试 | `store.rs` / `writer.rs` / `main.rs` / `mod.rs` 测试模块 | 见 §10；含 15+ 处 `with_path` → `with_dir` 的机械改造 |
| P7 | 文档 | `README.md`「翻译历史文件位置」、`docs/V0.2_FEATURE_PLAN.md` §1.5、`src/history/store.rs` 与 `writer.rs` 顶部模块注释 | 路径改 `history/seg-*.jsonl`；上限改「不超过 5000 条（按 500 条粒度裁剪）」；模块注释里的「按 created_at 倒序游标分页」「删 jsonl/tmp」一并改 |
| P8 | clear 超时（评审 #6） | `src/history/writer.rs::clear_blocking`、`src/main.rs` | `tokio::time::timeout` 包 `spawn_blocking`；可独立先合 |

预计改动量：`store.rs` 约 +500/−260（含 15+ 测试调用点）、`mod.rs` +10、`main.rs` ±50、`history.js` ±25、`writer.rs` 测试 ±30。

## 10. 测试计划

**单测（新增/改造，必须覆盖边界）**

1. `rotate_creates_next_segment_at_capacity`：`with_dir(dir, 500)`，写满 500 条后落到 `seg-000001`，旧段不再被追加。
2. `append_at_capacity_does_not_rewrite_any_file`（回归 #1）：满额后连续 append N 条，**所有已封存段文件内容与 mtime 不变**；`#[cfg(unix)]` 下额外断言 inode 不变（Windows 用 `len + mtime`）。
3. `retention_drops_oldest_segment_by_unlink`：覆盖 §3 的派生公式——`with_dir(dir, 3)` 时 `segment_records = 3`、`max_segments = 1`，连写 5 条后只剩 3 条（这是旧 `trim_keeps_newest_within_max` 的替代断言）。
4. `list_page_spans_segments`：最新段只有 3 条时，`limit = 50` 的首页要续读到上一段；`has_more` 正确。
5. `cursor_is_exclusive_regression`：首页最后一条不得出现在第二页（翻页不重复）。
6. `cursor_pagination_is_stable_across_rotation`：翻页期间触发旋转 + 新写入，不重复不丢条（改造现有 `cursor_pagination_is_stable_across_new_appends`）。
7. `cursor_beyond_newest_returns_empty`：清空/重建后残留的高位游标返回空页，不 panic、不把新数据显示成「更早」。
8. `malformed_lines_do_not_break_cursor`：段内注入坏行后，跨页结果仍不重不漏（第 6 节「按 seq 二分」的回归）。
9. `empty_or_missing_segment_is_skipped`：手工造 0 条活动段 / 删除中间段文件，`list_page` 不 panic、不报错、段内其它记录仍可读。
10. `restart_assigns_unique_seq`：重启后（含末行半写入）新记录的 `seq` 不与已有记录重复，`next_index` 正确恢复。
11. `get_by_seq`：命中/未命中/清空后均符合预期；`TranslationRecord::seq` 随记录返回。
12. `clear_removes_all_segments_and_legacy`：目录清空、`list` 空页、新写入从 `seg-000000` 重新开始、`translations.jsonl.tmp` 一并消失。
13. `legacy_files_are_ignored_and_removed`：放入旧 `translations.jsonl`（含坏行）+ `.tmp` → `open` 后不读取、被删除、`segments` 为空。
14. `out_of_order_created_at_does_not_break_paging`：乱序 `created_at` 下分页仍严格按 `seq` 倒序、无重复无遗漏（替代原来对 `(created_at,id)` 的排序假设）。

**手工验证**

- 升级：放入一份旧 `translations.jsonl` → 启动后历史为空（旧数据按计划丢弃）、旧文件消失、`history/` 下出现 `seg-000000.jsonl`。
- 满额：用 `with_dir`/临时配置把 `max_records` 调到 3 跑采集（不要靠临时改 `SEGMENT_RECORDS` 常量，容易忘记改回），确认按序出现多个段、最旧段被删、UI 翻页正常、`清空` 后目录清空。
- 退出：采集过程中退出，队列里最后几条仍落盘（现有 `flush_blocking` 语义不变）。
- 三平台：`cargo fmt --check` / `clippy` / `test`；Windows 上验证「删段 + 段文件被读者短暂打开」不会长期失败。

## 11. 验收标准

- [ ] 满额后 `append` 不触发任何整文件重写（测试 2 覆盖）；单条写入耗时不随历史条数增长。
- [ ] 首屏读取只解析最新段；内存常驻记录数 ≤ `segment_records × 3`。
- [ ] 分页严格按 `seq` 倒序，游标严格排他；翻页期间写入/旋转/裁剪不产生重复或遗漏；坏行、空段、缺失段不导致 panic 或错页。
- [ ] 保留条数 ≤ `max_records`（含 `max_records < SEGMENT_RECORDS` 的自定义配置），且 `max_records` 语义与 `history_list_page_limit` 一致。
- [ ] 启动时遗留单文件被清理且不被读取；`clear` 尽力删除全部数据文件并阻断在途写入。
- [ ] `cargo fmt --check` / `cargo clippy` / `cargo test` 全绿；README 与 `V0.2_FEATURE_PLAN` 的路径/上限描述与实际一致。

## 12. 备选方案（以及为什么不选）

| 方案 | 写法 | 裁决 |
| --- | --- | --- |
| A. 水位差 trim（`len > max + slack` 才截断到 `max`） | 改动 ~10 行；满额后每 `slack` 条一次全量重写，摊薄到 ~4 KB/条 | 若想先止血可单独合入；但保留「偶发 O(n) 重写 + 锁内长临界区」，且不解决 #3/#5。本次已确定重构，不再需要 |
| B. 分段（本方案） | 写路径彻底 O(1)，读路径按需 | 推荐。改动面集中在 `store.rs`，对外命令面只动 `seq` 相关 |
| C. 单文件 + 后台 compaction | append O(1)；compaction 期间仍 O(n)，需要与读者协调 | 复杂度接近 B，收益更差 |

## 13. 修订记录（v1 → v2）

按代码复核修正，对应 v1 的问题：

| # | v2 变更 | 原因 |
| --- | --- | --- |
| 1 | §6 游标改为严格排他（不再 `0..=index`） | v1 伪码会把游标记录再读一次 → 每页首条重复 |
| 2 | §6 段内改用 `seq` 二分定位，不用数组下标 | 坏行让「数组位置 ≠ index」→ 翻页漏条/重复 |
| 3 | §5/§6 明确 `next_index` 语义、空段跳过、游标 clamp | v1 的 `count_of - 1` 在空段下会下溢/越界 panic |
| 4 | §3 `SEGMENT_RECORDS = min(500, max_records)`，`MAX_SEGMENTS` 随之派生 | 否则 `max_records = 3` 时实际保留 500 条，破坏既有语义与测试 |
| 5 | §3/§11 保留上限统一为 **≤ `max_records`** | v1 同时写了「≤5000」与「≤5499」，与 `MAX_SEGMENTS = 10` 矛盾 |
| 6 | 删除迁移章节（§7 重写为「启动清理」） | 历史功能未上线，不需要迁移；顺带消除 v1 的 `seq` 缺字段导致迁移读 0 条、以及「升级→降级→再升级」删数据的问题 |
| 7 | §4/§6 取消 `get` 的 id 兜底、`HistoryCursor` 不再带 id | 无兼容需求；避免最坏 10 段 × 500 行的详情扫描 |
| 8 | §7 补 `clear` 部分失败语义 | v1 的 `?` 会在第一个失败处停下，UI 显示失败而已删一半 |
| 9 | §7 `clear` 超时改为 `tokio::time::timeout` | `std::sync::mpsc::SyncSender` 没有 `send_timeout`（crossbeam 才有） |
| 10 | §9 P1/P4/P6 补 `with_dir`/`dir()` 与 15+ 测试调用点 | `HistoryStore` 从文件变目录，`with_path`/`path()` 全量受影响 |
| 11 | §5 补启动恢复规则（段号空洞、空尾段复用、段号冲突递增重试、6 位上限） | v1 只写了「扫目录」，未定义这些分支 |
| 12 | §8 补 Unix 多实例静默丢写 | v1 只说「不会复活」，隐去了丢写代价 |
| 13 | §10 测试项调整（inode 仅 Unix、`with_dir(dir, 3)` 派生、坏行/空段/游标回归） | 提高回归针对性 |
