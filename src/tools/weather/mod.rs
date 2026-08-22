mod format;
mod geocode;
mod wire;
use format::*;
use geocode::*;
use wire::*;

use super::{ToolRegistry, ToolSpec};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const TOOL_NAME: &str = "get_weather";
const TOOL_DESC: &str = "Weather data query. query_type defaults to forecast; supports forecast (current weather and forecast), air_quality, historical, marine, climate, and elevation. location is required: pass a city, place name, postal code, or airport code. Do not guess; ask the user when the location is missing. days is optional, default 3. start_date/end_date apply to historical and climate. country_code is an optional ISO-3166-1 alpha2 code (e.g. CN, JP, US) to disambiguate identically named places.";
const OPEN_METEO_GEOCODING_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const OPEN_METEO_FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
const OPEN_METEO_AIR_QUALITY_URL: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";
const OPEN_METEO_ARCHIVE_URL: &str = "https://archive-api.open-meteo.com/v1/archive";
const OPEN_METEO_MARINE_URL: &str = "https://marine-api.open-meteo.com/v1/marine";
const OPEN_METEO_CLIMATE_URL: &str = "https://climate-api.open-meteo.com/v1/climate";
const OPEN_METEO_ELEVATION_URL: &str = "https://api.open-meteo.com/v1/elevation";

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        TOOL_NAME,
        TOOL_DESC,
        json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "Required city, place name, postal code, or airport code. Do not guess; ask the user when it is missing." },
                "query_type": { "type": "string", "enum": ["forecast", "air_quality", "historical", "marine", "climate", "elevation"], "description": "Query type. forecast=current weather and forecast, air_quality, historical, marine, climate=climate trends, elevation. Defaults to forecast." },
                "days": { "type": "integer", "description": "Number of forecast days, default 3, max 7." },
                "start_date": { "type": "string", "description": "Start date, format YYYY-MM-DD. Used by historical and climate." },
                "end_date": { "type": "string", "description": "End date, format YYYY-MM-DD. Used by historical and climate; defaults to start_date when omitted." },
                "country_code": { "type": "string", "description": "Optional ISO-3166-1 alpha2 country code to disambiguate identically named places, e.g. CN, JP, US." }
            },
            "additionalProperties": false
        }),
        |args| async move { get_weather(args).await },
    ));
}

