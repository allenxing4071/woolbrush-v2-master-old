/// 气温市场类型
export type TempMarketType = "highest";

/// 温度档位（一个可交易的结果区间）
export interface TempThreshold {
  /** 档位标识，如 "21°C" / "30°C or higher" */
  label: string;
  /** 市场问题文本 */
  question: string;
  /** 市场 slug */
  slug: string;
  /** 市场 ID */
  market_id: string;
  /** 条件 ID */
  condition_id: string;
  /** YES token ID */
  yes_token_id: string;
  /** NO token ID */
  no_token_id: string;
  /** YES 当前价格 (0.0 - 1.0) */
  yes_price: number;
  /** NO 当前价格 (0.0 - 1.0) */
  no_price: number;
}

/// 气温市场事件（一个城市+日期+类型的完整事件）
export interface TempMarketEvent {
  event_id: string;
  event_slug: string;
  title: string;
  city: string;
  city_tz: string;
  market_type: TempMarketType;
  end_date_iso: string;
  image: string | null;
  icon: string | null;
  thresholds: TempThreshold[];
}

/// 城市气温市场汇总（一个城市的最高温市场）
export interface CityTempMarkets {
  city: string;
  city_tz: string;
  highest: TempMarketEvent | null;
}
