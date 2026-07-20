from __future__ import annotations
from dataclasses import dataclass, field
from typing import Any, Dict, List, Tuple, Optional, Union, Type
import math
import pandas as pd
import numpy as np
from datetime import datetime
import json
import csv
import os
from enum import Enum

from .analyzers import factor_backtest

# 可选的 numba 加速（大样本时触发）
try:
    from numba import njit
    NUMBA_AVAILABLE = True
except Exception:  # pragma: no cover
    NUMBA_AVAILABLE = False
    def njit(*args, **kwargs):  # type: ignore
        def wrapper(func):
            return func
        return wrapper


@dataclass
class FactorResult:
    """单个因子的评价结果"""
    factor_name: str
    ic: float
    ic_ir: float  # IC信息比率
    ic_win_rate: float  # IC胜率
    monotonicity: float
    quantile_returns: List[float]
    factor_stats: Dict[str, float]
    rank_ic: float  # 秩相关系数
    turnover_rate: float  # 换手率
    decay_analysis: Dict[str, float]  # 因子衰减分析
    stability_score: float  # 稳定性评分


@dataclass
class MultiFactorReport:
    """多因子综合评价报告"""
    factor_results: Dict[str, FactorResult]
    factor_ranking: List[Tuple[str, float]]  # 因子排名
    correlation_matrix: pd.DataFrame  # 因子间相关性矩阵
    summary_stats: Dict[str, Any]
    report_timestamp: str


class AnalysisMethod(Enum):
    TS = "ts"  # 时间序列
    CS = "cs"  # 横截面


@dataclass
class FactorConfig:
    """因子分析配置（可序列化、可扩展）"""
    quantiles: int = 5
    forward_periods: List[int] = field(default_factory=lambda: [1, 5, 10, 20])
    window_size: int = 60
    nan_policy: str = "drop"  # drop | zero
    winsorize: Optional[Tuple[float, float]] = None  # e.g., (0.01, 0.99)
    standardize: bool = False  # z-score within cross section
    horizon_unit: str = "bars"  # bars | days | months | quarters (informational)
    method: AnalysisMethod = AnalysisMethod.TS


# 分析器注册表与装饰器
ANALYZER_REGISTRY: Dict[str, Type[object]] = {}


def register_analyzer(method: AnalysisMethod):
    """注册分析器类的装饰器。分析器需实现 analyze(self, mfa, factor_name, config) -> FactorResult。"""
    def decorator(cls: Type[object]) -> Type[object]:
        ANALYZER_REGISTRY[method.value] = cls
        return cls
    return decorator


