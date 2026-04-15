# QMT 行情免维护方案实现说明

## 目标

- 从数据库优先读取行情数据，缺口时自动调用 MiniQmt/`XtQuant.XtData` 下载补齐并回写数据库，实现“免维护”的行情供给链。
- 统一封装数据请求、持久化与补数流程，为 Python 策略脚本与 Rust 引擎共用。

## 系统概览

- `MarketDataService`：对外入口，负责 orchestrate 数据查询、缺口检测、补数与返回。
- `DBDataProvider`：封装数据库读取逻辑，返回已有数据并识别缺口。
- `XtDataProvider`：基于 `XtQuant.XtData` 的下载与获取封装，内建重试、分段下载。
- `PersistenceService`：批量写入行情与元数据，提供幂等的 upsert。
- `DataIntegrityChecker`：校验缺口补齐情况、重复/异常值，输出告警。
- 辅助模块：`InstrumentService`（合约信息）、`CalendarService`（交易日历）、`Config`（QMT 连接与 DB 配置）、`Logging & Monitoring`。

```text
DataRequest
   │
   ▼
MarketDataService ───▶ DBDataProvider ──▶ 数据缺口?
                       │                    │
                       └──── 完整数据 ◀─────┘
                                            缺口列表
                                              │
                                              ▼
                                   XtDataProvider.download_history_data2
                                              │
                                              ▼
                                   PersistenceService (raw_quotes/upsert)
                                              │
                                              ▼
                                   DBDataProvider 重新获取
```

## 数据流细节

1. **请求封装**  
   - `DataRequest` 包含 `symbol(s)`, `period`, `start_time`, `end_time`, `count`, `dividend_type`, `fields` 等。
   - 时间范围遵循 `[start_time, end_time]` 闭区间；`count=-1` 代表全部。
2. **数据库查询**  
   - `DBDataProvider.fetch(request)`  
     - 读取 `raw_quotes`（或 `agg_quotes`）表，返回 `pandas.DataFrame`。  
     - 计算期望时间轴（借助 `CalendarService` 与周期规则）并识别缺口。
     - 输出 `(dataframe, missing_ranges)`，`missing_ranges` 用交易日/时间戳表示。
3. **缺口补齐**  
   - 当 `missing_ranges` 非空：  
     - 拆分为适合 `download_history_data2` 的批次（按交易日段或最大条数）。  
     - 调用 `XtDataProvider.download_history_data2(stock_list, period, start_time, end_time, incrementally)`。  
     - 监听进度回调，日志记录 `{finished, total, stockcode, message}`，异常自动重试（指数退避）。  
     - 对于短缺尾部可使用增量模式（`start_time=''`）。  
4. **数据获取与写库**  
   - 下载后通过 `XtDataProvider.get_market_data(..., fill_data=False)` 获取新增数据（或 `get_local_data`）。  
   - `PersistenceService.upsert_quotes(dataframe)`  
     - 映射字段：`time → ts`, `open`, `high`, `low`, `close`, `volume`, `amount`, `openInterest`, `preClose`, `suspendFlag` 等。  
     - 主键 `(symbol, period, ts)`；可使用 `INSERT ... ON CONFLICT DO UPDATE`（PostgreSQL）或等价语句。  
   - 更新 `data_source_status(symbol, period, last_sync_ts)`。
5. **返回结果**  
   - 再次调用 `DBDataProvider` 获取完整数据，按 `DataRequest` 组合并返回。

## 数据库设计

- 表 `raw_quotes`
  - 列：`symbol`, `market`, `period`, `ts`, `open`, `high`, `low`, `close`, `volume`, `amount`, `open_interest`, `pre_close`, `suspend_flag`, `dividend_type`, `updated_at`。
  - 主键：`(symbol, period, ts, dividend_type)`；视需要加入 `market`。
- 可选表 `agg_quotes`（按需聚合）或 `cache_latest_quotes`（实时订阅落地）。
- 元数据表 `data_source_status(symbol, period, dividend_type, last_sync_ts, last_sync_count, note)`。
- 索引：`(symbol, period, ts)`、`(ts)` 支持范围查询；必要时分区（按 `symbol` 或 `period`）。

## XtData API 封装要点

- **连接管理**
  - 依赖 `xtdata.connect()` / `reconnect(ip, port)` 自动与 MiniQmt 对接。多实例环境下配置优先级。
  - 每次调用前检查连接状态，断线则重连。
- **下载接口**
  - `download_history_data2(stock_list, period, start_time='', end_time='', incrementally=None, callback=on_progress)`。
  - 针对单标的缺口可回落至 `download_history_data`。
  - 长区间拆分：利用 `CalendarService` 获取交易日列表；按批次调用。
  - 下载静态数据：`download_sector_data`, `download_holiday_data`, `download_index_weight` 等在部署时或定时任务执行。
