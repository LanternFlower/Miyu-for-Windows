//! Open-Meteo 各接口的响应结构。
//!
//! 纯 serde，不含逻辑。六个接口（预报、空气质量、历史、海洋、气候、海拔）返回
//! 的字段几乎不重叠，所以各有一组结构而不是一个大结构塞满 Option。

use crate::tools::weather::*;

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct GeocodingResponse {
    pub(in crate::tools::weather) results: Option<Vec<GeocodingResult>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct GeocodingResult {
    pub(in crate::tools::weather) name: String,
    pub(in crate::tools::weather) latitude: f64,
    pub(in crate::tools::weather) longitude: f64,
    pub(in crate::tools::weather) elevation: Option<f64>,
    pub(in crate::tools::weather) feature_code: Option<String>,
    pub(in crate::tools::weather) country_code: Option<String>,
    pub(in crate::tools::weather) country: Option<String>,
    pub(in crate::tools::weather) admin1: Option<String>,
    pub(in crate::tools::weather) admin2: Option<String>,
    pub(in crate::tools::weather) timezone: Option<String>,
    pub(in crate::tools::weather) population: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct ForecastResponse {
    pub(in crate::tools::weather) current: CurrentWeather,
    pub(in crate::tools::weather) daily: DailyWeather,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct CurrentWeather {
    pub(in crate::tools::weather) time: String,
    pub(in crate::tools::weather) temperature_2m: Option<f64>,
    pub(in crate::tools::weather) relative_humidity_2m: Option<u64>,
    pub(in crate::tools::weather) apparent_temperature: Option<f64>,
    pub(in crate::tools::weather) is_day: Option<u8>,
    pub(in crate::tools::weather) precipitation: Option<f64>,
    pub(in crate::tools::weather) weather_code: Option<i64>,
    pub(in crate::tools::weather) cloud_cover: Option<u64>,
    pub(in crate::tools::weather) wind_speed_10m: Option<f64>,
    pub(in crate::tools::weather) wind_direction_10m: Option<f64>,
    pub(in crate::tools::weather) wind_gusts_10m: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct DailyWeather {
    pub(in crate::tools::weather) time: Vec<String>,
    pub(in crate::tools::weather) weather_code: Option<Vec<i64>>,
    pub(in crate::tools::weather) temperature_2m_max: Option<Vec<f64>>,
    pub(in crate::tools::weather) temperature_2m_min: Option<Vec<f64>>,
    pub(in crate::tools::weather) precipitation_sum: Option<Vec<f64>>,
    pub(in crate::tools::weather) precipitation_probability_max: Option<Vec<u64>>,
    pub(in crate::tools::weather) wind_speed_10m_max: Option<Vec<f64>>,
    pub(in crate::tools::weather) wind_gusts_10m_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct AirQualityResponse {
    pub(in crate::tools::weather) current: AirQualityCurrent,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct AirQualityCurrent {
    pub(in crate::tools::weather) time: String,
    pub(in crate::tools::weather) european_aqi: Option<u64>,
    pub(in crate::tools::weather) us_aqi: Option<u64>,
    pub(in crate::tools::weather) pm10: Option<f64>,
    pub(in crate::tools::weather) pm2_5: Option<f64>,
    pub(in crate::tools::weather) carbon_monoxide: Option<f64>,
    pub(in crate::tools::weather) nitrogen_dioxide: Option<f64>,
    pub(in crate::tools::weather) sulphur_dioxide: Option<f64>,
    pub(in crate::tools::weather) ozone: Option<f64>,
    pub(in crate::tools::weather) uv_index: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct HistoricalResponse {
    pub(in crate::tools::weather) daily: HistoricalDaily,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct HistoricalDaily {
    pub(in crate::tools::weather) time: Vec<String>,
    pub(in crate::tools::weather) weather_code: Option<Vec<i64>>,
    pub(in crate::tools::weather) temperature_2m_max: Option<Vec<f64>>,
    pub(in crate::tools::weather) temperature_2m_min: Option<Vec<f64>>,
    pub(in crate::tools::weather) precipitation_sum: Option<Vec<f64>>,
    pub(in crate::tools::weather) wind_speed_10m_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct MarineResponse {
    pub(in crate::tools::weather) current: MarineCurrent,
    pub(in crate::tools::weather) daily: MarineDaily,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct MarineCurrent {
    pub(in crate::tools::weather) time: String,
    pub(in crate::tools::weather) wave_height: Option<f64>,
    pub(in crate::tools::weather) wave_direction: Option<f64>,
    pub(in crate::tools::weather) wave_period: Option<f64>,
    pub(in crate::tools::weather) sea_surface_temperature: Option<f64>,
    pub(in crate::tools::weather) ocean_current_velocity: Option<f64>,
    pub(in crate::tools::weather) ocean_current_direction: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct MarineDaily {
    pub(in crate::tools::weather) time: Vec<String>,
    pub(in crate::tools::weather) wave_height_max: Option<Vec<f64>>,
    pub(in crate::tools::weather) wave_direction_dominant: Option<Vec<f64>>,
    pub(in crate::tools::weather) wave_period_max: Option<Vec<f64>>,
    pub(in crate::tools::weather) swell_wave_height_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct ClimateResponse {
    pub(in crate::tools::weather) daily: ClimateDaily,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct ClimateDaily {
    pub(in crate::tools::weather) time: Vec<String>,
    pub(in crate::tools::weather) temperature_2m_mean: Option<Vec<f64>>,
    pub(in crate::tools::weather) temperature_2m_max: Option<Vec<f64>>,
    pub(in crate::tools::weather) temperature_2m_min: Option<Vec<f64>>,
    pub(in crate::tools::weather) precipitation_sum: Option<Vec<f64>>,
    pub(in crate::tools::weather) wind_speed_10m_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::tools::weather) struct ElevationResponse {
    pub(in crate::tools::weather) elevation: Vec<Option<f64>>,
}