class MultiFactorAnalyzer:
    """多因子批量分析器"""
    
    def __init__(
        self,
        bars: List[Dict[str, Any]],
        risk_free_rate: float = 0.02,
        datetime_col: str = "datetime",
        symbol_col: str = "symbol",
        window_size: int = 60,
        nan_policy: str = "drop",  # drop | zero
        winsorize: Optional[Tuple[float, float]] = None,  # e.g., (0.01, 0.99)
        standardize: bool = False,  # z-score within cross section
        horizon_unit: str = "bars",  # bars | days | months | quarters (informational)
    ):
        """
        初始化多因子分析器
        
        Args:
            bars: 包含价格和因子数据的bar列表
            risk_free_rate: 无风险利率
        """
        self.bars = bars
        self.risk_free_rate = risk_free_rate
        self.factor_names = []
        self.datetime_col = datetime_col
        self.symbol_col = symbol_col
        self.window_size = int(window_size)
        self.nan_policy = nan_policy
        self.winsorize = winsorize
        self.standardize = standardize
        self.horizon_unit = horizon_unit
        self._extract_factor_names()
        # 构建 DataFrame 以便向量化计算
        self.df = pd.DataFrame(self.bars)
        # 规范化价格列为 float
        if not self.df.empty and "close" in self.df.columns:
            self.df["close"] = pd.to_numeric(self.df["close"], errors="coerce").astype(float)

    @classmethod
    def from_feeds(
        cls,
        feeds: Dict[str, List[Dict[str, Any]]],
        risk_free_rate: float = 0.02,
        datetime_col: str = "datetime",
        symbol_col: str = "symbol",
        window_size: int = 60,
        nan_policy: str = "drop",
        winsorize: Optional[Tuple[float, float]] = None,
        standardize: bool = False,
        horizon_unit: str = "bars",
    ) -> "MultiFactorAnalyzer":
        """从多资产 feeds 字典创建分析器（自动展开为 bars 列表）。"""
        bars: List[Dict[str, Any]] = []
        for sym, rows in feeds.items():
            for row in rows:
                b = dict(row)
                if symbol_col not in b:
                    b[symbol_col] = sym
                bars.append(b)
        return cls(
            bars,
            risk_free_rate=risk_free_rate,
            datetime_col=datetime_col,
            symbol_col=symbol_col,
            window_size=window_size,
            nan_policy=nan_policy,
            winsorize=winsorize,
            standardize=standardize,
            horizon_unit=horizon_unit,
        )
    
    def _extract_factor_names(self):
        """提取所有因子名称"""
        if not self.bars:
            return
        
        # 从第一个bar中提取所有因子名称
        first_bar = self.bars[0]
        price_fields = {'datetime', 'open', 'high', 'low', 'close', 'volume', 'symbol'}
        self.factor_names = [k for k in first_bar.keys() if k not in price_fields]
    
    def analyze_single_factor(self, factor_name: str, quantiles: int = 5, 
                            forward_periods: List[int] = [1, 5, 10, 20]) -> FactorResult:
        """
        分析单个因子
        
        Args:
            factor_name: 因子名称
            quantiles: 分位数数量
            forward_periods: 前向收益期数列表
            
        Returns:
            FactorResult: 因子评价结果
        """
        # 基础因子回测
        fb_result = factor_backtest(self.bars, factor_name, quantiles, forward_periods[0])
        
        # 计算IC时间序列
        ic_series = self._calculate_ic_series(factor_name, forward_periods[0])
        
        # 计算IC信息比率
        ic_ir = np.mean(ic_series) / (np.std(ic_series) + 1e-8) if len(ic_series) > 1 else 0.0
        
        # 计算IC胜率
        ic_win_rate = np.sum(np.array(ic_series) > 0) / len(ic_series) if ic_series else 0.0
        
        # 计算秩相关系数
        rank_ic = self._calculate_rank_ic(factor_name, forward_periods[0])
        
        # 计算换手率
        turnover_rate = self._calculate_turnover_rate(factor_name, quantiles)
        
        # 因子衰减分析
        decay_analysis = self._analyze_factor_decay(factor_name, forward_periods)
        
        # 计算稳定性评分
        stability_score = self._calculate_stability_score(factor_name, ic_series)
        
        return FactorResult(
            factor_name=factor_name,
            ic=fb_result.get('ic', 0.0),
            ic_ir=ic_ir,
            ic_win_rate=ic_win_rate,
            monotonicity=fb_result.get('monotonicity', 0.0),
            quantile_returns=fb_result.get('mean_returns', []),
            factor_stats=fb_result.get('factor_stats', {}),
            rank_ic=rank_ic,
            turnover_rate=turnover_rate,
            decay_analysis=decay_analysis,
            stability_score=stability_score
        )
    
    def _calculate_ic_series(self, factor_name: str, forward: int) -> List[float]:
        """计算IC时间序列（优先向量化；大样本可自动启用 numba 加速）。"""
        if self.df.empty or factor_name not in self.df.columns or "close" not in self.df.columns:
            return []

        s_fac = pd.to_numeric(self.df[factor_name], errors="coerce").astype(float)
        s_close = self.df["close"].astype(float)
        # 对齐 forward 期收益（与因子同索引左对齐）
        s_ret = (s_close.shift(-forward) / s_close) - 1.0
        s_fac = s_fac.iloc[:-forward] if forward > 0 else s_fac
        s_ret = s_ret.iloc[:-forward] if forward > 0 else s_ret

        window_size = self.window_size

        # 大样本且 numba 可用时，采用分段窗口的 numba JIT 实现
        if NUMBA_AVAILABLE and len(s_fac) >= 5000:
            fac_arr = s_fac.to_numpy(dtype=np.float64)
            ret_arr = s_ret.to_numpy(dtype=np.float64)

            @njit(cache=True)
            def rolling_corr_stepwise(x: np.ndarray, y: np.ndarray, win: int) -> np.ndarray:
                n = x.shape[0]
                if n < win:
                    return np.empty((0,), dtype=np.float64)
                num_windows = (n // win)
                out = np.empty((num_windows,), dtype=np.float64)
                k = 0
                for start in range(0, n - win + 1, win):
                    end = start + win
                    # 计算窗口内皮尔逊相关
                    sx = 0.0
                    sy = 0.0
                    for i in range(start, end):
                        sx += x[i]
                        sy += y[i]
                    mx = sx / win
                    my = sy / win
                    cov = 0.0
                    vx = 0.0
                    vy = 0.0
                    for i in range(start, end):
                        dx = x[i] - mx
                        dy = y[i] - my
                        cov += dx * dy
                        vx += dx * dx
                        vy += dy * dy
                    denom = math.sqrt(vx * vy) + 1e-12
                    out[k] = cov / denom if denom > 0.0 else 0.0
                    k += 1
                return out

            vals = rolling_corr_stepwise(fac_arr, ret_arr, window_size)
            return [float(v) for v in vals if not np.isnan(v)]

        # 默认走 Pandas 向量化 rolling 相关系数（步长为1，更细粒度）
        ic_rolling = s_fac.rolling(window_size, min_periods=10).corr(s_ret)
        ic_series = ic_rolling.dropna().to_list()
        return [float(v) for v in ic_series]
    
    def _calculate_rank_ic(self, factor_name: str, forward: int) -> float:
        """计算秩相关系数（Spearman），向量化实现。"""
        if self.df.empty or factor_name not in self.df.columns or "close" not in self.df.columns:
            return 0.0
        s_fac = pd.to_numeric(self.df[factor_name], errors="coerce").astype(float)
        s_close = self.df["close"].astype(float)
        s_ret = (s_close.shift(-forward) / s_close) - 1.0
        # 对齐长度
        s_fac = s_fac.iloc[:-forward] if forward > 0 else s_fac
        s_ret = s_ret.iloc[:-forward] if forward > 0 else s_ret
        if s_fac.size <= 1:
            return 0.0
        return float(s_fac.corr(s_ret, method="spearman")) if s_fac.notna().any() and s_ret.notna().any() else 0.0
    
    def _calculate_turnover_rate(self, factor_name: str, quantiles: int) -> float:
        """计算因子换手率（向量化：绝对收益率的均值）。"""
        if self.df.empty or factor_name not in self.df.columns:
            return 0.0
        s = pd.to_numeric(self.df[factor_name], errors="coerce").astype(float)
        pct = s.pct_change().abs()
        return float(pct.mean(skipna=True)) if pct.size else 0.0

    def analyze_single_factor_cs(
        self,
        factor_name: str,
        quantiles: int = 5,
        forward: int = 1,
    ) -> FactorResult:
        """
        横截面因子评价：按每个交易日对多资产做分位分桶与截面 IC。
        需要存在 symbol 与 datetime 列。
        """
        df = self.df.copy()
        if df.empty or factor_name not in df.columns or "close" not in df.columns:
            return FactorResult(
                factor_name=factor_name,
                ic=0.0,
                ic_ir=0.0,
                ic_win_rate=0.0,
                monotonicity=0.0,
                quantile_returns=[],
                factor_stats={},
                rank_ic=0.0,
                turnover_rate=0.0,
                decay_analysis={},
                stability_score=0.0,
            )

        # 标准化列名
        if self.datetime_col in df.columns:
            df["__dt"] = pd.to_datetime(df[self.datetime_col], errors="coerce")
        else:
            df["__dt"] = pd.to_datetime(df.get("datetime"), errors="coerce")
        if self.symbol_col not in df.columns and "symbol" in df.columns:
            df[self.symbol_col] = df["symbol"]

        cols = [self.symbol_col, "__dt", "close", factor_name]
        df = df[cols].copy()
        df = df.dropna(subset=["__dt", "close"])  # 必要字段

        # 因子预处理策略（逐日）：winsorize → standardize
        def _winsorize_series(x: pd.Series, limits: Tuple[float, float]) -> pd.Series:
            lo, hi = limits
            ql = x.quantile(lo)
            qh = x.quantile(hi)
            return x.clip(ql, qh)

        # 计算 forward 收益（按 symbol 分组）
        df.sort_values([self.symbol_col, "__dt"], inplace=True)
        grp = df.groupby(self.symbol_col, sort=False)
        fwd_close = grp["close"].shift(-forward)
        df["ret_fwd"] = (fwd_close / df["close"]) - 1.0

        # 处理因子缺失
        s_fac = pd.to_numeric(df[factor_name], errors="coerce").astype(float)
        if self.nan_policy == "zero":
            s_fac = s_fac.fillna(0.0)
        df[factor_name] = s_fac

        if self.winsorize is not None:
            df[factor_name] = df.groupby("__dt")[factor_name].transform(
                lambda x: _winsorize_series(pd.to_numeric(x, errors="coerce").astype(float), self.winsorize)  # type: ignore[arg-type]
            )
        if self.standardize:
            df[factor_name] = df.groupby("__dt")[factor_name].transform(
                lambda x: (x - x.mean()) / (x.std(ddof=0) + 1e-12)
            )

        # 逐日截面分位分桶
        ranks_pct = df.groupby("__dt")[factor_name].rank(pct=True, method="first")
        bucket = np.floor(ranks_pct * quantiles)
        bucket = bucket.clip(0, quantiles - 1)
        # 使用可空整型，避免 NaN 转换错误
        df["__q"] = bucket.astype("Int64")

        # 逐日每个分位的平均 forward 收益 → 再对时间求均值
        q_ret_daily = df.groupby(["__dt", "__q"], sort=False)["ret_fwd"].mean()
        q_ret = q_ret_daily.groupby("__q").mean().reindex(range(quantiles), fill_value=0.0)
        mean_returns = q_ret.to_list()

        # 截面 IC：逐日 spearman
        ic_by_date = df.groupby("__dt").apply(
            lambda g: g[factor_name].corr(g["ret_fwd"], method="spearman")
        )
        ic_by_date = ic_by_date.replace([np.inf, -np.inf], np.nan).dropna()
        ic = float(ic_by_date.mean()) if not ic_by_date.empty else 0.0
        ic_ir = float(ic / (ic_by_date.std(ddof=0) + 1e-12)) if not ic_by_date.empty else 0.0
        ic_win_rate = float((ic_by_date > 0).mean()) if not ic_by_date.empty else 0.0

        monotonicity = 0.0
        if len(mean_returns) > 1:
            inc = sum(1 for i in range(1, len(mean_returns)) if mean_returns[i] > mean_returns[i - 1])
            dec = sum(1 for i in range(1, len(mean_returns)) if mean_returns[i] < mean_returns[i - 1])
            denom = (len(mean_returns) - 1)
            monotonicity = (inc - dec) / denom if denom > 0 else 0.0

        fac_all = pd.to_numeric(df[factor_name], errors="coerce").astype(float)
        factor_stats = {
            "mean": float(fac_all.mean(skipna=True)) if fac_all.size else 0.0,
            "std": float(fac_all.std(ddof=0, skipna=True)) if fac_all.size else 0.0,
            "min": float(fac_all.min(skipna=True)) if fac_all.size else 0.0,
            "max": float(fac_all.max(skipna=True)) if fac_all.size else 0.0,
        }

        # Rank IC（与 spearman 相同）
        rank_ic = ic

        # 最高分位组合的换手率（逐日成分变化）
        df_top = df[df["__q"] == (quantiles - 1)][["__dt", self.symbol_col]]
        df_top = df_top.groupby("__dt")[self.symbol_col].apply(set).sort_index()
        turnover_vals: List[float] = []
        prev_set: Optional[set] = None
        for _, curr_set in df_top.items():
            if prev_set is None:
                prev_set = curr_set
                continue
            if not prev_set:
                turnover_vals.append(0.0)
            else:
                inter = len(prev_set.intersection(curr_set))
                turnover = 1.0 - (inter / max(1, len(prev_set)))
                turnover_vals.append(float(turnover))
            prev_set = curr_set
        turnover_rate = float(np.mean(turnover_vals)) if turnover_vals else 0.0

        # 衰减：不同 forward 的截面 ic 均值
        decay_analysis: Dict[str, float] = {}
        for fwd in [1, 5, 10, 20]:
            df_tmp = df.copy()
            df_tmp.sort_values([self.symbol_col, "__dt"], inplace=True)
            grp_tmp = df_tmp.groupby(self.symbol_col, sort=False)
            fwd_close_tmp = grp_tmp["close"].shift(-fwd)
            df_tmp["ret_fwd"] = (fwd_close_tmp / df_tmp["close"]) - 1.0
            ic_series_tmp = df_tmp.groupby("__dt").apply(
                lambda g: g[factor_name].corr(g["ret_fwd"], method="spearman")
            ).replace([np.inf, -np.inf], np.nan).dropna()
            decay_analysis[f"ic_{fwd}d"] = float(ic_series_tmp.mean()) if not ic_series_tmp.empty else 0.0

        stability_score = 0.0
        if not ic_by_date.empty:
            ic_std = float(ic_by_date.std(ddof=0))
            ic_mean = float(ic_by_date.mean())
            stability_score = min(abs(ic_mean) / (ic_std + 1e-12) / 10.0, 1.0)

        return FactorResult(
            factor_name=factor_name,
            ic=ic,
            ic_ir=ic_ir,
            ic_win_rate=ic_win_rate,
            monotonicity=monotonicity,
            quantile_returns=mean_returns,
            factor_stats=factor_stats,
            rank_ic=rank_ic,
            turnover_rate=turnover_rate,
            decay_analysis=decay_analysis,
            stability_score=stability_score,
        )
    
    def _analyze_factor_decay(self, factor_name: str, forward_periods: List[int]) -> Dict[str, float]:
        """分析因子衰减特征"""
        decay_ics = {}
        
        for forward in forward_periods:
            fb_result = factor_backtest(self.bars, factor_name, 5, forward)
            decay_ics[f'ic_{forward}d'] = fb_result.get('ic', 0.0)
        
        return decay_ics
    
    def _calculate_stability_score(self, factor_name: str, ic_series: List[float]) -> float:
        """计算因子稳定性评分"""
        if len(ic_series) < 2:
            return 0.0
        
        # 基于IC的标准差计算稳定性
        ic_std = np.std(ic_series)
        ic_mean = np.mean(ic_series)
        
        # 稳定性评分 = IC均值 / (IC标准差 + 1e-8)
        stability = abs(ic_mean) / (ic_std + 1e-8)
        
        # 归一化到0-1区间
        return min(stability / 10.0, 1.0)
    
    def _calculate_correlation(self, x: List[float], y: List[float]) -> float:
        """计算皮尔逊相关系数"""
        if len(x) != len(y) or len(x) < 2:
            return 0.0
        
        x_mean = np.mean(x)
        y_mean = np.mean(y)
        
        numerator = sum((xi - x_mean) * (yi - y_mean) for xi, yi in zip(x, y))
        x_var = sum((xi - x_mean) ** 2 for xi in x)
        y_var = sum((yi - y_mean) ** 2 for yi in y)
        
        denominator = math.sqrt(x_var * y_var)
        return numerator / (denominator + 1e-12)
    
    def analyze_all_factors(
        self,
        quantiles: int = 5,
        forward_periods: List[int] = [1, 5, 10, 20],
        method: str = "ts",  # ts: 时间序列; cs: 横截面
    ) -> MultiFactorReport:
        """
        分析所有因子
        
        Args:
            quantiles: 分位数数量
            forward_periods: 前向收益期数列表
            
        Returns:
            MultiFactorReport: 多因子评价报告
        """
        factor_results = {}
        
        # 分析每个因子
        for factor_name in self.factor_names:
            try:
                if method == "cs":
                    result = self.analyze_single_factor_cs(factor_name, quantiles, forward_periods[0])
                else:
                    result = self.analyze_single_factor(factor_name, quantiles, forward_periods)
                factor_results[factor_name] = result
            except Exception as e:
                print(f"分析因子 {factor_name} 时出错: {e}")
                continue
        
        # 计算因子排名
        factor_ranking = self._calculate_factor_ranking(factor_results)
        
        # 计算因子间相关性矩阵
        correlation_matrix = self._calculate_correlation_matrix()
        
        # 生成汇总统计
        summary_stats = self._generate_summary_stats(factor_results)
        
        return MultiFactorReport(
            factor_results=factor_results,
            factor_ranking=factor_ranking,
            correlation_matrix=correlation_matrix,
            summary_stats=summary_stats,
            report_timestamp=datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        )

    # 基于配置对象的调度（利用装饰器注册的分析器）
    def analyze_all_factors_with_config(self, config: FactorConfig) -> MultiFactorReport:
        analyzer_cls = ANALYZER_REGISTRY.get(config.method.value)
        # 若未注册，回退到字符串分支
        if analyzer_cls is None:
            return self.analyze_all_factors(
                quantiles=config.quantiles,
                forward_periods=config.forward_periods,
                method=config.method.value,
            )

        analyzer = analyzer_cls()  # type: ignore[call-arg]
        factor_results: Dict[str, FactorResult] = {}
        for factor_name in self.factor_names:
            try:
                result = analyzer.analyze(self, factor_name, config)  # type: ignore[attr-defined]
                factor_results[factor_name] = result
            except Exception as e:
                print(f"分析因子 {factor_name} 时出错: {e}")
                continue

        factor_ranking = self._calculate_factor_ranking(factor_results)
        correlation_matrix = self._calculate_correlation_matrix()
        summary_stats = self._generate_summary_stats(factor_results)
        return MultiFactorReport(
            factor_results=factor_results,
            factor_ranking=factor_ranking,
            correlation_matrix=correlation_matrix,
            summary_stats=summary_stats,
            report_timestamp=datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        )
    
    def _calculate_factor_ranking(self, factor_results: Dict[str, FactorResult]) -> List[Tuple[str, float]]:
        """计算因子综合排名"""
        rankings = []
        
        for factor_name, result in factor_results.items():
            # 综合评分 = IC * 0.3 + IC_IR * 0.2 + 单调性 * 0.2 + 稳定性 * 0.2 + 胜率 * 0.1
            score = (abs(result.ic) * 0.3 + 
                    abs(result.ic_ir) * 0.2 + 
                    abs(result.monotonicity) * 0.2 + 
                    result.stability_score * 0.2 + 
                    result.ic_win_rate * 0.1)
            
            rankings.append((factor_name, score))
        
        # 按评分降序排列
        rankings.sort(key=lambda x: x[1], reverse=True)
        return rankings
    
    def _calculate_correlation_matrix(self) -> pd.DataFrame:
        """计算因子间相关性矩阵（pandas 成对相关，缺失值不填 0）"""
        if not self.factor_names:
            return pd.DataFrame()
        
        # 直接复用 self.df，避免逐 bar 重建 list；None 经 coerce 变 NaN，
        # corr() 自动做成对（pairwise）处理
        cols = [c for c in self.factor_names if c in self.df.columns]
        if not cols:
            return pd.DataFrame()
        df = self.df[cols].apply(pd.to_numeric, errors="coerce")
        return df.corr()
    
    def _generate_summary_stats(self, factor_results: Dict[str, FactorResult]) -> Dict[str, Any]:
        """生成汇总统计"""
        if not factor_results:
            return {}
        
        # 统计IC分布
        ics = [result.ic for result in factor_results.values()]
        ic_irs = [result.ic_ir for result in factor_results.values()]
        
        return {
            "total_factors": len(factor_results),
            "avg_ic": np.mean(ics),
            "std_ic": np.std(ics),
            "max_ic": np.max(ics),
            "min_ic": np.min(ics),
            "avg_ic_ir": np.mean(ic_irs),
            "positive_ic_count": sum(1 for ic in ics if ic > 0),
            "negative_ic_count": sum(1 for ic in ics if ic < 0),
            "avg_stability": np.mean([result.stability_score for result in factor_results.values()]),
            "avg_turnover": np.mean([result.turnover_rate for result in factor_results.values()])
        }
    
    def export_report(self, report: MultiFactorReport, output_dir: str = "factor_reports"):
        """导出因子评价报告"""
        os.makedirs(output_dir, exist_ok=True)
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        
        # 导出详细报告
        self._export_detailed_report(report, output_dir, timestamp)
        
        # 导出因子排名
        self._export_factor_ranking(report, output_dir, timestamp)
        
        # 导出相关性矩阵
        self._export_correlation_matrix(report, output_dir, timestamp)
        
        # 导出汇总统计
        self._export_summary_stats(report, output_dir, timestamp)
        
        print(f"因子评价报告已导出到: {output_dir}")
    
    def _export_detailed_report(self, report: MultiFactorReport, output_dir: str, timestamp: str):
        """导出详细报告"""
        filename = os.path.join(output_dir, f"detailed_report_{timestamp}.json")
        
        detailed_data = {}
        for factor_name, result in report.factor_results.items():
            detailed_data[factor_name] = {
                "ic": result.ic,
                "ic_ir": result.ic_ir,
                "ic_win_rate": result.ic_win_rate,
                "monotonicity": result.monotonicity,
                "rank_ic": result.rank_ic,
                "turnover_rate": result.turnover_rate,
                "stability_score": result.stability_score,
                "quantile_returns": result.quantile_returns,
                "factor_stats": result.factor_stats,
                "decay_analysis": result.decay_analysis
            }
        
        with open(filename, 'w', encoding='utf-8') as f:
            json.dump(detailed_data, f, indent=2, ensure_ascii=False)
    
    def _export_factor_ranking(self, report: MultiFactorReport, output_dir: str, timestamp: str):
        """导出因子排名"""
        filename = os.path.join(output_dir, f"factor_ranking_{timestamp}.csv")
        
        with open(filename, 'w', newline='', encoding='utf-8') as f:
            writer = csv.writer(f)
            writer.writerow(['Rank', 'Factor_Name', 'Score', 'IC', 'IC_IR', 'Monotonicity', 'Stability'])
            
            for rank, (factor_name, score) in enumerate(report.factor_ranking, 1):
                result = report.factor_results[factor_name]
                writer.writerow([
                    rank, factor_name, f"{score:.4f}", 
                    f"{result.ic:.4f}", f"{result.ic_ir:.4f}", 
                    f"{result.monotonicity:.4f}", f"{result.stability_score:.4f}"
                ])
    
    def _export_correlation_matrix(self, report: MultiFactorReport, output_dir: str, timestamp: str):
        """导出相关性矩阵"""
        filename = os.path.join(output_dir, f"correlation_matrix_{timestamp}.csv")
        report.correlation_matrix.to_csv(filename)
    
    def _export_summary_stats(self, report: MultiFactorReport, output_dir: str, timestamp: str):
        """导出汇总统计"""
        filename = os.path.join(output_dir, f"summary_stats_{timestamp}.json")
        
        with open(filename, 'w', encoding='utf-8') as f:
            json.dump(report.summary_stats, f, indent=2, ensure_ascii=False) 