- **数据获取接口**
  - `get_market_data(field_list, stock_list, period, start_time, end_time, count, dividend_type, fill_data)`：缓存数据。  
  - `get_local_data(...)`：读取 MiniQmt `userdata_mini` 文件，适合批处理。  
  - Tick 数据返回 `np.ndarray`；K 线返回 `dict[field -> DataFrame]`（index=stock，columns=time）。需转换为长表落库。
- **实时订阅（可选）**
  - `subscribe_quote` / `run()`：实时数据推送，回调写入内存缓存或队列。  
  - 定期刷入数据库（可批量 upsert）。  
  - `subscribe_whole_quote` 适合大规模使用；需限制订阅量避免超过 50 支单股订阅。
- **辅助接口**
  - `get_instrument_detail`, `get_instrument_type`：校验合约/维护元信息。  
  - `get_trading_calendar`, `get_trading_dates`, `get_holidays`：用于缺口推断、夜间批量补数。  
  - `get_divid_factors`: 支持复权数据写库或查询时即计算。

## 异常处理与监控

- 统一异常封装 `XtDataError`，归类：连接异常、下载失败、数据为空、限流（可加重试节奏）。  
- 监控指标：
  - 下载耗时、补数条数、缺口率、`XtData` 重试次数、数据库写入延迟。  
  - 定时汇报 `data_source_status` 与告警。  
- 日志结构化记录关键参数（标的、周期、时间范围、复权方式、耗时、条数、错误信息）。
- 网络/接口错误重试策略：
  - 指数退避（如 1s, 5s, 15s...），最多 N 次（默认 3）。  
  - 部分成功时标记剩余缺口，进入下一轮补数。  
  - 多次失败时写入 `data_source_status.note` 以便人工排查。

## 测试策略

- **单元测试**
  - `DBDataProvider`: 时间轴生成、缺口识别、数据裁剪。  
  - `XtDataProvider`: 参数构造、异常处理（可使用 Mock）。  
  - `PersistenceService`: upsert 去重、字段映射正确性。
- **集成测试**
  - 场景一：数据库无数据 → 自动下载 → 写库 → 返回完整数据。  
  - 场景二：数据库部分缺失 → 识别缺口 → 仅补缺 → 返回完整数据。  
  - 场景三：下载失败重试，最终记录告警。  
  - 可通过开关 `XtDataProvider` Mock 数据以模拟 QMT 环境。
- **性能测试**
  - 批量标的（`stock_list` 大于 50）分批执行，评估下载速度与数据库写入吞吐。  
  - Tick 数据大数据量入库的内存占用与写入策略。

## 实施步骤

1. **准备阶段**
   - 确认数据库类型、连接配置、表结构（迁移脚本）。  
   - 明确策略端需要的字段和周期，整理字段映射。  
   - 准备 MiniQmt & xtdata 环境（含账号、端口、数据目录）。
2. **基础模块实现**
   - 定义 `DataRequest` & `MarketDataService` 接口。  
   - 实现 `DBDataProvider`、`PersistenceService` 核心逻辑。  
   - 封装 `XtDataProvider`（连接、下载、获取、重试、回调日志）。
3. **补数流程整合**
   - 实装缺口检测、批量下载、写库、二次查询的完整流程。  
   - 接入 `DataIntegrityChecker`，确保缺口被填满。  
   - 增加配置管理（YAML/JSON/ENV），统一控制周期、限速、重试次数。
4. **静态数据与元信息**
   - 定时任务下载节假日、板块、合约信息等静态数据，写入数据库或缓存文件。  
   - 提供接口查询板块、交易日历等。
5. **实时订阅扩展（可选）**
   - 根据需要实现 `subscribe_quote` 的实时数据流，写入缓存/数据库。  
   - 确保订阅线程安全关闭，避免阻塞主线程。
6. **集成 & 测试**
   - 编写集成测试脚本（可放在 `examples/` 中）验证端到端流程。  
   - 运行性能测试、压测数据库写入路径。  
   - 汇总测试报告与监控基线。
7. **上线与运维**
   - 部署后台定时补数任务（夜间）及指标监控。  
   - 建立故障告警通道，记录下载失败或数据异常。  
   - 定期校验数据库与 QMT 数据一致性（抽样比对）。

## 与现有项目的集成建议

- Python 示例：在 `examples/run_mvp_db.py` 中新建 `MarketDataService` 调用路径，演示“先 db 后自动补数”的使用方式。
- Rust 引擎：在 `engine_rust` 中新增 `market_data` 模块，通过 FFI 调用 Python 服务或直接封装 `xtdata`（若提供 C 接口）。  
- 配置文件统一放在 `config/market_data.yaml`（示例），包含 `xtdata` 连接信息、数据库 DSN、缺口批次大小、重试策略等。

## 后续关注

- 兼容多复权模式：数据库可按 `dividend_type` 区分，或实时换算。  
- Tick 大数据入库：需评估压缩存储、分区策略，必要时使用列式存储或对象存储。  
- 调整下载节奏与限流：根据 QMT 限制适当增加间隔，避免 403/限频。  
- 多环境隔离：测试、生产分别配置 MiniQmt 与数据库实例，避免互扰。