async fn get_weather(args: Value) -> Result<String> {
    let request = WeatherRequest::from_args(&args);
    if request.location.is_empty() {
        bail!(
            "location is required. Ask the user which city or place to query, then call get_weather again with that location."
        );
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("miyu-weather/0.1")
        .build()?;

    match request.query_type {
        WeatherQueryType::Forecast => match get_weather_open_meteo(&client, &request).await {
            Ok(weather) => Ok(weather),
            Err(open_meteo_error) => {
                get_weather_wttr(&client, &request.location, "open_meteo_fallback")
                    .await
                    .map_err(|wttr_error| {
                        anyhow!(
                    "weather query failed; open_meteo: {open_meteo_error}; wttr.in: {wttr_error}"
                )
                    })
            }
        },
        WeatherQueryType::AirQuality => get_air_quality_open_meteo(&client, &request).await,
        WeatherQueryType::Historical => get_historical_open_meteo(&client, &request).await,
        WeatherQueryType::Marine => get_marine_open_meteo(&client, &request).await,
        WeatherQueryType::Climate => get_climate_open_meteo(&client, &request).await,
        WeatherQueryType::Elevation => get_elevation_open_meteo(&client, &request).await,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WeatherQueryType {
    Forecast,
    AirQuality,
    Historical,
    Marine,
    Climate,
    Elevation,
}

impl WeatherQueryType {
    fn from_args(args: &Value) -> Self {
        match args
            .get("query_type")
            .and_then(Value::as_str)
            .unwrap_or("forecast")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "air_quality" | "air-quality" | "air" | "aqi" => Self::AirQuality,
            "historical" | "history" | "archive" => Self::Historical,
            "marine" | "sea" | "ocean" => Self::Marine,
            "climate" | "climate_change" | "climate-change" => Self::Climate,
            "elevation" | "altitude" => Self::Elevation,
            _ => Self::Forecast,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Forecast => "forecast",
            Self::AirQuality => "air_quality",
            Self::Historical => "historical",
            Self::Marine => "marine",
            Self::Climate => "climate",
            Self::Elevation => "elevation",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WeatherRequest {
    location: String,
    query_type: WeatherQueryType,
    days: usize,
    start_date: Option<String>,
    end_date: Option<String>,
    country_code: Option<String>,
}

impl WeatherRequest {
    fn from_args(args: &Value) -> Self {
        let location = args
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let days = args
            .get("days")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 7) as usize;
        let start_date = optional_trimmed_string(args, "start_date");
        let end_date = optional_trimmed_string(args, "end_date");
        let country_code = args
            .get("country_code")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_uppercase());

        Self {
            location,
            query_type: WeatherQueryType::from_args(args),
            days,
            start_date,
            end_date,
            country_code,
        }
    }
}

fn optional_trimmed_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn get_weather_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let locations = geocode_location(client, request).await?;
    let selected = select_location(&request.location, &locations)
        .ok_or_else(|| anyhow!("no geocoding results for {}", request.location))?;
    let forecast = fetch_forecast(client, selected, request.days).await?;
    format_open_meteo_result(selected, &locations, &forecast)
}

async fn get_air_quality_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let days = request.days.min(7).to_string();
    let response: AirQualityResponse = client
        .get(OPEN_METEO_AIR_QUALITY_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            (
                "current",
                "european_aqi,us_aqi,pm10,pm2_5,carbon_monoxide,nitrogen_dioxide,sulphur_dioxide,ozone,uv_index",
            ),
            ("forecast_days", days.as_str()),
            ("timezone", "auto"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_air_quality_result(&selected, &locations, &response)
}

async fn get_historical_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let start_date = request
        .start_date
        .as_deref()
        .ok_or_else(|| anyhow!("historical query requires start_date"))?;
    let end_date = request.end_date.as_deref().unwrap_or(start_date);
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let response: HistoricalResponse = client
        .get(OPEN_METEO_ARCHIVE_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            ("start_date", start_date),
            ("end_date", end_date),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,wind_speed_10m_max",
            ),
            ("timezone", "auto"),
            ("temperature_unit", "celsius"),
            ("wind_speed_unit", "kmh"),
            ("precipitation_unit", "mm"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_historical_result(&selected, &locations, &response, start_date, end_date)
}

async fn get_marine_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let days = request.days.min(7).to_string();
    let response: MarineResponse = client
        .get(OPEN_METEO_MARINE_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            (
                "current",
                "wave_height,wave_direction,wave_period,sea_surface_temperature,ocean_current_velocity,ocean_current_direction",
            ),
            (
                "daily",
                "wave_height_max,wave_direction_dominant,wave_period_max,swell_wave_height_max",
            ),
            ("forecast_days", days.as_str()),
            ("timezone", "auto"),
            ("length_unit", "metric"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_marine_result(&selected, &locations, &response)
}

async fn get_climate_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let start_date = request
        .start_date
        .as_deref()
        .ok_or_else(|| anyhow!("climate query requires start_date"))?;
    let end_date = request.end_date.as_deref().unwrap_or(start_date);
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let response: ClimateResponse = client
        .get(OPEN_METEO_CLIMATE_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            ("start_date", start_date),
            ("end_date", end_date),
            ("models", "EC_Earth3P_HR"),
            (
                "daily",
                "temperature_2m_mean,temperature_2m_max,temperature_2m_min,precipitation_sum,wind_speed_10m_max",
            ),
            ("temperature_unit", "celsius"),
            ("wind_speed_unit", "kmh"),
            ("precipitation_unit", "mm"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_climate_result(&selected, &locations, &response, start_date, end_date)
}

async fn get_elevation_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let response: ElevationResponse = client
        .get(OPEN_METEO_ELEVATION_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_elevation_result(&selected, &locations, &response)
}

async fn fetch_forecast(
    client: &reqwest::Client,
    location: &GeocodingResult,
    days: usize,
) -> Result<ForecastResponse> {
    let cache_key = format!(
        "{:.3}|{:.3}|{}",
        location.latitude, location.longitude, days
    );
    if let Some(cached) = read_cache(forecast_cache(), &cache_key, FORECAST_CACHE_TTL) {
        return Ok(cached);
    }

    let latitude = location.latitude.to_string();
    let longitude = location.longitude.to_string();
    let days = days.to_string();
    let response: ForecastResponse = client
        .get(OPEN_METEO_FORECAST_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            (
                "current",
                "temperature_2m,relative_humidity_2m,apparent_temperature,is_day,precipitation,weather_code,cloud_cover,wind_speed_10m,wind_direction_10m,wind_gusts_10m",
            ),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max,wind_gusts_10m_max",
            ),
            ("forecast_days", days.as_str()),
            ("timezone", "auto"),
            ("temperature_unit", "celsius"),
            ("wind_speed_unit", "kmh"),
            ("precipitation_unit", "mm"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    write_cache(forecast_cache(), cache_key, response.clone(), FORECAST_CACHE_TTL);
    Ok(response)
}

async fn get_weather_wttr(
    client: &reqwest::Client,
    location: &str,
    fallback_reason: &str,
) -> Result<String> {
    if location.is_empty() {
        bail!("wttr.in fallback requires a non-empty location");
    }
    let url = format!(
        "https://wttr.in/{}?format=%C+%t+%w+%l",
        urlencoding::encode(location)
    );
    let text = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let text = text.trim();
    if text.is_empty() {
        bail!("weather response was empty");
    }
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "wttr_in",
        "mode": "fallback",
        "fallback_reason": fallback_reason,
        "summary": format!("current weather(condition,temperature,wind,location): {text}"),
        "source": {
            "name": "wttr.in"
        }
    }))?)
}

fn format_location(location: &GeocodingResult) -> String {
    [
        Some(location.name.as_str()),
        location.admin1.as_deref(),
        location.country.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join("，")
}

fn normalize_location(value: &str) -> String {
    value.trim().to_lowercase().replace(char::is_whitespace, "")
}

fn format_temperature(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}°C"))
        .unwrap_or_else(|| "未知温度".to_string())
}

fn format_percent(value: Option<u64>) -> String {
    value
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_speed(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} km/h"))
        .unwrap_or_else(|| "未知风速".to_string())
}

fn format_precipitation(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} mm"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_micrograms(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} µg/m³"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_optional_number(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_count(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "未知".to_string())
}

fn format_meters(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} m"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_seconds(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} s"))
        .unwrap_or_else(|| "未知".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归(PR#31):读取只判过期不删,缓存无界增长;写入时应清扫。
    #[test]
    fn write_cache_sweeps_expired_entries() {
        let cache: Mutex<HashMap<String, CacheEntry<u32>>> = Mutex::new(HashMap::new());
        write_cache(&cache, "old".to_string(), 1, Duration::from_secs(3600));
        std::thread::sleep(Duration::from_millis(5));
        // 用 1ns TTL 触发清扫:5ms 前写入的 old 必然过期,new 本身刚插入。
        write_cache(&cache, "new".to_string(), 2, Duration::from_nanos(1));
        let cache = cache.lock().unwrap();
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("new"));
    }

    /// 空 location 必须在发起任何网络请求前直接报错——不允许 IP 自动定位。
    #[tokio::test]
    async fn empty_location_errors_without_network() {
        for args in [json!({}), json!({ "location": "  " }), json!({ "query_type": "air_quality" })] {
            let error = get_weather(args).await.expect_err("empty location must fail");
            assert!(
                error.to_string().contains("location is required"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn parses_args_with_defaults_and_clamps_days() {
        let request = WeatherRequest::from_args(&json!({
            "location": " Beijing ",
            "query_type": "air_quality",
            "days": 99,
            "start_date": "2024-01-01",
            "country_code": "cn"
        }));
        assert_eq!(request.location, "Beijing");
        assert_eq!(request.query_type, WeatherQueryType::AirQuality);
        assert_eq!(request.days, 7);
        assert_eq!(request.start_date.as_deref(), Some("2024-01-01"));
        assert_eq!(request.country_code.as_deref(), Some("CN"));

        let request = WeatherRequest::from_args(&json!({}));
        assert_eq!(request.location, "");
        assert_eq!(request.query_type, WeatherQueryType::Forecast);
        assert_eq!(request.days, 3);
        assert_eq!(request.country_code, None);

        let request = WeatherRequest::from_args(&json!({"query_type": "climate"}));
        assert_eq!(request.query_type, WeatherQueryType::Climate);
    }

    #[test]
    fn maps_weather_codes_to_chinese_labels() {
        assert_eq!(weather_code_label(0), "晴");
        assert_eq!(weather_code_label(3), "阴");
        assert_eq!(weather_code_label(53), "中等毛毛雨");
        assert_eq!(weather_code_label(65), "大雨");
        assert_eq!(weather_code_label(75), "大雪");
        assert_eq!(weather_code_label(95), "雷暴");
        assert_eq!(weather_code_label(12345), "未知天气");
    }

    #[test]
    fn maps_wind_direction_degrees() {
        assert_eq!(wind_direction_label(0.0), "北风");
        assert_eq!(wind_direction_label(45.0), "东北风");
        assert_eq!(wind_direction_label(90.0), "东风");
        assert_eq!(wind_direction_label(180.0), "南风");
        assert_eq!(wind_direction_label(270.0), "西风");
        assert_eq!(wind_direction_label(337.0), "西北风");
    }

    #[test]
    fn maps_air_quality_labels() {
        assert_eq!(european_aqi_label(20), "良好");
        assert_eq!(european_aqi_label(80), "差");
        assert_eq!(us_aqi_label(50), "良好");
        assert_eq!(us_aqi_label(151), "不健康");
    }

    #[test]
    fn selects_capital_and_population_for_ambiguous_location() {
        let small = GeocodingResult {
            name: "Beijing".to_string(),
            latitude: 35.2,
            longitude: 110.7,
            elevation: None,
            feature_code: Some("PPL".to_string()),
            country_code: Some("CN".to_string()),
            country: Some("中国".to_string()),
            admin1: Some("山西".to_string()),
            admin2: None,
            timezone: Some("Asia/Shanghai".to_string()),
            population: None,
        };
        let capital = GeocodingResult {
            name: "北京".to_string(),
            latitude: 39.9,
            longitude: 116.4,
            elevation: None,
            feature_code: Some("PPLC".to_string()),
            country_code: Some("CN".to_string()),
            country: Some("中国".to_string()),
            admin1: Some("北京市".to_string()),
            admin2: None,
            timezone: Some("Asia/Shanghai".to_string()),
            population: Some(18_960_744),
        };
        let locations = vec![small, capital];
        let selected = select_location("Beijing", &locations).unwrap();
        assert_eq!(selected.name, "北京");
    }

    #[test]
    fn expands_common_translated_location_aliases() {
        assert_eq!(
            geocoding_query_names("东京"),
            vec!["Tokyo".to_string(), "东京".to_string(), "東京".to_string()]
        );
        assert_eq!(
            geocoding_query_names("日本东京"),
            vec![
                "Tokyo".to_string(),
                "日本东京".to_string(),
                "東京".to_string()
            ]
        );
        assert_eq!(
            geocoding_query_names("纽约"),
            vec!["New York".to_string(), "纽约".to_string()]
        );
        assert_eq!(
            geocoding_query_names("Beijing"),
            vec!["Beijing".to_string()]
        );
    }

    #[test]
    fn selects_japanese_tokyo_for_chinese_tokyo_alias() {
        let china_tokyo = GeocodingResult {
            name: "东京".to_string(),
            latitude: 28.0,
            longitude: 119.4,
            elevation: None,
            feature_code: Some("PPL".to_string()),
            country_code: Some("CN".to_string()),
            country: Some("中国".to_string()),
            admin1: Some("浙江".to_string()),
            admin2: None,
            timezone: Some("Asia/Shanghai".to_string()),
            population: None,
        };
        let japan_tokyo = GeocodingResult {
            name: "東京".to_string(),
            latitude: 35.6895,
            longitude: 139.69171,
            elevation: Some(44.0),
            feature_code: Some("PPLC".to_string()),
            country_code: Some("JP".to_string()),
            country: Some("日本".to_string()),
            admin1: Some("东京都".to_string()),
            admin2: None,
            timezone: Some("Asia/Tokyo".to_string()),
            population: Some(9_733_276),
        };
        let locations = vec![china_tokyo, japan_tokyo];
        let selected = select_location("东京", &locations).unwrap();
        assert_eq!(selected.country_code.as_deref(), Some("JP"));
    }
}
